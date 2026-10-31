//! ワークショップの進行に使う共有状態: 匿名モードとブレイクアウト(小部屋)。
//!
//! タイマーや投票と同じく要素とは別のルートマップに入れ、Undo の対象にしない。
//!
//! - `session` のキー `anon`: 匿名モード。全員の画面で他の人のカーソルと名前を隠す。
//! - `session` のキー `breakout`: 小部屋の回(`round`)と部屋の一覧(名前とフレーム)。
//! - `breakout` マップ: キー `"{回}/{参加者}"` に、その人がいる部屋の番号と表示名。
//!   各自が自分のキーだけ書くので衝突しない。

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec2;
use yrs::{Any, Map, Number, Out, Transact};

use crate::board::Board;
use crate::element::{Element, ElementId};
use crate::shape::Shape;

const SESSION: &str = "session";
const MEMBERS: &str = "breakout";
const ORIGIN: &str = "session";

/// 部屋の数の上限。
pub const MAX_ROOMS: usize = 12;
/// 部屋(フレーム)の大きさ。
pub const ROOM_SIZE: Vec2 = Vec2::new(1400.0, 900.0);
const ROOM_GAP: f32 = 160.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    pub name: String,
    pub frame: ElementId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakout {
    pub round: String,
    pub rooms: Vec<Room>,
}

/// 部屋にいる参加者。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub user: String,
    pub name: String,
    pub room: usize,
}

fn valid_user(user: &str) -> bool {
    !user.is_empty() && user.len() <= 64 && !user.contains('/')
}

/// 文字列の簡単なハッシュ(同時に入った人を部屋に散らすため)。
fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

impl Board {
    fn ws_get(&self, key: &str) -> Option<Any> {
        let doc = self.doc()?;
        let map = doc.get_or_insert_map(SESSION);
        let txn = doc.transact();
        match map.get(&txn, key) {
            Some(Out::Any(a)) => Some(a),
            _ => None,
        }
    }

    fn ws_set(&mut self, key: &str, value: Option<Any>) {
        let Some(doc) = self.doc() else { return };
        let map = doc.get_or_insert_map(SESSION);
        let mut txn = doc.transact_mut_with(ORIGIN);
        match value {
            Some(v) => {
                map.insert(&mut txn, key, v);
            }
            None => {
                map.remove(&mut txn, key);
            }
        }
    }

    // ---- 匿名モード ----

    pub fn anonymous(&self) -> bool {
        matches!(self.ws_get("anon"), Some(Any::Bool(true)))
    }

    pub fn set_anonymous(&mut self, on: bool) {
        self.ws_set("anon", on.then_some(Any::Bool(true)));
    }

    // ---- ブレイクアウト ----

