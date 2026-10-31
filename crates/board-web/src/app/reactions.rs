//! 絵文字リアクション(その場で浮かんで消える。保存しない)と、スタンプの選択。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::Vec2;
use board_core::protocol::{Message, Reaction};
use board_core::stamp::{STAMPS, Stamp, StampGroup};
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

/// パレットの分類の見出し。
fn group_label(g: StampGroup) -> &'static str {
    match g {
        StampGroup::Reaction => "反応",
        StampGroup::Status => "状態",
        StampGroup::Mark => "記号",
        StampGroup::Number => "番号",
        StampGroup::Dot => "投票",
        StampGroup::Hanko => "ハンコ",
    }
}

/// HTML に埋める文字のエスケープ。
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// パレットのボタン 1 つ。
fn stamp_button(s: &Stamp) -> String {
    let [r, g, b, _] = s.color.0;
    let class = if s.is_hanko() {
        "stamp-badge hanko"
    } else if s.group == StampGroup::Dot {
        "stamp-badge dot"
    } else {
        "stamp-badge"
    };
    let long = if s.glyph.chars().count() >= 3 {
        " long"
    } else {
        ""
    };
    format!(
        r#"<button data-stamp="{}" title="{}"><span class="{class}{long}" translate="no" style="--c: rgb({r},{g},{b})">{}</span></button>"#,
        s.name,
        esc(s.title),
        esc(&s.glyph)
    )
}

/// 印鑑に入れる名前の既定値。「山田 太郎」なら姓の「山田」、「ゲスト・カピバラ」なら空。
fn seal_name(display: &str) -> String {
    if display.starts_with("ゲスト") {
        return String::new();
    }
    let first = display.split_whitespace().next().unwrap_or("");
    let limit = if first.is_ascii() { 8 } else { 4 };
    first.chars().take(limit).collect()
}

/// 今日の日付(日付印用、`26.10.31` の形)。
fn today() -> String {
    let d = js_sys::Date::new_0();
    format!(
        "{:02}.{:02}.{:02}",
        d.get_full_year() % 100,
        d.get_month() + 1,
        d.get_date()
    )
}

/// スタンプ・リアクションのポップオーバーを作る。
pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let picker: Element = by_id(&doc, "stamp-picker")?;
    let name = seal_name(&app.borrow().identity.name);

    let mut html = String::from(r#"<div class="stamp-tabs" role="tablist">"#);
    for (i, g) in StampGroup::ALL.iter().enumerate() {
        html.push_str(&format!(
            r#"<button class="stamp-tab" role="tab" data-tab="{}" aria-selected="{}">{}</button>"#,
            g.key(),
            i == 0,
            group_label(*g)
        ));
    }
    html.push_str(r#"<button class="stamp-tab" role="tab" data-tab="emoji" aria-selected="false">絵文字</button></div>"#);
    for (i, g) in StampGroup::ALL.iter().enumerate() {
        let hidden = if i == 0 { "" } else { " hidden" };
        html.push_str(&format!(
            r#"<div class="stamp-group" data-group="{}"{hidden}>"#,
            g.key()
        ));
        if *g == StampGroup::Hanko {
            html.push_str(&format!(
                r#"<div class="hanko-maker"><input id="hanko-name" maxlength="8" placeholder="印鑑の名前" value="{}" aria-label="印鑑の名前"><button data-hanko="name" title="名前の印鑑を押す"><span class="stamp-badge hanko" translate="no">印</span></button><button data-hanko="date" title="日付印(承認)を押す"><span class="stamp-badge hanko long" translate="no">日付</span></button></div>"#,
                esc(&name)
            ));
        }
        for s in STAMPS.iter().filter(|s| s.group == *g) {
            html.push_str(&stamp_button(s));
        }
        html.push_str("</div>");
    }
    html.push_str(r#"<div class="stamp-group" data-group="emoji" hidden><div class="picker-label">リアクション(みんなの画面に浮かぶ)</div>"#);
    for e in REACTIONS {
        html.push_str(&format!(
            r#"<button data-emoji="{e}" title="リアクション {e}">{e}</button>"#
        ));
    }
    html.push_str("</div>");
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
            if let Some(tab) = btn.get_attribute("data-tab") {
                select_tab(&picker2, &tab);
                return;
            }
            let mut a = app.borrow_mut();
            let hanko = btn.get_attribute("data-hanko").map(|kind| {
                let name = a
                    .document
                    .get_element_by_id("hanko-name")
                    .and_then(|i| i.dyn_into::<web_sys::HtmlInputElement>().ok())
                    .map(|i| i.value())
                    .unwrap_or_default();
                if kind == "date" {
                    Stamp::date_hanko("承認", &today(), &name)
                } else {
                    Stamp::name_hanko(&name)
                }
            });
            if let Some(stamp) = hanko.or_else(|| {
                btn.get_attribute("data-stamp")
                    .and_then(|n| Stamp::by_name(&n))
            }) {
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

/// パレットの分類を切り替える。
fn select_tab(picker: &Element, tab: &str) {
    let Ok(tabs) = picker.query_selector_all(".stamp-tab") else {
        return;
    };
    for i in 0..tabs.length() {
        if let Some(t) = tabs.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
            let on = t.get_attribute("data-tab").as_deref() == Some(tab);
            let _ = t.set_attribute("aria-selected", if on { "true" } else { "false" });
        }
    }
    let Ok(groups) = picker.query_selector_all(".stamp-group") else {
        return;
    };
    for i in 0..groups.length() {
        if let Some(g) = groups.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
            if g.get_attribute("data-group").as_deref() == Some(tab) {
                let _ = g.remove_attribute("hidden");
            } else {
                let _ = g.set_attribute("hidden", "");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_name_takes_surname() {
        assert_eq!(seal_name("山田 太郎"), "山田");
        assert_eq!(seal_name("ゲスト・カピバラ"), "");
        assert_eq!(seal_name("Tsunomon"), "Tsunomon");
    }
}
