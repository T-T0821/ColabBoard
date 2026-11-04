//! 会議の記録と進行の共有状態: 文字起こし・議題(AI ファシリテーター)・リアルタイム議事録。
//!
//! タイマーや投票([`crate::session`])と同じく、要素とは別のルートに入れるので Undo の対象にならない。
//! 同期と保存は要素と同じ仕組みに乗る(途中から参加した人にも届き、ボードと一緒に残る)。
//!
//! - `transcript`: 文字起こしの 1 発言ごとに 1 キー(`"{時刻}-{参加者}-{連番}"`)。各自が自分の発言だけ
//!   足すので衝突しない。値は `{at, who, name, text}`。
//! - `session` の `meeting`: 議題と進み具合(JSON 文字列。値ごと置き換え、最後に書いた人の状態になる)。
//! - `session` の `live_minutes`: 自動更新の設定と、最後に AI が作った議事録(JSON 文字列)。
//! - `session` の `advice`: AI ファシリテーターの最新の助言(JSON 文字列)。

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use yrs::{Any, Map, Number, Out, Transact};

use crate::board::Board;

const SESSION: &str = "session";
const TRANSCRIPT: &str = "transcript";
const ORIGIN: &str = "session";

/// 1 発言の文字数の上限(超えた分は切り詰める)。
pub const MAX_LINE_CHARS: usize = 500;
/// 1 枚のボードに残す発言の上限(超えたら足さない。消してから続ける)。
pub const MAX_LINES: usize = 5000;
/// 議題の数の上限。
pub const MAX_AGENDA: usize = 12;
/// 議題名の文字数の上限。
pub const MAX_AGENDA_TITLE: usize = 80;

/// 文字起こしの 1 発言。
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptLine {
    /// 話し終えた時刻(ミリ秒の UNIX 時刻、話した人の時計)。
    pub at_ms: f64,
    /// 話した人の参加者 ID。
    pub user: String,
    /// 話した人の表示名。
    pub name: String,
    pub text: String,
}

/// 議題 1 つ。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgendaItem {
    pub title: String,
    /// 割り当て時間(分)。
    pub minutes: u32,
    /// この議題で目指すこと(任意)。
    #[serde(default)]
    pub goal: String,
}

/// 会議の進み具合。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meeting {
    /// 会議の目的。
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub agenda: Vec<AgendaItem>,
    /// 今の議題(始まる前・終わった後は `None`)。
    #[serde(default)]
    pub current: Option<usize>,
    /// 会議を始めた時刻(ミリ秒の UNIX 時刻。始まる前は 0)。
    #[serde(default)]
    pub started_at_ms: f64,
    /// 今の議題に入った時刻。
    #[serde(default)]
    pub item_started_at_ms: f64,
    /// 議題ごとに実際にかかった時間(ミリ秒。議題の順)。
    #[serde(default)]
    pub spent_ms: Vec<f64>,
    #[serde(default)]
    pub ended: bool,
    /// 会議を始めた人の参加者 ID(議題の時間切れのときの自動の助言はこの人の画面から頼む)。
    #[serde(default)]
    pub host: String,
}

impl Meeting {
    pub fn is_running(&self) -> bool {
        self.started_at_ms > 0.0 && !self.ended
    }

    /// 予定の合計(分)。
    pub fn planned_minutes(&self) -> u32 {
        self.agenda.iter().map(|a| a.minutes).sum()
    }

    pub fn current_item(&self) -> Option<&AgendaItem> {
        self.agenda.get(self.current?)
    }

    /// 今の議題で使った時間(ミリ秒)。
    pub fn item_elapsed_ms(&self, now_ms: f64) -> f64 {
        if self.current.is_some() && self.is_running() {
            (now_ms - self.item_started_at_ms).max(0.0)
        } else {
            0.0
        }
    }

    /// 議題を整える(空の題を外し、長さと数をそろえる)。
    pub fn normalize(&mut self) {
        for a in &mut self.agenda {
            a.title = clip(
                a.title.split_whitespace().collect::<Vec<_>>().join(" "),
                MAX_AGENDA_TITLE,
            );
            a.goal = clip(a.goal.split_whitespace().collect::<Vec<_>>().join(" "), 200);
            a.minutes = a.minutes.clamp(1, 240);
        }
        self.agenda.retain(|a| !a.title.is_empty());
        self.agenda.truncate(MAX_AGENDA);
        self.goal = clip(self.goal.trim().to_string(), 500);
        if self.current.is_some_and(|i| i >= self.agenda.len()) {
            self.current = None;
        }
        self.spent_ms.resize(self.agenda.len(), 0.0);
    }