    pub fn breakout(&self) -> Option<Breakout> {
        let Some(Any::Map(m)) = self.ws_get("breakout") else {
            return None;
        };
        let round = match m.get("round") {
            Some(Any::String(s)) => s.to_string(),
            _ => return None,
        };
        let rooms = match m.get("rooms") {
            Some(Any::Array(a)) => a
                .iter()
                .filter_map(|r| match r {
                    Any::Map(r) => {
                        let name = match r.get("name") {
                            Some(Any::String(s)) => s.to_string(),
                            _ => String::new(),
                        };
                        let frame = match r.get("frame") {
                            Some(Any::String(s)) => crate::crdt::id_of(s)?,
                            _ => return None,
                        };
                        Some(Room { name, frame })
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        (!rooms.is_empty()).then_some(Breakout { round, rooms })
    }

    /// `n` 個の部屋(フレーム)を `origin`(左上)から並べて作り、小部屋を始める。
    /// 前の回の割り当ては消える。作ったフレームの ID を返す。
    pub fn start_breakout(&mut self, n: usize, origin: Vec2) -> Vec<ElementId> {
        let n = n.clamp(1, MAX_ROOMS);
        self.end_breakout();
        let cols = if n <= 3 { n } else { n.div_ceil(2) };
        let mut rooms = Vec::new();
        let mut ids = Vec::new();
        for i in 0..n {
            let id = self.alloc_id();
            let pos = origin
                + Vec2::new(
                    (i % cols) as f32 * (ROOM_SIZE.x + ROOM_GAP),
                    (i / cols) as f32 * (ROOM_SIZE.y + ROOM_GAP),
                );
            let name = format!("部屋 {}", i + 1);
            let mut f = Shape::frame(id, pos, ROOM_SIZE);
            f.text = name.clone();
            self.push(f);
            ids.push(id);
            let room: HashMap<String, Any> = [
                ("name".to_string(), Any::String(name.into())),
                (
                    "frame".to_string(),
                    Any::String(crate::crdt::key_of(id).into()),
                ),
            ]
            .into_iter()
            .collect();
            rooms.push(Any::Map(Arc::new(room)));
        }
        let round = format!("{:x}", self.alloc_id().0);
        let value: HashMap<String, Any> = [
            ("round".to_string(), Any::String(round.into())),
            ("rooms".to_string(), Any::Array(rooms.into())),
        ]
        .into_iter()
        .collect();
        self.ws_set("breakout", Some(Any::Map(Arc::new(value))));
        ids
    }

    /// 小部屋を終える(部屋のフレームと中身はボードに残る)。
    pub fn end_breakout(&mut self) {
        self.ws_set("breakout", None);
        let Some(doc) = self.doc() else { return };
        let map = doc.get_or_insert_map(MEMBERS);
        let mut txn = doc.transact_mut_with(ORIGIN);
        map.clear(&mut txn);
    }

    /// 今の回で部屋にいる人(部屋の番号順、同じ部屋なら名前順)。
    pub fn breakout_members(&self) -> Vec<Member> {
        let Some(b) = self.breakout() else {
            return Vec::new();
        };
        let Some(doc) = self.doc() else {
            return Vec::new();
        };
        let map = doc.get_or_insert_map(MEMBERS);
        let txn = doc.transact();
        let prefix = format!("{}/", b.round);
        let mut out: Vec<Member> = map
            .iter(&txn)
            .filter_map(|(k, v)| {
                let user = k.strip_prefix(&prefix)?;
                let Out::Any(Any::Map(m)) = v else {
                    return None;
                };
                let room = match m.get("room") {
                    Some(Any::Number(Number::Int(n))) => *n as usize,
                    Some(Any::Number(Number::Float(n))) => *n as usize,
                    _ => return None,
                };
                let name = match m.get("name") {
                    Some(Any::String(s)) => s.to_string(),
                    _ => String::new(),
                };
                (room < b.rooms.len()).then(|| Member {
                    user: user.to_string(),
                    name,
                    room,
                })
            })
            .collect();
        out.sort_by(|a, b| (a.room, &a.name, &a.user).cmp(&(b.room, &b.name, &b.user)));
        out
    }

    /// 自分がいる部屋の番号。
    pub fn my_room(&self, user: &str) -> Option<usize> {
        self.breakout_members()
            .into_iter()
            .find(|m| m.user == user)
            .map(|m| m.room)
    }

    /// 部屋に入る(移る)。
    pub fn join_room(&mut self, user: &str, name: &str, room: usize) -> bool {
        let Some(b) = self.breakout() else {
            return false;
        };
        if room >= b.rooms.len() || !valid_user(user) {
            return false;
        }
        let Some(doc) = self.doc() else { return false };
        let map = doc.get_or_insert_map(MEMBERS);
        let mut txn = doc.transact_mut_with(ORIGIN);
        let value: HashMap<String, Any> = [
            ("room".to_string(), Any::Number(Number::Int(room as i64))),
            ("name".to_string(), Any::String(name.into())),
        ]
        .into_iter()
        .collect();
        map.insert(
            &mut txn,
            format!("{}/{user}", b.round),
            Any::Map(Arc::new(value)),
        );
        true
    }

    /// 自動で入る部屋: いちばん人の少ない部屋(同数なら参加者ごとにばらける)。
    pub fn suggest_room(&self, user: &str) -> Option<usize> {
        let b = self.breakout()?;
        let mut counts = vec![0usize; b.rooms.len()];
        for m in self.breakout_members() {
            if m.user != user {
                counts[m.room] += 1;
            }
        }
        let min = *counts.iter().min()?;
        let candidates: Vec<usize> = (0..counts.len()).filter(|&i| counts[i] == min).collect();
        Some(candidates[(hash(user) % candidates.len() as u64) as usize])
    }

    /// 同時に入って偏ったときの移り先。自分の部屋がいちばん少ない部屋より 2 人以上多く、
    /// その部屋で自分が移る番(全員が同じ計算をするので、1 人だけが動く)なら移る部屋を返す。
    pub fn rebalance_room(&self, user: &str) -> Option<usize> {
        let b = self.breakout()?;
        let members = self.breakout_members();
        let mine = members.iter().find(|m| m.user == user)?.room;
        let mut counts = vec![0usize; b.rooms.len()];
        for m in &members {
            counts[m.room] += 1;
        }
        let min = *counts.iter().min()?;
        if counts[mine] < min + 2 {
            return None;
        }
        let mover = members
            .iter()
            .filter(|m| m.room == mine)
            .max_by_key(|m| (hash(&m.user), m.user.as_str()))?;
        if mover.user != user {
            return None;
        }
        counts.iter().position(|&c| c == min)
    }

    /// 部屋のフレームの矩形。
    pub fn room_bounds(&self, room: usize) -> Option<crate::geom::Rect> {
        let b = self.breakout()?;
        self.get(b.rooms.get(room)?.frame).map(Element::bounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_toggle_syncs() {
        let mut a = Board::new();
        assert!(!a.anonymous());
        a.set_anonymous(true);
        let mut b = Board::new();
        for u in a.take_outgoing() {
            b.apply_remote(&u).unwrap();
        }
        assert!(b.anonymous());
        b.set_anonymous(false);
        assert!(!b.anonymous());
    }

    #[test]
    fn breakout_rooms_and_members() {
        let mut b = Board::new();
        let frames = b.start_breakout(3, Vec2::ZERO);
        assert_eq!(frames.len(), 3);
        let br = b.breakout().unwrap();
        assert_eq!(br.rooms.len(), 3);
        assert_eq!(br.rooms[1].name, "部屋 2");
        assert_eq!(br.rooms[2].frame, frames[2]);

        // 人の少ない部屋に順に入る。
        let mut used = Vec::new();
        for (i, u) in ["u1", "u2", "u3"].iter().enumerate() {
            let r = b.suggest_room(u).unwrap();
            assert!(!used.contains(&r), "{i}");
            used.push(r);
            assert!(b.join_room(u, u, r));
        }
        assert_eq!(b.breakout_members().len(), 3);
        // 移る。
        assert!(b.join_room("u1", "u1", 2));
        assert_eq!(b.my_room("u1"), Some(2));
        assert!(!b.join_room("u1", "u1", 9));
        assert!(!b.join_room("a/b", "x", 0));
        assert!(b.room_bounds(0).is_some());

        // 偏ったら 1 度に 1 人だけ移り、ならされる。
        b.join_room("u2", "u2", 2);
        b.join_room("u3", "u3", 2);
        for _ in 0..3 {
            let movers: Vec<_> = ["u1", "u2", "u3"]
                .iter()
                .filter_map(|u| b.rebalance_room(u).map(|r| (*u, r)))
                .collect();
            assert!(movers.len() <= 1, "{movers:?}");
            let Some(&(u, r)) = movers.first() else { break };
            b.join_room(u, u, r);
        }
        let mut rooms: Vec<usize> = b.breakout_members().iter().map(|m| m.room).collect();
        rooms.sort_unstable();
        assert_eq!(rooms, vec![0, 1, 2]);

        // 新しい回では前の割り当てを使わない。
        b.start_breakout(2, Vec2::new(0.0, 5000.0));
        assert!(b.breakout_members().is_empty());
        b.end_breakout();
        assert!(b.breakout().is_none());
        assert!(b.my_room("u1").is_none());
        // フレームは残る。
        assert!(b.get(frames[0]).is_some());
    }

    #[test]
    fn breakout_syncs_between_boards() {
        let mut a = Board::new();
        a.start_breakout(2, Vec2::ZERO);
        let mut b = Board::new();
        for u in a.take_outgoing() {
            b.apply_remote(&u).unwrap();
        }
        assert!(b.join_room("bob", "Bob", 1));
        for u in b.take_outgoing() {
            a.apply_remote(&u).unwrap();
        }
        assert_eq!(
            a.breakout_members(),
            vec![Member {
                user: "bob".into(),
                name: "Bob".into(),
                room: 1
            }]
        );
    }
}
