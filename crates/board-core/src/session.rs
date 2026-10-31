//! ボードを使った会議の進行に使う共有状態: タイマーと投票。
//!
//! 要素とは別のルートマップに入れるので、Undo の対象にはならない(誰かのタイマー操作を
//! 自分の Ctrl+Z で戻してしまわないように)。同期と保存は要素と同じ仕組みに乗る。
//!
//! - `session`: キー `timer` と `vote` に、それぞれ 1 つの値(`Any::Map`)を丸ごと入れる。
//!   値ごと置き換えるので、同時に操作しても混ざらず、最後に書いた人の状態になる。
//! - `votes`: キー `"{回}/{参加者}/{要素}"` に票数。各自が自分のキーだけ書くので衝突しない。

use std::collections::HashMap;
use std::sync::Arc;

use yrs::{Any, Map, Number, Out, Transact};

use crate::board::Board;
use crate::element::ElementId;
use crate::shape::ShapeKind;

const SESSION: &str = "session";
const VOTES: &str = "votes";
const ORIGIN_SESSION: &str = "session";

/// 1 回の投票で 1 人が使える票数の上限。
pub const MAX_VOTES: u32 = 99;

/// 全員に見えるタイマー。時刻はミリ秒の UNIX 時刻(各自の時計で数える)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timer {
    pub duration_ms: f64,
    /// 動いているときの終了時刻。
    pub ends_at_ms: f64,
    /// 一時停止中なら残り時間。
    pub paused_remaining_ms: Option<f64>,
}

impl Timer {
    /// 残り時間(ミリ秒、0 以上)。
    pub fn remaining_ms(&self, now_ms: f64) -> f64 {
        self.paused_remaining_ms
            .unwrap_or(self.ends_at_ms - now_ms)
            .max(0.0)
    }

    pub fn is_paused(&self) -> bool {
        self.paused_remaining_ms.is_some()
    }

    pub fn is_finished(&self, now_ms: f64) -> bool {
        !self.is_paused() && now_ms >= self.ends_at_ms
    }
}

/// 残り時間を `"03:05"` のように整形する(端数は切り上げ)。
pub fn format_remaining(ms: f64) -> String {
    let secs = (ms / 1000.0).ceil().max(0.0) as u64;
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// 進行中(または締め切った直後)の投票。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteSession {
    /// 回ごとの識別子。前の回の票と混ざらないようにキーに含める。
    pub round: String,
    /// 1 人あたりの票数。
    pub limit: u32,
    /// まだ投票を受け付けているか。締め切ると全員に結果が見える。
    pub open: bool,
}

/// 投票の集計。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoteTally {
    /// 要素ごとの合計(多い順、同数なら ID 順)。消えた要素の票は数えない。
    pub totals: Vec<(ElementId, u32)>,
    /// 自分が入れた票。
    pub mine: HashMap<ElementId, u32>,
    /// 自分が使った票数。
    pub used: u32,
    /// 1 票以上入れた人の数。
    pub voters: usize,
}

fn int(v: i64) -> Any {
    Any::Number(Number::Int(v))
}

fn float(v: f64) -> Any {
    Any::Number(Number::Float(v))
}

fn as_f64(a: Option<&Any>) -> Option<f64> {
    match a? {
        Any::Number(Number::Float(n)) => Some(*n),
        Any::Number(Number::Int(n)) => Some(*n as f64),
        _ => None,
    }
}

fn record(fields: Vec<(&str, Any)>) -> Any {
    let map: HashMap<String, Any> = fields
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    Any::Map(Arc::new(map))
}

fn vote_key(round: &str, user: &str, id: ElementId) -> String {
    format!("{round}/{user}/{}", crate::crdt::key_of(id))
}

/// 参加者 ID はキーの区切りに使う `/` を含めない。
fn valid_user(user: &str) -> bool {
    !user.is_empty() && user.len() <= 64 && !user.contains('/')
}