    /// 会議を始める(最初の議題から)。
    pub fn start(&mut self, now_ms: f64) {
        self.started_at_ms = now_ms;
        self.item_started_at_ms = now_ms;
        self.ended = false;
        self.spent_ms = vec![0.0; self.agenda.len()];
        self.current = if self.agenda.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    /// 次の議題へ進む。最後の議題の次は会議の終わり(`current` が `None`)。
    pub fn advance(&mut self, now_ms: f64) {
        let Some(i) = self.current else { return };
        let used = self.item_elapsed_ms(now_ms);
        self.spent_ms.resize(self.agenda.len(), 0.0);
        if let Some(s) = self.spent_ms.get_mut(i) {
            *s += used;
        }
        self.item_started_at_ms = now_ms;
        self.current = (i + 1 < self.agenda.len()).then_some(i + 1);
    }

    /// 会議を終える。
    pub fn finish(&mut self, now_ms: f64) {
        if self.current.is_some() {
            self.spent_ms.resize(self.agenda.len(), 0.0);
            if let Some(s) = self.current.and_then(|i| self.spent_ms.get_mut(i)) {
                *s += (now_ms - self.item_started_at_ms).max(0.0);
            }
        }
        self.current = None;
        self.ended = true;
    }
}

/// リアルタイム議事録の状態。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveMinutes {
    /// 自動で更新するか。
    #[serde(default)]
    pub auto: bool,
    /// 自動更新を受け持つ人の参加者 ID(その人の画面から AI を呼ぶ)。
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub owner_name: String,
    /// 最後に作った議事録(サーバーの `minutes` の構造そのまま)。
    #[serde(default)]
    pub minutes: Option<serde_json::Value>,
    /// 最後に作った議事録のマークダウン(ボードに置くとき使う)。
    #[serde(default)]
    pub markdown: String,
    /// 作った時刻(ミリ秒の UNIX 時刻)。
    #[serde(default)]
    pub updated_at_ms: f64,
    /// 作ったときの発言の数(増えていなければ作り直さない)。
    #[serde(default)]
    pub lines: usize,
}

/// AI ファシリテーターの助言。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Advice {
    /// サーバーの `facilitate`(check)の応答そのまま。
    pub body: serde_json::Value,
    pub at_ms: f64,
    /// どの議題のときの助言か。
    #[serde(default)]
    pub item: Option<usize>,
}

fn clip(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        s.chars().take(max).collect()
    }
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

fn as_str(a: Option<&Any>) -> String {
    match a {
        Some(Any::String(s)) => s.to_string(),
        _ => String::new(),
    }
}

