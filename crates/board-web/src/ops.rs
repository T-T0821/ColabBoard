//! 運用の画面(サーバー管理者だけ): バックアップ・復元。
//!
//! ボード一覧の「運用」ボタンか `?ops=backups` で開く。ホーム画面とは別の
//! 要素(`#ops`)に描くので、ホーム画面の描き直しに巻き込まれない。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::api::ops::{BackupBoard, BackupInfo, BackupStatus, RestoreResult};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlElement};

use crate::http::{self, escape};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Backups,
}

impl Tab {
    pub fn parse(s: &str) -> Option<Tab> {
        match s {
            "backups" => Some(Tab::Backups),
            _ => None,
        }
    }
}

struct Ops {
    el: HtmlElement,
    tab: Tab,
    backups: Option<BackupStatus>,
    /// ボードを選んでいるバックアップと、その中のボード。
    picking: Option<(BackupInfo, Option<Vec<BackupBoard>>)>,
    /// 戻す確認中のボード。
    confirm: Option<String>,
    busy: bool,
    /// (うまくいったか, 文)。
    notice: Option<(bool, String)>,
}

type Shared = Rc<RefCell<Ops>>;

thread_local! {
    static OPEN: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

/// 開く(開いていればタブを切り替える)。
pub fn open(tab: Tab) {
    if let Some(ops) = OPEN.with(|o| o.borrow().clone()) {
        ops.borrow_mut().tab = tab;
        load(ops.clone(), tab);
        ops.borrow().render();
        return;
    }
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(el) = document.create_element("div") else {
        return;
    };
    let Ok(el) = el.dyn_into::<HtmlElement>() else {
        return;
    };
    el.set_id("ops");
    el.set_class_name("ops-overlay");
    if let Some(body) = document.body() {
        let _ = body.append_child(&el);
    }
    let ops: Shared = Rc::new(RefCell::new(Ops {
        el: el.clone(),
        tab,
        backups: None,
        picking: None,
        confirm: None,
        busy: false,
        notice: None,
    }));
    install(&ops);
    OPEN.with(|o| *o.borrow_mut() = Some(ops.clone()));
    ops.borrow().render();
    load(ops, tab);
}

fn close() {
    if let Some(ops) = OPEN.with(|o| o.borrow_mut().take()) {
        ops.borrow().el.remove();
    }
}

fn load(ops: Shared, tab: Tab) {
    match tab {
        Tab::Backups => wasm_bindgen_futures::spawn_local(load_backups(ops)),
    }
}

async fn load_backups(ops: Shared) {
    let res = http::get::<BackupStatus>("/admin/backups").await;
    let mut o = ops.borrow_mut();
    match res {
        Ok(s) => o.backups = Some(s),
        Err(e) => o.notice = Some((false, e.message())),
    }
    o.render();
}

/// 「2026/10/31 21:05」の形(端末の時刻帯)。
fn datetime(ms: u64) -> String {
    let d = js_sys::Date::new(&JsValue::from_f64(ms as f64));
    format!(
        "{}/{:02}/{:02} {:02}:{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
    )
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

impl Ops {
    fn render(&self) {
        let notice = match &self.notice {
            Some((ok, m)) => format!(
                r#"<p class="ops-notice" data-ok="{ok}" role="status">{}</p>"#,
                escape(m)
            ),
            None => String::new(),
        };
        let body = match self.tab {
            Tab::Backups => self.render_backups(),
        };
        self.el.set_inner_html(&format!(
            r#"<div class="ops-dialog" role="dialog" aria-modal="true" aria-label="運用">
  <header class="ops-head">
    <h2>運用</h2>
    <span class="spacer"></span>
    <button type="button" class="quiet" data-ops="close">閉じる</button>
  </header>
  {notice}
  <div class="ops-body">{body}</div>
</div>"#,
        ));
    }

    fn render_backups(&self) -> String {
        let Some(st) = &self.backups else {
            return r#"<p class="muted">読み込み中…</p>"#.into();
        };
        if !st.enabled {
            return r#"<p class="muted">バックアップは無効です。サーバーの <code>DATA_DIR</code> を設定するか、<code>BACKUP_LOCATION</code> で置き場所を指定してください。</p>"#.into();
        }
        let schedule = if st.interval_hours == 0 {
            "定期バックアップは止めています(手動だけ)".to_string()
        } else {
            let next = st
                .next_at
                .map(|t| format!(" · <span>次回</span> {}", escape(&datetime(t))))
                .unwrap_or_default();
            format!(
                "<span>{} 時間ごと</span> · <span>定期と手動それぞれ {} 個まで残す</span>{next}",
                st.interval_hours, st.keep
            )
        };
        let error = match &st.last_error {
            Some(e) => format!(
                r#"<p class="ops-notice" data-ok="false">前回の失敗: {}</p>"#,
                escape(e)
            ),
            None => String::new(),
        };
        let mut rows = String::new();
        for b in &st.backups {
            let by =
                b.by.as_deref()
                    .map(|l| format!(r#" <span class="muted">@{}</span>"#, escape(l)))
                    .unwrap_or_default();
            let skipped = if b.skipped.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<div class="muted small">入っていない: {}</div>"#,
                    escape(&b.skipped.join(", "))
                )
            };
            rows.push_str(&format!(
                r#"<tr><td class="nowrap">{when}</td><td>{kind}{by}</td><td>{boards} 件</td><td class="nowrap">{size}{skipped}</td>
<td class="nowrap"><a class="button" href="{base}/api/admin/backups/{id}/download" download>ダウンロード</a>
<button type="button" class="quiet" data-ops="pick" data-id="{id}">ボードを戻す…</button></td></tr>"#,
                when = escape(&datetime(b.created)),
                kind = b.trigger_label(),
                boards = b.boards,
                size = size(b.bytes),
                base = escape(&http::base()),
                id = b.id,
            ));
        }
        if rows.is_empty() {
            rows = r#"<tr><td colspan="5" class="muted">まだバックアップがありません。</td></tr>"#
                .into();
        }
        let busy = self.busy || st.running;
        format!(
            r#"<div class="ops-toolbar">
  <div><div>保存先: <span translate="no">{target}</span></div><div class="muted small">{schedule}</div></div>
  <span class="spacer"></span>
  <button type="button" class="primary" data-ops="backup-now" {disabled}>{label}</button>
</div>
{error}
<div class="audit-scroll"><table class="audit ops-backups"><thead><tr><th>日時</th><th>種類</th><th>ボード</th><th>大きさ</th><th></th></tr></thead><tbody>{rows}</tbody></table></div>
{pick}
<p class="muted small">サーバー全体を戻すときは、サーバーを止めて <code>board-server restore &lt;ID&gt; --yes</code> を実行します(今のデータは脇に残ります)。</p>"#,
            target = escape(st.target.as_deref().unwrap_or("")),
            disabled = if busy { "disabled" } else { "" },
            label = if busy {
                "取っています…"
            } else {
                "今すぐバックアップ"
            },
            pick = self.render_pick(),
        )
    }

    fn render_pick(&self) -> String {
        let Some((info, boards)) = &self.picking else {
            return String::new();
        };
        let list = match boards {
            None => r#"<p class="muted">読み込み中…</p>"#.to_string(),
            Some(bs) if bs.is_empty() => {
                r#"<p class="muted">このバックアップにボードはありません。</p>"#.to_string()
            }
            Some(bs) => {
                let mut rows = String::new();
                for b in bs {
                    let state = match b.current.as_str() {
                        "exists" => "今もある",
                        "deleted" => "削除済み",
                        _ => "このサーバーにない",
                    };
                    let action = if self.confirm.as_deref() == Some(b.id.as_str()) {
                        format!(
                            r#"<span class="confirm">{warn}</span>
<button type="button" class="primary" data-ops="restore" data-id="{id}" {disabled}>戻す</button>
<button type="button" class="quiet" data-ops="cancel-restore">キャンセル</button>"#,
                            warn = if b.current == "exists" {
                                "今の中身を置き換えます(今の状態はバージョン履歴に残ります)"
                            } else {
                                "コメント・バージョン履歴・メンバーごと作り直します"
                            },
                            id = escape(&b.id),
                            disabled = if self.busy { "disabled" } else { "" },
                        )
                    } else {
                        format!(
                            r#"<button type="button" data-ops="ask-restore" data-id="{}">戻す</button>"#,
                            escape(&b.id)
                        )
                    };
                    rows.push_str(&format!(
                        r#"<li><div class="info"><a class="title" href="?room={url}" translate="no">{title}</a>
<span class="muted small">{owner}{ago} · コメント {comments} · バージョン {versions} · {size}</span></div>
<span class="state" data-state="{cur}">{state}</span><span class="actions">{action}</span></li>"#,
                        url = http::encode(&b.id),
                        title = escape(&b.title),
                        owner = b
                            .owner
                            .as_deref()
                            .map(|o| format!("@{} · ", escape(o)))
                            .unwrap_or_default(),
                        ago = escape(&crate::home::ago(b.updated)),
                        comments = b.comments,
                        versions = b.versions,
                        size = size(b.bytes as u64),
                        cur = escape(&b.current),
                    ));
                }
                format!(r#"<ul class="ops-boards">{rows}</ul>"#)
            }
        };
        format!(
            r#"<section class="ops-pick">
  <header><h3>{when} のバックアップから戻す</h3><span class="spacer"></span>
  <button type="button" class="quiet" data-ops="close-pick">閉じる</button></header>
  {list}
</section>"#,
            when = escape(&datetime(info.created)),
        )
    }
}

fn install(ops: &Shared) {
    let el = ops.borrow().el.clone();
    let o = ops.clone();
    let on_click = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |e: web_sys::MouseEvent| {
        let Some(target) = e.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        // 外側(暗い背景)を押したら閉じる。
        if target.id() == "ops" {
            close();
            return;
        }
        let Some(btn) = target.closest("[data-ops]").ok().flatten() else {
            return;
        };
        let action = btn.get_attribute("data-ops").unwrap_or_default();
        let id = btn.get_attribute("data-id").unwrap_or_default();
        on_action(&o, &action, id);
    });
    let _ = el.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref());
    on_click.forget();
    let on_key = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(|e: web_sys::KeyboardEvent| {
        if e.key() == "Escape" {
            close();
        }
    });
    let _ = el.add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref());
    on_key.forget();
}

