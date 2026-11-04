//! 手書きの線を整った図形にする(書式バーの「図形に整形」「AI で整形」)。
//!
//! - 端末だけ: [`board_core::handwriting::recognize_ink`] で四角・丸・三角・ひし形・線・矢印などの図形と、
//!   手書きの文字(英数字・かな・小学校で習う漢字。教えた字も)を認識する。線はサーバーに送らない
//! - AI: サーバー(`/api/boards/{id}/ai/ink`)が線を画像にして Claude に読ませる。
//!   「クラウド AI を使わない」設定([`crate::local_ai::cloud_off`])のときは端末だけの認識にする
//!
//! どちらも [`board_core::ink::build`] で要素にし、元の線を消して置き換える(1 回の Undo で戻る)。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use board_core::handwriting;
use board_core::ink::{self, InkItem, InkKind};
use board_core::{Element, ElementId, ShapeKind, Stroke};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use super::App;
use super::ai::{api_url, post_json};

thread_local! {
    /// お知らせの通し番号(古いお知らせを消すタイマーが新しいものを消さないように)。
    static TOAST_SEQ: Cell<u32> = const { Cell::new(0) };
}

/// 画面下にお知らせを出す。`state` は busy / done / error。busy 以外は数秒で消える。
pub(super) fn toast(doc: &web_sys::Document, msg: &str, state: &str) {
    let Some(el) = doc
        .get_element_by_id("ai-toast")
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    el.set_text_content(Some(msg));
    let _ = el.set_attribute("data-state", state);
    el.set_hidden(false);
    let seq = TOAST_SEQ.with(|s| {
        s.set(s.get().wrapping_add(1));
        s.get()
    });
    if state == "busy" {
        return;
    }
    let hide = Closure::once_into_js(move || {
        if TOAST_SEQ.with(Cell::get) == seq {
            el.set_hidden(true);
        }
    });
    if let Some(w) = web_sys::window() {
        let ms = if state == "error" { 7000 } else { 4000 };
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(hide.unchecked_ref(), ms);
    }
}

thread_local! {
    static INK_BUSY: Cell<bool> = const { Cell::new(false) };
}