impl Board {
    fn meeting_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        let doc = self.doc()?;
        let map = doc.get_or_insert_map(SESSION);
        let txn = doc.transact();
        match map.get(&txn, key) {
            Some(Out::Any(Any::String(s))) => serde_json::from_str(&s).ok(),
            _ => None,
        }
    }

    fn set_meeting_json<T: Serialize>(&mut self, key: &str, value: Option<&T>) {
        let Some(doc) = self.doc() else { return };
        let map = doc.get_or_insert_map(SESSION);
        let mut txn = doc.transact_mut_with(ORIGIN);
        match value.and_then(|v| serde_json::to_string(v).ok()) {
            Some(s) => {
                map.insert(&mut txn, key, Any::String(s.into()));
            }
            None => {
                map.remove(&mut txn, key);
            }
        }
    }

    // ---- 議題と進行 ----

    pub fn meeting(&self) -> Option<Meeting> {
        self.meeting_json("meeting")
    }

    /// 会議の状態を書く(`None` で片付ける)。
    pub fn set_meeting(&mut self, m: Option<&Meeting>) {
        let m = m.map(|m| {
            let mut m = m.clone();
            m.normalize();
            m
        });
        self.set_meeting_json("meeting", m.as_ref());
    }

    // ---- リアルタイム議事録 ----

    pub fn live_minutes(&self) -> LiveMinutes {
        self.meeting_json("live_minutes").unwrap_or_default()
    }

    pub fn set_live_minutes(&mut self, l: &LiveMinutes) {
        self.set_meeting_json("live_minutes", Some(l));
    }

    // ---- AI ファシリテーターの助言 ----

    pub fn advice(&self) -> Option<Advice> {
        self.meeting_json("advice")
    }

    pub fn set_advice(&mut self, a: Option<&Advice>) {
        self.set_meeting_json("advice", a);
    }

    // ---- 文字起こし ----

    /// 発言の数(変化の目印に使う。安い)。
    pub fn transcript_len(&self) -> usize {
        let Some(doc) = self.doc() else { return 0 };
        let map = doc.get_or_insert_map(TRANSCRIPT);
        let txn = doc.transact();
        map.len(&txn) as usize
    }

    /// 発言を 1 つ足す。空、上限を超えた、共同編集でないときは false。
    pub fn add_transcript(&mut self, user: &str, name: &str, text: &str, at_ms: f64) -> bool {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() || user.trim().is_empty() {
            return false;
        }
        let len = self.transcript_len();
        let Some(doc) = self.doc() else { return false };
        if len >= MAX_LINES {
            return false;
        }
        let map = doc.get_or_insert_map(TRANSCRIPT);
        let mut txn = doc.transact_mut_with(ORIGIN);
        let user: String = user.chars().take(64).collect();
        let base = format!("{:013}-{}", at_ms.max(0.0) as u64, user);
        let mut key = format!("{base}-0");
        let mut n = 0;
        while map.get(&txn, &key).is_some() {
            n += 1;
            key = format!("{base}-{n}");
        }
        let fields: HashMap<String, Any> = [
            ("at", float(at_ms)),
            ("who", Any::String(user.as_str().into())),
            (
                "name",
                Any::String(clip(name.trim().to_string(), 40).as_str().into()),
            ),
            (
                "text",
                Any::String(clip(text, MAX_LINE_CHARS).as_str().into()),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        map.insert(&mut txn, key, Any::Map(Arc::new(fields)));
        true
    }

    /// 発言を古い順に。
    pub fn transcript(&self) -> Vec<TranscriptLine> {
        let Some(doc) = self.doc() else {
            return Vec::new();
        };
        let map = doc.get_or_insert_map(TRANSCRIPT);
        let txn = doc.transact();
        let mut out: Vec<(String, TranscriptLine)> = map
            .iter(&txn)
            .filter_map(|(k, v)| {
                let Out::Any(Any::Map(m)) = v else {
                    return None;
                };
                let text = as_str(m.get("text"));
                if text.is_empty() {
                    return None;
                }
                Some((
                    k.to_string(),
                    TranscriptLine {
                        at_ms: as_f64(m.get("at")).unwrap_or(0.0),
                        user: as_str(m.get("who")),
                        name: as_str(m.get("name")),
                        text,
                    },
                ))
            })
            .collect();
        out.sort_by(|a, b| a.1.at_ms.total_cmp(&b.1.at_ms).then_with(|| a.0.cmp(&b.0)));
        out.into_iter().map(|(_, l)| l).collect()
    }

    /// 文字起こしを全部消す。
    pub fn clear_transcript(&mut self) {
        let Some(doc) = self.doc() else { return };
        let map = doc.get_or_insert_map(TRANSCRIPT);
        let mut txn = doc.transact_mut_with(ORIGIN);
        map.clear(&mut txn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(a: &mut Board, b: &mut Board) {
        for u in a.take_outgoing() {
            b.apply_remote(&u).unwrap();
        }
        for u in b.take_outgoing() {
            a.apply_remote(&u).unwrap();
        }
    }

    #[test]
    fn transcript_merges_in_time_order() {
        let mut a = Board::with_client_id(1);
        let mut b = Board::with_client_id(2);
        assert!(a.add_transcript("ua", "佐藤", "  では始めます ", 1_000.0));
        assert!(b.add_transcript("ub", "鈴木", "よろしくお願いします", 2_000.0));
        assert!(a.add_transcript("ua", "佐藤", "最初の議題は", 2_000.0));
        assert!(a.add_transcript("ua", "佐藤", "同じ時刻でも消えない", 2_000.0));
        assert!(!a.add_transcript("ua", "佐藤", "   ", 3_000.0), "空");
        exchange(&mut a, &mut b);
        let t = b.transcript();
        assert_eq!(t.len(), 4);
        assert_eq!(t[0].text, "では始めます");
        assert_eq!(t[0].name, "佐藤");
        assert_eq!(a.transcript(), t, "どちらでも同じ順");
        assert_eq!(b.transcript_len(), 4);
        // 文字起こしは Undo の対象外。
        assert!(!a.can_undo());
        b.clear_transcript();
        exchange(&mut a, &mut b);
        assert!(a.transcript().is_empty());
    }

    #[test]
    fn long_lines_are_clipped() {
        let mut a = Board::with_client_id(1);
        let long = "あ".repeat(MAX_LINE_CHARS + 50);
        assert!(a.add_transcript("ua", "佐藤", &long, 1.0));
        assert_eq!(a.transcript()[0].text.chars().count(), MAX_LINE_CHARS);
    }

    #[test]
    fn meeting_advances_and_records_time() {
        let mut a = Board::with_client_id(1);
        let mut b = Board::with_client_id(2);
        let mut m = Meeting {
            goal: "来期の方針を決める".into(),
            agenda: vec![
                AgendaItem {
                    title: " 振り返り ".into(),
                    minutes: 10,
                    goal: String::new(),
                },
                AgendaItem {
                    title: "".into(),
                    minutes: 5,
                    goal: String::new(),
                },
                AgendaItem {
                    title: "方針".into(),
                    minutes: 0,
                    goal: "1 つに絞る".into(),
                },
            ],
            ..Default::default()
        };
        m.normalize();
        assert_eq!(m.agenda.len(), 2, "空の題は外す");
        assert_eq!(m.agenda[0].title, "振り返り");
        assert_eq!(m.agenda[1].minutes, 1, "0 分は 1 分に");
        assert_eq!(m.planned_minutes(), 11);

        m.start(1_000.0);
        assert!(m.is_running());
        assert_eq!(m.current, Some(0));
        a.set_meeting(Some(&m));
        exchange(&mut a, &mut b);
        let mut m = b.meeting().unwrap();
        assert_eq!(m.item_elapsed_ms(61_000.0), 60_000.0);
        m.advance(601_000.0);
        assert_eq!(m.current, Some(1));
        assert_eq!(m.spent_ms[0], 600_000.0);
        m.advance(661_000.0);
        assert_eq!(m.current, None, "最後の次");
        m.finish(700_000.0);
        assert!(m.ended && !m.is_running());
        assert_eq!(m.spent_ms, vec![600_000.0, 60_000.0]);
        b.set_meeting(Some(&m));
        exchange(&mut a, &mut b);
        assert_eq!(a.meeting().unwrap(), m);
        assert!(!a.can_undo());
        a.set_meeting(None);
        exchange(&mut a, &mut b);
        assert!(b.meeting().is_none());
    }

    #[test]
    fn live_minutes_and_advice_sync() {
        let mut a = Board::with_client_id(1);
        let mut b = Board::with_client_id(2);
        assert_eq!(a.live_minutes(), LiveMinutes::default());
        a.set_live_minutes(&LiveMinutes {
            auto: true,
            owner: "ua".into(),
            owner_name: "佐藤".into(),
            markdown: "# 議事録".into(),
            lines: 3,
            ..Default::default()
        });
        a.set_advice(Some(&Advice {
            body: serde_json::json!({"suggestion": "結論を確認しましょう"}),
            at_ms: 5.0,
            item: Some(0),
        }));
        exchange(&mut a, &mut b);
        assert!(b.live_minutes().auto);
        assert_eq!(b.live_minutes().owner, "ua");
        assert_eq!(
            b.advice().unwrap().body["suggestion"],
            "結論を確認しましょう"
        );
    }

    #[test]
    fn local_board_ignores_meeting() {
        let mut b = Board::demo(3);
        assert!(!b.add_transcript("ua", "a", "x", 1.0));
        b.set_meeting(Some(&Meeting::default()));
        assert!(b.meeting().is_none());
        assert!(b.transcript().is_empty());
    }
}
