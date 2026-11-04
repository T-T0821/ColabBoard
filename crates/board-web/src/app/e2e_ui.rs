//! エンドツーエンド暗号化ボードの画面: ログの復号と取り込み、ログをまとめる、
//! 鍵のパネル(指紋・鍵付きリンク・鍵の作り直し)と通報。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::Element as BoardElement;
use board_core::api::{E2eInfo, NewReport, Report, RotateE2e};
use board_core::protocol::Message;
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, HtmlInputElement, HtmlSelectElement};

use super::{App, by_id, listen};
use crate::e2e::{self, Sealer};
use crate::http::{self, escape};

/// 通報に添えるボードの文字の上限(サーバーと同じ)。
const MAX_EVIDENCE_BYTES: usize = 64 * 1024;

/// サーバーのログについて分かっていること。
#[derive(Default)]
pub struct SealedState {
    /// ログの先頭の通し番号。
    base: u64,
    /// 次の通し番号(ここまで受け取った)。
    next: u64,
    /// ログに溜まっている件数の見込み。
    count: usize,
}

pub struct E2eUi {
    badge: HtmlElement,
    panel: HtmlElement,
    message: Option<(bool, String)>,
    busy: bool,
}

impl E2eUi {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            badge: by_id(document, "e2e-badge")?,
            panel: by_id(document, "e2e-panel")?,
            message: None,
            busy: false,
        })
    }
}

impl App {
    /// 初回同期のログを復号し、1 つにまとめたボード全体を返す。
    pub(super) fn open_sealed_log(&mut self, base: u64, blobs: &[Vec<u8>]) -> Option<Vec<u8>> {
        let Some(sealer) = &self.e2e else {
            log::warn!("暗号化ボードですが、鍵がありません");
            return None;
        };
        let mut plain = Vec::with_capacity(blobs.len());
        let mut failed = 0;
        for b in blobs {
            match sealer.open(b) {
                Ok(p) => plain.push(p),
                Err(e) => {
                    failed += 1;
                    log::warn!("復号できない暗号文があります: {e}");
                }
            }
        }
        if failed > 0 {
            super::show_error(&format!(
                "{failed} 件の内容を復号できませんでした。鍵が作り直された可能性があります。"
            ));
        }
        self.sealed = SealedState {
            base,
            next: base + blobs.len() as u64,
            count: blobs.len(),
        };
        match board_core::board::merge_updates(&plain) {
            Ok(m) => Some(m),
            Err(e) => {
                log::error!("復号した内容をまとめられません: {e}");
                Some(Vec::new())
            }
        }
    }

    /// 1 回の編集(暗号文)が届いた。
    pub(super) fn on_sealed(&mut self, seq: u64, blob: &[u8]) {
        let Some(sealer) = &self.e2e else { return };
        match sealer.open(blob) {
            Ok(update) => {
                if let Err(e) = self.live_board_mut().apply_remote(&update) {
                    log::warn!("更新を取り込めません: {e}");
                }
                self.editor.prune_selection(&self.board);
                self.dirty = true;
            }
            Err(e) => log::warn!("復号できない更新を捨てました: {e}"),
        }
        self.sealed.next = self.sealed.next.max(seq + 1);
        self.sealed.count += 1;
        self.maybe_compact(self.sealed.count >= e2e::COMPACT_AT * 2);
    }

    /// ログが長くなったら、ボード全体を 1 つの暗号文にまとめてサーバーに置き換えてもらう。
    /// まとめるのは編集できる人だけ(閲覧者の送ったものはサーバーが捨てる)。
    pub(super) fn maybe_compact(&mut self, needed: bool) {
        if !needed || !self.can_edit() || self.sealed.next <= self.sealed.base + 1 {
            return;
        }
        let Some(sealer) = &self.e2e else { return };
        let live = self.live_board.as_ref().unwrap_or(&self.board);
        let Ok(state) = live.encode_diff(None) else {
            return;
        };
        let msg = Message::Compact {
            base: self.sealed.base,
            upto: self.sealed.next,
            blob: sealer.seal_snapshot(&state),
        };
        if let Some(net) = self.net.as_ref() {
            net.send(&msg);
        }
        log::info!(
            "暗号化ボードのログをまとめました({} 件 → 1 件)",
            self.sealed.next - self.sealed.base
        );
        self.sealed.base = self.sealed.next - 1;
        self.sealed.count = 1;
    }

    fn e2e_ctx(&self) -> Option<(String, Option<String>, bool)> {
        let c = self.chrome.as_ref()?.borrow();
        Some((c.room.clone(), c.key.clone(), c.can_manage()))
    }