impl Board {
    fn session_get(&self, key: &str) -> Option<HashMap<String, Any>> {
        let doc = self.doc()?;
        let map = doc.get_or_insert_map(SESSION);
        let txn = doc.transact();
        match map.get(&txn, key) {
            Some(Out::Any(Any::Map(m))) => Some((*m).clone()),
            _ => None,
        }
    }

    fn session_set(&mut self, key: &str, value: Option<Any>) {
        let Some(doc) = self.doc() else { return };
        let map = doc.get_or_insert_map(SESSION);
        let mut txn = doc.transact_mut_with(ORIGIN_SESSION);
        match value {
            Some(v) => {
                map.insert(&mut txn, key, v);
            }
            None => {
                map.remove(&mut txn, key);
            }
        }
    }

    // ---- タイマー ----

    pub fn timer(&self) -> Option<Timer> {
        let m = self.session_get("timer")?;
        Some(Timer {
            duration_ms: as_f64(m.get("dur"))?,
            ends_at_ms: as_f64(m.get("end"))?,
            paused_remaining_ms: as_f64(m.get("paused")),
        })
    }

    fn set_timer(&mut self, t: Option<Timer>) {
        let value = t.map(|t| {
            let mut f = vec![("dur", float(t.duration_ms)), ("end", float(t.ends_at_ms))];
            if let Some(p) = t.paused_remaining_ms {
                f.push(("paused", float(p)));
            }
            record(f)
        });
        self.session_set("timer", value);
    }

    /// `duration_ms` のタイマーを今から始める(動いているものは置き換える)。
    pub fn start_timer(&mut self, duration_ms: f64, now_ms: f64) {
        let duration_ms = duration_ms.clamp(1000.0, 24.0 * 3600.0 * 1000.0);
        self.set_timer(Some(Timer {
            duration_ms,
            ends_at_ms: now_ms + duration_ms,
            paused_remaining_ms: None,
        }));
    }

    /// 一時停止と再開を切り替える。
    pub fn toggle_timer_pause(&mut self, now_ms: f64) {
        let Some(mut t) = self.timer() else { return };
        match t.paused_remaining_ms.take() {
            Some(rest) => t.ends_at_ms = now_ms + rest,
            None => t.paused_remaining_ms = Some(t.remaining_ms(now_ms)),
        }
        self.set_timer(Some(t));
    }

    /// 残り時間を増やす(終わっていたら今から数え直す)。
    pub fn extend_timer(&mut self, add_ms: f64, now_ms: f64) {
        let Some(mut t) = self.timer() else { return };
        match t.paused_remaining_ms.as_mut() {
            Some(rest) => *rest += add_ms,
            None => t.ends_at_ms = t.ends_at_ms.max(now_ms) + add_ms,
        }
        t.duration_ms += add_ms;
        self.set_timer(Some(t));
    }

    pub fn stop_timer(&mut self) {
        self.set_timer(None);
    }

    // ---- 投票 ----

    pub fn vote_session(&self) -> Option<VoteSession> {
        let m = self.session_get("vote")?;
        let round = match m.get("round") {
            Some(Any::String(s)) => s.to_string(),
            _ => return None,
        };
        Some(VoteSession {
            round,
            limit: as_f64(m.get("limit"))
                .unwrap_or(3.0)
                .clamp(1.0, MAX_VOTES as f64) as u32,
            open: matches!(m.get("open"), Some(Any::Bool(true))),
        })
    }

    fn set_vote_session(&mut self, v: &VoteSession) {
        let value = record(vec![
            ("round", Any::String(v.round.as_str().into())),
            ("limit", int(v.limit as i64)),
            ("open", Any::Bool(v.open)),
        ]);
        self.session_set("vote", Some(value));
    }

    /// 新しい投票を始める。前の回の票は消える。
    pub fn start_vote(&mut self, limit: u32) {
        self.clear_votes();
        let round = format!("{:x}", self.alloc_id().0);
        self.set_vote_session(&VoteSession {
            round,
            limit: limit.clamp(1, MAX_VOTES),
            open: true,
        });
    }