impl App {
    /// 選択中の手書きの線(蛍光ペンを除く)。
    pub(super) fn ink_strokes(&self) -> Vec<Stroke> {
        self.editor
            .selection()
            .iter()
            .filter_map(|id| match self.board.get(*id) {
                Some(Element::Stroke(s)) if !s.highlighter && !s.meta.locked => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// 認識結果で線を置き換える。置き換えた図形の数を返す。
    pub(super) fn apply_ink(&mut self, items: &[InkItem], strokes: &[Stroke]) -> usize {
        let refs: Vec<&Stroke> = strokes.iter().collect();
        // 線の端は、手書きの近くにある既存の図形にもつなぐ。
        let area = strokes
            .iter()
            .map(|s| s.bounds())
            .reduce(|a, b| a.union(&b))
            .map(|r| r.expand(64.0));
        let targets: Vec<(ElementId, board_core::Rect)> = area
            .map(|area| {
                self.board
                    .elements()
                    .iter()
                    .filter_map(Element::as_shape)
                    .filter(|s| {
                        !matches!(
                            s.kind,
                            ShapeKind::Frame | ShapeKind::Text | ShapeKind::Stamp
                        )
                    })
                    .map(|s| (s.id, s.bounds()))
                    .filter(|(_, r)| r.intersects(&area))
                    .collect()
            })
            .unwrap_or_default();
        let board = &mut self.board;
        let built = ink::build(items, &refs, &targets, &mut || board.alloc_id());
        if built.add.is_empty() {
            return 0;
        }
        let n = built.add.len();
        self.board.end_gesture();
        self.board.remove(&built.remove);
        let ids: Vec<ElementId> = built.add.into_iter().map(|e| self.board.push(e)).collect();
        self.board.end_gesture();
        // 残った線(読めなかったもの)も選択に残して、続けて直せるようにする。
        let mut sel = ids;
        sel.extend(
            strokes
                .iter()
                .map(|s| s.id)
                .filter(|id| !built.remove.contains(id)),
        );
        self.editor.set_selection(sel);
        self.dirty = true;
        n
    }

    /// 端末だけで認識して置き換える。
    pub(super) fn beautify_ink(&mut self) {
        if !self.can_edit() {
            return;
        }
        let strokes = self.ink_strokes();
        if strokes.is_empty() {
            toast(
                &self.document,
                "整形する手書きの線を選んでください",
                "error",
            );
            return;
        }
        let refs: Vec<&Stroke> = strokes.iter().collect();
        let items = handwriting::recognize_ink(&refs, &crate::local_ai::recognizer());
        let texts = items
            .iter()
            .filter(|i| matches!(i.kind, InkKind::Text(_)))
            .count();
        let shapes = items.len() - texts;
        let n = self.apply_ink(&items, &strokes);
        if n == 0 {
            toast(
                &self.document,
                if crate::local_ai::cloud_off() {
                    "図形や文字として読める線がありませんでした"
                } else {
                    "図形や文字として読める線がありませんでした。「AI で整形」も試せます"
                },
                "error",
            );
        } else if texts > 0 {
            toast(
                &self.document,
                &format!(
                    "{shapes} 個の図形と {texts} 行の文字に整えました(端末内で認識)。Ctrl/⌘+Z で戻せます"
                ),
                "done",
            );
        } else {
            toast(
                &self.document,
                &format!("{n} 個の図形に整えました。Ctrl/⌘+Z で戻せます"),
                "done",
            );
        }
    }
}

/// `{"items":[{"kind","strokes","text"}]}` を読む。
fn parse_items(text: &str) -> Vec<InkItem> {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|it| {
            let kind = InkKind::from_name(
                it["kind"].as_str().unwrap_or_default(),
                it["text"].as_str().unwrap_or_default(),
            )?;
            let strokes = it["strokes"]
                .as_array()?
                .iter()
                .filter_map(|s| s.as_str())
                .filter_map(|h| u64::from_str_radix(h, 16).ok())
                .map(ElementId)
                .collect();
            Some(InkItem { strokes, kind })
        })
        .collect()
}

/// Claude に読んでもらって置き換える。
pub(super) fn beautify_ink_ai(app: &Rc<RefCell<App>>) {
    if INK_BUSY.with(Cell::get) {
        return;
    }
    if crate::local_ai::cloud_off() {
        // クラウド AI を使わない設定: 線を送らずに端末で読む。
        app.borrow_mut().beautify_ink();
        return;
    }
    let (window, doc, url, strokes) = {
        let a = app.borrow();
        if !a.can_edit() {
            return;
        }
        let Some(room) = a.room.clone() else {
            toast(
                &a.document,
                "AI 機能は共同編集のボードでだけ使えます",
                "error",
            );
            return;
        };
        let strokes = a.ink_strokes();
        if strokes.is_empty() {
            toast(&a.document, "整形する手書きの線を選んでください", "error");
            return;
        }
        let Some(url) = api_url(&a.window, &room, "ink") else {
            return;
        };
        (a.window.clone(), a.document.clone(), url, strokes)
    };
    let ids: Vec<String> = strokes.iter().map(|s| format!("{:x}", s.id.0)).collect();
    let body = serde_json::json!({ "ids": ids }).to_string();
    INK_BUSY.with(|b| b.set(true));
    toast(
        &doc,
        &format!("AI が {} 本の線を読んでいます…", strokes.len()),
        "busy",
    );
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = post_json(&window, &url, &body).await;
        INK_BUSY.with(|b| b.set(false));
        let text = match result {
            Ok(v) => js_sys::JSON::stringify(&v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_default(),
            Err(e) => {
                toast(&doc, &e, "error");
                return;
            }
        };
        let items = parse_items(&text);
        // 待っている間に消された・動かされた線は、今の状態で組み立てる。
        let mut a = app.borrow_mut();
        let current: Vec<Stroke> = strokes
            .iter()
            .filter_map(|s| match a.board.get(s.id) {
                Some(Element::Stroke(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        let n = a.apply_ink(&items, &current);
        if n == 0 {
            toast(&doc, "AI にも図形として読めませんでした", "error");
        } else {
            toast(
                &doc,
                &format!("AI が {n} 個の図形と文字に整えました。Ctrl/⌘+Z で戻せます"),
                "done",
            );
        }
    });
}
