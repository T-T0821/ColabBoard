//! 絵文字リアクション(その場で浮かんで消える。保存しない)と、スタンプの選択。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::Vec2;
use board_core::protocol::{Message, Reaction};
use board_core::stamp::{STAMPS, Stamp};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, HtmlElement, MouseEvent};

use super::{App, by_id, listen};

/// 選べるリアクション。
pub const REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "🎉", "👀", "🔥"];

/// 浮かべておく時間(ミリ秒)。CSS のアニメーションと合わせる。
const SHOW_MS: i32 = 2200;

impl App {
    /// 自分のリアクションを、カーソルの位置(なければ画面の中央)に出して皆に送る。
    pub(super) fn send_reaction(&mut self, emoji: &str) {
        let at = self
            .cursor_world
            .unwrap_or_else(|| self.camera.screen_to_world(self.camera.viewport * 0.5));
        self.show_reaction(at, emoji, None);
        if let Some(net) = &self.net {
            net.send(&Message::Reaction(Reaction {
                peer: 0,
                x: at.x,
                y: at.y,
                emoji: emoji.to_string(),
            }));
        }
    }

    pub(super) fn on_reaction(&mut self, r: Reaction) {
        if !r.is_valid() {
            return;
        }
        let who = self.cursors.get(r.peer).map(|p| (p.name.clone(), p.color));
        self.show_reaction(Vec2::new(r.x, r.y), &r.emoji, who);
    }

    /// ワールド座標 `at` に絵文字を浮かべる。`who` は送った人の名前と色。
    fn show_reaction(&self, at: Vec2, emoji: &str, who: Option<(String, u32)>) {
        let Ok(el) = self.document.create_element("div") else {
            return;
        };
        let _ = el.set_attribute("class", "reaction");
        let _ = el.set_attribute("aria-hidden", "true");
        el.set_text_content(Some(emoji));
        if let Some((name, color)) = who
            && let Ok(tag) = self.document.create_element("small")
        {
            tag.set_text_content(Some(&name));
            let _ = tag.set_attribute("style", &format!("--c: #{color:06x}"));
            let _ = el.append_child(&tag);
        }
        let dpr = self.window.device_pixel_ratio() as f32;
        let canvas = self.canvas.get_bounding_client_rect();
        let p = self.camera.world_to_screen(at) / dpr;
        if let Some(h) = el.dyn_ref::<HtmlElement>() {
            let css = h.style();
            let _ = css.set_property("left", &format!("{}px", canvas.left() as f32 + p.x));
            let _ = css.set_property("top", &format!("{}px", canvas.top() as f32 + p.y));
        }
        let Some(body) = self.document.body() else {
            return;
        };
        let _ = body.append_child(&el);
        let remove = Closure::once_into_js(move || el.remove());
        let _ = self
            .window
            .set_timeout_with_callback_and_timeout_and_arguments_0(remove.unchecked_ref(), SHOW_MS);
    }
}

/// スタンプ・リアクションのポップオーバーを作る。
pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let picker: Element = by_id(&doc, "stamp-picker")?;

    let mut html = String::from(r#"<div class="picker-label">スタンプ(クリックで押す)</div>"#);
    for s in STAMPS {
        let [r, g, b, _] = s.color.0;
        html.push_str(&format!(
            r#"<button data-stamp="{}" title="{}"><span class="stamp-badge" style="--c: rgb({r},{g},{b})">{}</span></button>"#,
            s.name, s.title, s.glyph
        ));
    }
    html.push_str(r#"<div class="picker-label">リアクション(みんなの画面に浮かぶ)</div>"#);
    for e in REACTIONS {
        html.push_str(&format!(
            r#"<button data-emoji="{e}" title="リアクション {e}">{e}</button>"#
        ));
    }
    picker.set_inner_html(&html);

    {
        let app = app.clone();
        let picker2 = picker.clone();
        listen(picker.as_ref(), "click", false, move |e: MouseEvent| {
            e.stop_propagation();
            let Some(btn) = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|t| t.closest("button").ok().flatten())
            else {
                return;
            };
            let mut a = app.borrow_mut();
            if let Some(stamp) = btn
                .get_attribute("data-stamp")
                .and_then(|n| Stamp::by_name(&n))
            {
                if !a.can_edit() {
                    return;
                }
                a.editor.set_stamp(stamp);
                a.dirty = true;
                let _ = picker2.remove_attribute("data-open");
            } else if let Some(emoji) = btn.get_attribute("data-emoji") {
                a.send_reaction(&emoji);
            }
        })?;
    }
    super::toolbar::popover(app, "tool-stamp", picker)?;
    Ok(())
}