    /// 締め切る(結果は残る)。
    pub fn close_vote(&mut self) {
        if let Some(mut v) = self.vote_session()
            && v.open
        {
            v.open = false;
            self.set_vote_session(&v);
        }
    }

    /// 投票と結果を片付ける。
    pub fn clear_vote(&mut self) {
        self.clear_votes();
        self.session_set("vote", None);
    }

    fn clear_votes(&mut self) {
        let Some(doc) = self.doc() else { return };
        let votes = doc.get_or_insert_map(VOTES);
        let mut txn = doc.transact_mut_with(ORIGIN_SESSION);
        votes.clear(&mut txn);
    }

    /// 投票できる要素か(付箋だけ)。
    pub fn is_votable(&self, id: ElementId) -> bool {
        self.get(id)
            .and_then(|e| e.as_shape())
            .is_some_and(|s| s.kind == ShapeKind::Sticky)
    }

    /// 1 票入れる。受付中でない、票が残っていない、付箋でないときは false。
    pub fn cast_vote(&mut self, user: &str, id: ElementId) -> bool {
        let Some(session) = self.vote_session().filter(|v| v.open) else {
            return false;
        };
        if !valid_user(user) || !self.is_votable(id) {
            return false;
        }
        let tally = self.vote_tally(user);
        if tally.used >= session.limit {
            return false;
        }
        let n = tally.mine.get(&id).copied().unwrap_or(0) + 1;
        self.write_vote(&vote_key(&session.round, user, id), n);
        true
    }

    /// 自分の票を 1 つ戻す。
    pub fn retract_vote(&mut self, user: &str, id: ElementId) -> bool {
        let Some(session) = self.vote_session().filter(|v| v.open) else {
            return false;
        };
        let n = self.vote_tally(user).mine.get(&id).copied().unwrap_or(0);
        if n == 0 || !valid_user(user) {
            return false;
        }
        self.write_vote(&vote_key(&session.round, user, id), n - 1);
        true
    }

    fn write_vote(&mut self, key: &str, n: u32) {
        let Some(doc) = self.doc() else { return };
        let votes = doc.get_or_insert_map(VOTES);
        let mut txn = doc.transact_mut_with(ORIGIN_SESSION);
        if n == 0 {
            votes.remove(&mut txn, key);
        } else {
            votes.insert(&mut txn, key, int(n as i64));
        }
    }