    /// 鍵のバッジとパネルを今の状態に合わせる。
    pub(super) fn render_e2e(&self) {
        let ui = &self.e2e_ui;
        let Some(sealer) = &self.e2e else {
            ui.badge.set_hidden(true);
            return;
        };
        ui.badge.set_hidden(false);
        let _ = ui.badge.set_attribute(
            "aria-expanded",
            if ui.panel.hidden() { "false" } else { "true" },
        );
        if ui.panel.hidden() {
            return;
        }
        let Some((room, link_key, owner)) = self.e2e_ctx() else {
            return;
        };
        let link = e2e::key_link(&self.window, &room, link_key.as_deref(), sealer.key());
        let rotate = if owner {
            format!(
                r#"<section>
  <strong>鍵を作り直す</strong>
  <p class="muted small">メンバーを外したときや、鍵付きリンクが広まりすぎたときに。今の内容を新しい鍵で暗号化し直し、古い鍵では以後の内容を読めなくなります。ほかの人には新しい鍵付きリンクを渡し直してください。</p>
  <button type="button" data-action="e2e-rotate" {busy}>鍵を作り直す</button>
</section>"#,
                busy = if ui.busy { "disabled" } else { "" }
            )
        } else {
            String::new()
        };
        let message = ui
            .message
            .as_ref()
            .map(|(ok, m)| {
                format!(
                    r#"<p class="{cls}" role="status">{m}</p>"#,
                    cls = if *ok { "form-ok" } else { "form-error" },
                    m = escape(m)
                )
            })
            .unwrap_or_default();
        ui.panel.set_inner_html(&format!(
            r#"<header class="row"><strong class="grow"><span aria-hidden="true">🔒</span> エンドツーエンド暗号化</strong><button class="quiet" data-action="e2e-close" aria-label="閉じる">×</button></header>
<p class="muted small">このボードの中身は、このブラウザで暗号化してからサーバーに送っています。サーバーの管理者にも読めません。鍵はリンクの # 以降と、開いた端末にだけあります。</p>
<section>
  <strong>鍵の指紋</strong>
  <p><code class="fingerprint" translate="no">{fp}</code></p>
  <p class="muted small">一緒に使う人の画面と同じ表示なら、同じ鍵です。</p>
</section>
<section>
  <strong>鍵付きリンク</strong>
  <div class="row"><input class="grow" readonly value="{link}" data-copy aria-label="鍵付きリンク" /><button data-action="e2e-copy">コピー</button></div>
  <p class="muted small">鍵だけではボードは開けません(招待か共有リンクの権限も必要です)。鍵をなくすと、誰も中身を読めなくなります。</p>
</section>
{rotate}
<section>
  <strong>この種類のボードで使えない機能</strong>
  <p class="muted small">AI・コメント・バージョン履歴・Web 公開・動画メモ・課題カードは、サーバーが中身を読む必要があるため使えません。ボードのタイトルとカーソルの位置は暗号化されません。</p>
</section>
<section class="e2e-report">
  <details>
    <summary>このボードを通報する</summary>
    <form data-form="e2e-report" class="stack">
      <label>理由<select name="reason">{reasons}</select></label>
      <label>くわしく(任意)<textarea name="comment" rows="3" maxlength="2000" placeholder="何が問題か"></textarea></label>
      <label class="row"><input type="checkbox" name="evidence" /> <span class="grow small">ボードの文字を添えて、管理者が内容を確かめられるようにする(添えなければ、暗号化されたボードの中身は管理者にも見えません)</span></label>
      <button type="submit" class="danger" {busy}>通報する</button>
    </form>
  </details>
</section>
{message}"#,
            fp = escape(&sealer.key().fingerprint()),
            link = escape(&link),
            reasons = e2e::reason_options(),
            busy = if ui.busy { "disabled" } else { "" },
        ));
    }

    /// 通報に添える、ボードの文字(付箋・図形・テキスト)。
    fn board_text(&self) -> String {
        let live = self.live_board.as_ref().unwrap_or(&self.board);
        let mut out = String::new();
        for e in live.elements() {
            if let BoardElement::Shape(s) = e {
                let t = s.text.trim();
                if t.is_empty() {
                    continue;
                }
                if out.len() + t.len() + 1 > MAX_EVIDENCE_BYTES {
                    break;
                }
                out.push_str(t);
                out.push('\n');
            }
        }
        out
    }

    fn e2e_message(&mut self, ok: bool, msg: impl Into<String>) {
        self.e2e_ui.message = Some((ok, msg.into()));
        self.e2e_ui.busy = false;
        self.render_e2e();
    }
}

fn action_of(e: &web_sys::Event) -> Option<(String, Element)> {
    let el = e
        .target()?
        .dyn_into::<Element>()
        .ok()?
        .closest("[data-action]")
        .ok()??;
    Some((el.get_attribute("data-action")?, el))
}

