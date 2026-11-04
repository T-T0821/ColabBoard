//! シーケンス図の中間表現と、ボードの要素への変換。
//!
//! 参加者(ライフライン)を左から右へ、メッセージを上から下へ並べる。
//! 要素は UML の部品(`crate::uml::lifeline` と `crate::uml::message`)で作るので、
//! パネルやテンプレートから置いたシーケンス図と同じ見た目になる。

use glam::Vec2;

use crate::connector::{Connector, Endpoint, PathStyle};
use crate::element::{Element, ElementId};
use crate::uml;

/// シーケンス図全体。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SequenceSpec {
    pub title: String,
    pub participants: Vec<Participant>,
    pub messages: Vec<Message>,
}

/// 参加者(ライフライン)。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Participant {
    pub id: String,
    pub label: String,
}

/// メッセージ(参加者から参加者への矢印)。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    pub from: String,
    pub to: String,
    pub label: String,
    /// 戻り(破線)。
    pub reply: bool,
}

/// ライフラインどうしの最小の間隔。
const GAP: f32 = 120.0;
/// ライフラインの頭の高さ(`uml::lifeline` と同じ)。
const HEAD: f32 = 56.0;
/// 最初のメッセージの、頭の下からの距離。
const FIRST: f32 = 60.0;
/// メッセージの間隔。
const STEP: f32 = 64.0;
/// 自分へのメッセージの折り返しの幅と高さ。
const SELF_W: f32 = 48.0;
const SELF_H: f32 = 28.0;

impl SequenceSpec {
    /// 参加者を足す(すでにあれば名前だけ上書き)。ID を返す。
    pub fn participant(&mut self, id: &str, label: Option<&str>) -> String {
        match self.participants.iter_mut().find(|p| p.id == id) {
            Some(p) => {
                if let Some(l) = label {
                    p.label = l.to_string();
                }
            }
            None => self.participants.push(Participant {
                id: id.to_string(),
                label: label.unwrap_or(id).to_string(),
            }),
        }
        id.to_string()
    }

    /// メッセージを足す(知らない参加者は足す)。
    pub fn message(&mut self, from: &str, to: &str, label: &str, reply: bool) {
        self.participant(from, None);
        self.participant(to, None);
        self.messages.push(Message {
            from: from.into(),
            to: to.into(),
            label: label.trim().into(),
            reply,
        });
    }
}

/// 自分へのメッセージ(ライフラインから右へ出て下へ折れる線)。
pub fn self_message(id: ElementId, x: f32, y: f32, text: &str, reply: bool) -> Connector {
    let mut c = uml::message(id, x, x + SELF_W, y, text, reply);
    c.to = Endpoint::Free(Vec2::new(x + SELF_W, y + SELF_H));
    c.style = PathStyle::Elbow;
    c.route(|_| None);
    c
}

/// 自分へのメッセージの折れ線の大きさ(読み取り側で見分けるため)。
pub(crate) const SELF_SIZE: Vec2 = Vec2::new(SELF_W, SELF_H);

/// シーケンス図を要素にする。全体の中心が `center` に来る。
pub fn build(
    spec: &SequenceSpec,
    center: Vec2,
    alloc: &mut dyn FnMut() -> ElementId,
) -> Vec<Element> {
    let length = FIRST + STEP * spec.messages.len().max(1) as f32;
    let mut out: Vec<Element> = Vec::new();
    let mut centers: Vec<(&str, f32)> = Vec::new();
    let mut x = 0.0;
    for p in &spec.participants {
        let (els, head) = uml::lifeline(alloc, Vec2::new(x, 0.0), &p.label, length);
        let w = els
            .iter()
            .find(|e| e.id() == head)
            .map_or(160.0, |e| e.bounds().size().x);
        centers.push((p.id.as_str(), x + w * 0.5));
        out.extend(els);
        x += w + GAP;
    }
    let center_of = |id: &str| centers.iter().find(|(p, _)| *p == id).map(|(_, x)| *x);
    for (k, m) in spec.messages.iter().enumerate() {
        let (Some(a), Some(b)) = (center_of(&m.from), center_of(&m.to)) else {
            continue;
        };
        let y = HEAD + FIRST + STEP * k as f32 - STEP * 0.5;
        let c = if m.from == m.to {
            self_message(alloc(), a, y, &m.label, m.reply)
        } else {
            uml::message(alloc(), a, b, y, &m.label, m.reply)
        };
        out.push(c.into());
    }
    if let Some(b) = out.iter().map(Element::bounds).reduce(|a, b| a.union(&b)) {
        let d = center - b.center();
        out.iter_mut().for_each(|e| e.translate(d));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_lifelines_and_messages_in_order() {
        let mut s = SequenceSpec::default();
        s.participant("a", Some("利用者"));
        s.message("a", "b", "こんにちは", false);
        s.message("b", "a", "やあ", true);
        s.message("b", "b", "考える", false);
        let mut n = 0;
        let els = build(&s, Vec2::ZERO, &mut || {
            n += 1;
            ElementId(n)
        });
        // ライフライン 2 本(頭 + 破線)とメッセージ 3 本。
        assert_eq!(els.len(), 2 * 2 + 3);
        let msgs: Vec<&Connector> = els
            .iter()
            .filter_map(|e| match e {
                Element::Connector(c) if !c.text.is_empty() => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(msgs.len(), 3);
        assert!(msgs[1].dashed && !msgs[0].dashed);
        assert!(msgs[0].bounds().min.y < msgs[1].bounds().min.y);
    }
}