    /// 今の回の集計。`user` は自分の参加者 ID。
    pub fn vote_tally(&self, user: &str) -> VoteTally {
        let mut tally = VoteTally::default();
        let (Some(session), Some(doc)) = (self.vote_session(), self.doc()) else {
            return tally;
        };
        let votes = doc.get_or_insert_map(VOTES);
        let txn = doc.transact();
        let mut totals: HashMap<ElementId, u32> = HashMap::new();
        let mut voters = std::collections::HashSet::new();
        for (key, value) in votes.iter(&txn) {
            let mut parts = key.splitn(3, '/');
            let (Some(round), Some(who), Some(el)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Some(id) = crate::crdt::id_of(el) else {
                continue;
            };
            if round != session.round || self.get(id).is_none() {
                continue;
            }
            let n = match value {
                Out::Any(a) => as_f64(Some(&a)).unwrap_or(0.0).max(0.0) as u32,
                _ => 0,
            };
            if n == 0 {
                continue;
            }
            *totals.entry(id).or_default() += n;
            voters.insert(who.to_string());
            if who == user {
                *tally.mine.entry(id).or_default() += n;
                tally.used += n;
            }
        }
        tally.totals = totals.into_iter().collect();
        tally
            .totals
            .sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        tally.voters = voters.len();
        tally
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgba8;
    use crate::shape::Shape;
    use glam::Vec2;

    fn exchange(a: &mut Board, b: &mut Board) {
        for u in a.take_outgoing() {
            b.apply_remote(&u).unwrap();
        }
        for u in b.take_outgoing() {
            a.apply_remote(&u).unwrap();
        }
    }

    fn sticky(b: &mut Board, x: f32) -> ElementId {
        let id = b.alloc_id();
        b.push(Shape::sticky(id, Vec2::new(x, 0.0), Rgba8::WHITE));
        b.end_gesture();
        id
    }

    #[test]
    fn timer_syncs_pauses_and_extends() {
        let mut a = Board::with_client_id(1);
        let mut b = Board::with_client_id(2);
        a.start_timer(60_000.0, 1_000.0);
        exchange(&mut a, &mut b);
        let t = b.timer().unwrap();
        assert_eq!(t.remaining_ms(31_000.0), 30_000.0);
        assert_eq!(format_remaining(t.remaining_ms(31_000.0)), "00:30");
        assert!(!t.is_finished(60_999.0));
        assert!(t.is_finished(61_000.0));

        b.toggle_timer_pause(31_000.0);
        exchange(&mut a, &mut b);
        let t = a.timer().unwrap();
        assert!(t.is_paused());
        assert_eq!(t.remaining_ms(99_000.0), 30_000.0);

        a.extend_timer(60_000.0, 99_000.0);
        a.toggle_timer_pause(100_000.0);
        exchange(&mut a, &mut b);
        assert_eq!(b.timer().unwrap().remaining_ms(100_000.0), 90_000.0);

        // タイマー操作は Undo の対象外。
        assert!(!a.can_undo());
        b.stop_timer();
        exchange(&mut a, &mut b);
        assert!(a.timer().is_none());
    }

    #[test]
    fn format() {
        assert_eq!(format_remaining(0.0), "00:00");
        assert_eq!(format_remaining(1.0), "00:01");
        assert_eq!(format_remaining(185_000.0), "03:05");
        assert_eq!(format_remaining(-5.0), "00:00");
    }

    #[test]
    fn voting_respects_limits_and_rounds() {
        let mut a = Board::with_client_id(1);
        let mut b = Board::with_client_id(2);
        let s1 = sticky(&mut a, 0.0);
        let s2 = sticky(&mut a, 300.0);
        let rect = a.alloc_id();
        a.push(Shape::rect(rect, Vec2::ZERO, Vec2::ONE, Rgba8::WHITE));
        exchange(&mut a, &mut b);

        // 始まる前は入れられない。
        assert!(!a.cast_vote("ua", s1));
        a.start_vote(2);
        exchange(&mut a, &mut b);
        assert_eq!(b.vote_session().unwrap().limit, 2);

        assert!(a.cast_vote("ua", s1));
        assert!(a.cast_vote("ua", s1));
        assert!(!a.cast_vote("ua", s2), "上限を超えた");
        assert!(!b.cast_vote("ub", rect), "付箋以外");
        assert!(b.cast_vote("ub", s2));
        exchange(&mut a, &mut b);

        let t = b.vote_tally("ub");
        assert_eq!(t.totals, vec![(s1, 2), (s2, 1)]);
        assert_eq!(t.used, 1);
        assert_eq!(t.voters, 2);
        assert_eq!(a.vote_tally("ua").mine.get(&s1), Some(&2));

        assert!(a.retract_vote("ua", s1));
        assert!(!a.retract_vote("ua", s2));
        b.close_vote();
        exchange(&mut a, &mut b);
        assert!(!a.cast_vote("ua", s2), "締め切り後");
        assert_eq!(a.vote_tally("ua").totals, vec![(s1, 1), (s2, 1)]);

        // 消えた付箋の票は数えない。
        a.remove(&[s2]);
        assert_eq!(a.vote_tally("ua").totals, vec![(s1, 1)]);

        // 次の回は前の票を引き継がない。
        b.start_vote(3);
        exchange(&mut a, &mut b);
        assert!(a.vote_tally("ua").totals.is_empty());
        a.clear_vote();
        exchange(&mut a, &mut b);
        assert!(b.vote_session().is_none());
    }

    #[test]
    fn local_board_ignores_session() {
        let mut b = Board::demo(3);
        b.start_timer(1000.0, 0.0);
        b.start_vote(3);
        assert!(b.timer().is_none());
        assert!(b.vote_session().is_none());
    }
}