/// 鍵を作り直す: 新しい鍵で今のボード全体を暗号化してサーバーに置き換えてもらう。
fn rotate(app: &Rc<RefCell<App>>) {
    let prepared = {
        let mut a = app.borrow_mut();
        let Some((room, _, true)) = a.e2e_ctx() else {
            return;
        };
        if a.e2e.is_none() || a.e2e_ui.busy {
            return;
        }
        let live = a.live_board.as_ref().unwrap_or(&a.board);
        let Ok(state) = live.encode_diff(None) else {
            return;
        };
        let new_key = e2e::new_key();
        let blob = Sealer::new(&room, new_key.clone()).seal_snapshot(&state);
        // 返事より先にサーバーがつなぎ直させるので、新しい鍵は先に覚えておく(失敗したら戻す)。
        let old_key = a.e2e.as_ref().map(|s| s.key().clone());
        e2e::store_key(&a.window, &room, &new_key);
        if let Some(s) = a.e2e.as_mut() {
            s.set_key(new_key.clone());
        }
        if let Some(c) = &a.chrome {
            c.borrow_mut().e2e_key = Some(new_key.to_link());
        }
        a.e2e_ui.busy = true;
        a.e2e_ui.message = Some((true, "鍵を作り直しています…".into()));
        a.render_e2e();
        (room, new_key, old_key, blob)
    };
    let (room, new_key, old_key, blob) = prepared;
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let req = RotateE2e {
            key_id: new_key.key_id(),
            snapshot: e2e::b64(&blob),
        };
        let res = http::send::<_, E2eInfo>(
            "POST",
            &format!("/boards/{}/e2e/rotate", http::encode(&room)),
            &req,
        )
        .await;
        let mut a = app.borrow_mut();
        match res {
            Ok(_) => a.e2e_message(
                true,
                "鍵を作り直しました。上の新しい鍵付きリンクを、一緒に使う人に渡し直してください。",
            ),
            Err(e) => {
                if let Some(old) = old_key {
                    e2e::store_key(&a.window, &room, &old);
                    if let Some(c) = &a.chrome {
                        c.borrow_mut().e2e_key = Some(old.to_link());
                    }
                    if let Some(s) = a.e2e.as_mut() {
                        s.set_key(old);
                    }
                }
                a.e2e_message(false, format!("作り直せませんでした: {}", e.message()));
            }
        }
    });
}

fn report(app: &Rc<RefCell<App>>, form: &Element) {
    let value = |name: &str| -> String {
        form.query_selector(&format!("[name={name}]"))
            .ok()
            .flatten()
            .map(|el| {
                if let Some(s) = el.dyn_ref::<HtmlSelectElement>() {
                    s.value()
                } else if let Some(t) = el.dyn_ref::<web_sys::HtmlTextAreaElement>() {
                    t.value()
                } else if let Some(i) = el.dyn_ref::<HtmlInputElement>() {
                    if i.type_() == "checkbox" {
                        i.checked().to_string()
                    } else {
                        i.value()
                    }
                } else {
                    String::new()
                }
            })
            .unwrap_or_default()
    };
    let (path, req) = {
        let mut a = app.borrow_mut();
        let Some((room, key, _)) = a.e2e_ctx() else {
            return;
        };
        if a.e2e_ui.busy {
            return;
        }
        let evidence = (value("evidence") == "true").then(|| a.board_text());
        a.e2e_ui.busy = true;
        (
            http::board_path(&room, key.as_deref(), "/reports"),
            NewReport {
                reason: value("reason"),
                comment: value("comment"),
                evidence,
            },
        )
    };
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let res = http::send::<_, Report>("POST", &path, &req).await;
        let mut a = app.borrow_mut();
        match res {
            Ok(_) => a.e2e_message(
                true,
                "通報しました。管理者が確認します。通報したことは、ほかの参加者には知らされません。",
            ),
            Err(e) => a.e2e_message(false, format!("通報できませんでした: {}", e.message())),
        }
    });
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let (badge, panel, sealed) = {
        let a = app.borrow();
        (
            a.e2e_ui.badge.clone(),
            a.e2e_ui.panel.clone(),
            a.e2e.is_some(),
        )
    };
    if sealed {
        if let Some(body) = app.borrow().document.body() {
            let _ = body.class_list().add_1("e2e-board");
        }
        app.borrow().render_e2e();
    }
    {
        let app = app.clone();
        listen(
            badge.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                let mut a = app.borrow_mut();
                let open = a.e2e_ui.panel.hidden();
                a.e2e_ui.panel.set_hidden(!open);
                if open {
                    a.e2e_ui.message = None;
                }
                a.render_e2e();
            },
        )?;
    }
    {
        let app = app.clone();
        listen(
            panel.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                let Some((action, el)) = action_of(&e) else {
                    return;
                };
                match action.as_str() {
                    "e2e-close" => {
                        let a = app.borrow();
                        a.e2e_ui.panel.set_hidden(true);
                        a.render_e2e();
                    }
                    "e2e-copy" => {
                        let a = app.borrow();
                        if let Ok(Some(input)) = a.e2e_ui.panel.query_selector("[data-copy]")
                            && let Ok(input) = input.dyn_into::<HtmlInputElement>()
                        {
                            let _ = a.window.navigator().clipboard().write_text(&input.value());
                            el.set_text_content(Some("コピーしました"));
                        }
                    }
                    "e2e-rotate" => rotate(&app),
                    _ => {}
                }
            },
        )?;
    }
    {
        let app = app.clone();
        listen(panel.as_ref(), "submit", false, move |e: web_sys::Event| {
            e.prevent_default();
            if let Some(form) = e.target().and_then(|t| t.dyn_into::<Element>().ok())
                && form.get_attribute("data-form").as_deref() == Some("e2e-report")
            {
                report(&app, &form);
            }
        })?;
    }
    Ok(())
}
