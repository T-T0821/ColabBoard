//! 他の参加者のカーソル表示と、自分の表示名・色。

use std::collections::HashMap;

use board_core::protocol::{Presence, Voice};
use board_core::{Camera, Vec2};
use web_sys::{Document, HtmlElement, Window};

/// 参加者の色(カーソルと名札)。
const COLORS: [u32; 8] = [
    0xe5484d, 0x2f6fed, 0x30a46c, 0xf76b15, 0x8e4ec6, 0x0090ff, 0xd6409f, 0x12a594,
];
const ANIMALS: [&str; 12] = [
    "カワウソ",
    "ペンギン",
    "キツネ",
    "パンダ",
    "コアラ",
    "ラッコ",
    "フクロウ",
    "イルカ",
    "リス",
    "ハリネズミ",
    "シマエナガ",
    "カピバラ",
];

pub struct Identity {
    /// 参加者を見分ける ID(投票の集計に使う)。タブを開き直しても変わらない。
    pub id: String,
    pub name: String,
    pub color: u32,
}

impl Identity {
    /// localStorage に覚えておき、次回も同じ名前・色にする。
    pub fn load(window: &Window) -> Self {
        let storage = window.local_storage().ok().flatten();
        let get = |k: &str| storage.as_ref().and_then(|s| s.get_item(k).ok().flatten());
        let rand = || (js_sys::Math::random() * 1e9) as usize;
        let name = get("colabboard.name")
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("ゲスト・{}", ANIMALS[rand() % ANIMALS.len()]));
        let color = get("colabboard.color")
            .and_then(|c| u32::from_str_radix(&c, 16).ok())
            .unwrap_or_else(|| COLORS[rand() % COLORS.len()]);
        let id = get("colabboard.user")
            .filter(|u| (8..=32).contains(&u.len()) && u.bytes().all(|b| b.is_ascii_hexdigit()))
            .unwrap_or_else(|| format!("{:08x}{:08x}", rand() as u32, rand() as u32));
        if let Some(s) = &storage {
            let _ = s.set_item("colabboard.user", &id);
            let _ = s.set_item("colabboard.name", &name);
            let _ = s.set_item("colabboard.color", &format!("{color:06x}"));
        }
        Self { id, name, color }
    }
}

/// ログインしたときの表示名を、次回からのカーソルの名前にする。
pub fn remember_name(window: &Window, name: &str) {
    if let Ok(Some(s)) = window.local_storage() {
        let _ = s.set_item("colabboard.name", name);
    }
}

struct Peer {
    presence: Presence,
    el: HtmlElement,
}

pub struct Cursors {
    document: Document,
    layer: HtmlElement,
    peers: HashMap<u32, Peer>,
}

impl Cursors {
    pub fn new(document: Document, layer: HtmlElement) -> Self {
        Self {
            document,
            layer,
            peers: HashMap::new(),
        }
    }

    /// 自分以外の参加者数(カーソルを一度でも送ってきた人)。
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn get(&self, peer: u32) -> Option<&Presence> {
        self.peers.get(&peer).map(|p| &p.presence)
    }

    pub fn peers(&self) -> impl Iterator<Item = &Presence> {
        self.peers.values().map(|p| &p.presence)
    }

    pub fn upsert(&mut self, p: Presence) {
        let peer = match self.peers.get_mut(&p.peer) {
            Some(peer) => peer,
            None => {
                let Some(el) = self.create_element() else {
                    return;
                };
                self.peers.entry(p.peer).or_insert(Peer {
                    presence: p.clone(),
                    el,
                })
            }
        };
        let css = format!("#{:06x}", p.color & 0xff_ffff);
        let _ = peer.el.style().set_property("--c", &css);
        if let Some(label) = peer.el.last_element_child() {
            label.set_text_content(Some(&p.name));
        }
        peer.presence = p;
    }

    /// カーソルの名札に通話の状態(マイク・ミュート・発話中)を出す。
    pub fn set_voice(&mut self, peer: u32, voice: Option<Voice>) {
        let Some(p) = self.peers.get(&peer) else {
            return;
        };
        let el = &p.el;
        match voice.filter(|v| v.in_call) {
            Some(v) => {
                let _ = el.set_attribute("data-voice", if v.muted { "muted" } else { "on" });
                let _ = el.toggle_attribute_with_force("data-speaking", v.speaking && !v.muted);
            }
            None => {
                let _ = el.remove_attribute("data-voice");
                let _ = el.remove_attribute("data-speaking");
            }
        }
    }

    pub fn remove(&mut self, peer: u32) {
        if let Some(p) = self.peers.remove(&peer) {
            p.el.remove();
        }
    }

    pub fn clear(&mut self) {
        for (_, p) in self.peers.drain() {
            p.el.remove();
        }
    }

    fn create_element(&self) -> Option<HtmlElement> {
        let el: HtmlElement =
            wasm_bindgen::JsCast::dyn_into(self.document.create_element("div").ok()?).ok()?;
        el.set_class_name("cursor");
        el.set_inner_html(
            r#"<svg viewBox="0 0 18 22" width="18" height="22" aria-hidden="true"><path d="M2.5 2.5v14.2l3.9-3.6 2.6 5.9 2.6-1.1-2.5-5.8h5.4z"/></svg><span class="cursor-name"></span>"#,
        );
        self.layer.append_child(&el).ok()?;
        Some(el)
    }

    /// カメラに合わせて画面上の位置を更新する。`origin` は canvas 左上(CSS ピクセル)。
    pub fn layout(&self, camera: &Camera, dpr: f32, origin: Vec2) {
        for peer in self.peers.values() {
            let p = &peer.presence;
            let style = peer.el.style();
            if !p.is_visible() {
                let _ = style.set_property("display", "none");
                continue;
            }
            let s = camera.world_to_screen(Vec2::new(p.x, p.y)) / dpr + origin;
            let _ = style.set_property("display", "block");
            let _ = style.set_property("transform", &format!("translate({}px, {}px)", s.x, s.y));
        }
    }
}