fn on_action(ops: &Shared, action: &str, id: String) {
    match action {
        "close" => close(),
        "backup-now" => {
            {
                let mut o = ops.borrow_mut();
                o.busy = true;
                o.notice = None;
                o.render();
            }
            let ops = ops.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let res = http::send::<_, BackupInfo>("POST", "/admin/backups", &()).await;
                {
                    let mut o = ops.borrow_mut();
                    o.busy = false;
                    o.notice = Some(match res {
                        Ok(b) => (
                            true,
                            format!(
                                "バックアップを取りました(ボード {} 件・{})",
                                b.boards,
                                size(b.bytes)
                            ),
                        ),
                        Err(e) => (false, e.message()),
                    });
                }
                load_backups(ops).await;
            });
        }
        "pick" => {
            let Ok(bid) = id.parse::<u64>() else { return };
            let info = ops
                .borrow()
                .backups
                .as_ref()
                .and_then(|s| s.backups.iter().find(|b| b.id == bid).cloned());
            let Some(info) = info else { return };
            {
                let mut o = ops.borrow_mut();
                o.picking = Some((info, None));
                o.confirm = None;
                o.render();
            }
            let ops = ops.clone();
            wasm_bindgen_futures::spawn_local(load_pick(ops, bid));
        }
        "close-pick" => {
            let mut o = ops.borrow_mut();
            o.picking = None;
            o.confirm = None;
            o.render();
        }
        "ask-restore" => {
            let mut o = ops.borrow_mut();
            o.confirm = Some(id);
            o.render();
        }
        "cancel-restore" => {
            let mut o = ops.borrow_mut();
            o.confirm = None;
            o.render();
        }
        "restore" => {
            let Some(bid) = ops.borrow().picking.as_ref().map(|(b, _)| b.id) else {
                return;
            };
            {
                let mut o = ops.borrow_mut();
                o.busy = true;
                o.render();
            }
            let ops = ops.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let res = http::send::<_, RestoreResult>(
                    "POST",
                    &format!("/admin/backups/{bid}/boards/{}/restore", http::encode(&id)),
                    &(),
                )
                .await;
                {
                    let mut o = ops.borrow_mut();
                    o.busy = false;
                    o.confirm = None;
                    o.notice = Some(match res {
                        Ok(r) if r.recreated => (
                            true,
                            "ボードを作り直しました。オーナーのボード一覧に戻っています。".into(),
                        ),
                        Ok(_) => (
                            true,
                            "ボードの中身を戻しました。戻す前の状態はバージョン履歴にあります。"
                                .into(),
                        ),
                        Err(e) => (false, e.message()),
                    });
                    o.render();
                }
                load_pick(ops, bid).await;
            });
        }
        _ => {}
    }
}

async fn load_pick(ops: Shared, bid: u64) {
    let res = http::get::<Vec<BackupBoard>>(&format!("/admin/backups/{bid}/boards")).await;
    let mut o = ops.borrow_mut();
    match res {
        Ok(list) => {
            if let Some((info, boards)) = o.picking.as_mut()
                && info.id == bid
            {
                *boards = Some(list);
            }
        }
        Err(e) => {
            o.picking = None;
            o.notice = Some((false, e.message()));
        }
    }
    o.render();
}
