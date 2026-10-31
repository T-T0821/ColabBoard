//! コメント(ピン・スレッド・一覧)とメンション通知の UI。
//!
//! コメントはサーバーの HTTP API で読み書きする。誰かが書き込むとサーバーから
//! `Message::Refresh(TOPIC_COMMENTS)` が届くので取り直す。
//! 「コメント」を押してからボードをクリックするとその位置(図形の上なら図形に結びつけて)に置ける。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use board_core::api::{
    Anchor, Comment, EditComment, MarkRead, NewComment, Notification, NotificationKind, Posted,
    Role,
};
use board_core::{ElementId, Vec2};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, HtmlTextAreaElement};

use super::{App, by_id, listen};
use crate::home::{ago, board_url};
use crate::http::{self, escape};

/// 通知を見直す間隔(ミリ秒)。
const NOTIFICATION_INTERVAL_MS: f64 = 30_000.0;

#[derive(Clone, Debug, PartialEq)]
enum Open {
    Thread(u64),
    Draft { world: Vec2, anchor: Option<Anchor> },
}

#[derive(Default)]
struct Inbox {
    comments: Option<Vec<Comment>>,
    notifications: Option<Vec<Notification>>,
    /// 投稿の結果(メンションが届かなかった人など)。
    message: Option<String>,
    /// 投稿したスレッドを開く。
    open: Option<u64>,
}

pub struct Comments {
    pins: HtmlElement,
    popover: HtmlElement,
    panel: HtmlElement,
    toggle: HtmlElement,
    bell: HtmlElement,
    notif_menu: HtmlElement,
    list: Vec<Comment>,
    notifications: Vec<Notification>,
    pin_els: HashMap<u64, HtmlElement>,
    open: Option<Open>,
    /// クリックでコメントを置く状態。
    placing: bool,
    show_resolved: bool,
    message: Option<String>,
    inbox: Rc<RefCell<Inbox>>,
    /// `?comment=` で開くスレッド(一覧が届いたら開く)。
    pending_open: Option<u64>,
    enabled: bool,
    notif_at: f64,
    rendered_popover: Option<String>,
}

impl Comments {
    pub fn new(document: &Document) -> Result<Self, String> {
        let pending_open = document
            .location()
            .and_then(|l| l.search().ok())
            .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
            .and_then(|p| p.get("comment"))
            .and_then(|c| c.parse().ok());
        Ok(Self {
            pins: by_id(document, "comment-pins")?,
            popover: by_id(document, "comment-popover")?,
            panel: by_id(document, "comments-panel")?,
            toggle: by_id(document, "comments-toggle")?,
            bell: by_id(document, "notif-button")?,
            notif_menu: by_id(document, "notif-menu")?,
            list: Vec::new(),
            notifications: Vec::new(),
            pin_els: HashMap::new(),
            open: None,
            placing: false,
            show_resolved: false,
            message: None,
            inbox: Rc::default(),
            pending_open,
            enabled: false,
            notif_at: f64::NEG_INFINITY,
            rendered_popover: None,
        })
    }

    fn root(&self, id: u64) -> Option<&Comment> {
        self.list.iter().find(|c| c.id == id && c.thread.is_none())
    }

    fn replies(&self, id: u64) -> impl Iterator<Item = &Comment> {
        self.list.iter().filter(move |c| c.thread == Some(id))
    }

    fn roots(&self) -> impl Iterator<Item = &Comment> {
        self.list.iter().filter(|c| c.thread.is_none())
    }
}

/// コメントの画面上の位置(ワールド座標)。要素に結びついていれば要素に付いていく。
fn world_pos(c: &Comment, board: &board_core::Board) -> Vec2 {
    if let Some(a) = &c.anchor
        && let Ok(raw) = u64::from_str_radix(&a.element, 16)
        && let Some(e) = board.get(ElementId(raw))
    {
        return e.bounds().min + Vec2::new(a.dx, a.dy);
    }
    Vec2::new(c.x, c.y)
}

fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

/// 本文を HTML に。`@ログイン名` を強調する。
fn body_html(body: &str) -> String {
    let mut out = String::new();
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push_str("<br>");
        }
        let mut rest = line;
        while let Some(at) = rest.find('@') {
            out.push_str(&escape(&rest[..at]));
            let tail = &rest[at + 1..];
            let len = tail
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
                .count();
            if len >= 3 {
                out.push_str(&format!(
                    r#"<span class="mention">@{}</span>"#,
                    escape(&tail[..len])
                ));
            } else {
                out.push('@');
                out.push_str(&escape(&tail[..len]));
            }
            rest = &tail[len..];
        }
        out.push_str(&escape(rest));
    }
    out
}

impl App {
    fn comment_ctx(&self) -> Option<(String, Option<String>, Option<String>, Role)> {
        let c = self.chrome.as_ref()?.borrow();
        Some((
            c.room.clone(),
            c.key.clone(),
            c.me.as_ref().map(|m| m.login.clone()),
            c.role(),
        ))
    }

    /// コメントの一覧を取り直す。
    pub(super) fn fetch_comments(&self) {
        let Some((room, key, me, _)) = self.comment_ctx() else {
            return;
        };
        let inbox = self.comments.inbox.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match http::get::<Vec<Comment>>(&http::board_path(&room, key.as_deref(), "/comments"))
                .await
            {
                Ok(list) => inbox.borrow_mut().comments = Some(list),
                Err(e) => log::warn!("コメントを取得できません: {}", e.message()),
            }
            if me.is_some()
                && let Ok(n) = http::get::<Vec<Notification>>("/notifications").await
            {
                inbox.borrow_mut().notifications = Some(n);
            }
        });
    }

    fn fetch_notifications(&self) {
        let inbox = self.comments.inbox.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(n) = http::get::<Vec<Notification>>("/notifications").await {
                inbox.borrow_mut().notifications = Some(n);
            }
        });
    }

    /// 毎フレーム: 届いた一覧を反映する。
    pub(super) fn comments_frame(&mut self, now: f64) {
        let ctx = self.comment_ctx();
        let enabled = ctx.is_some();
        if enabled != self.comments.enabled {
            self.comments.enabled = enabled;
            if let Some(body) = self.document.body() {
                let _ = body.set_attribute("data-comments", if enabled { "on" } else { "off" });
            }
            if enabled {
                self.fetch_comments();
                self.comments.notif_at = now;
            }
        }
        if !enabled {
            return;
        }
        let logged_in = ctx.as_ref().is_some_and(|c| c.2.is_some());
        if logged_in && now - self.comments.notif_at > NOTIFICATION_INTERVAL_MS {
            self.comments.notif_at = now;
            self.fetch_notifications();
        }
        let mut inbox = self.comments.inbox.borrow_mut();
        let list = inbox.comments.take();
        let notifications = inbox.notifications.take();
        let message = inbox.message.take();
        let open = inbox.open.take();
        drop(inbox);
        let mut changed = false;
        if let Some(list) = list {
            self.comments.list = list;
            changed = true;
            if let Some(Open::Thread(id)) = self.comments.open
                && self.comments.root(id).is_none()
            {
                self.comments.open = None;
            }
            if let Some(id) = self.comments.pending_open.take() {
                self.open_thread(id, true);
            }
        }
        if let Some(open) = open {
            self.comments.open = Some(Open::Thread(open));
            changed = true;
        }
        if message.is_some() {
            self.comments.message = message;
            changed = true;
        }
        if let Some(n) = notifications {
            self.comments.notifications = n;
            self.render_bell();
        }
        if changed {
            self.render_comments();
            self.dirty = true;
        }
    }

    fn render_bell(&self) {
        let unread = self
            .comments
            .notifications
            .iter()
            .filter(|n| !n.read)
            .count();
        let c = &self.comments;
        c.bell.set_inner_html(&format!(
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 16V11a6 6 0 0112 0v5l2 2H4zM10 20a2 2 0 004 0"/></svg>{}"#,
            if unread > 0 {
                format!(r#"<span class="count">{}</span>"#, unread.min(99))
            } else {
                String::new()
            }
        ));
        let _ = c
            .bell
            .set_attribute("aria-label", &format!("通知({unread} 件の未読)"));
        if c.notif_menu.hidden() {
            return;
        }
        let mut html = String::from(
            r#"<header class="row"><strong class="grow">通知</strong><button class="link small" data-action="read-all">すべて既読</button></header><ul>"#,
        );
        if c.notifications.is_empty() {
            html.push_str(r#"<li class="muted">通知はありません</li>"#);
        }
        for n in c.notifications.iter().take(30) {
            let what = match n.kind {
                NotificationKind::Mention => "あなたをメンションしました",
                NotificationKind::Reply => "スレッドに返信しました",
            };
            html.push_str(&format!(
                r#"<li><button class="notif {unread}" data-action="open-notif" data-id="{id}"><span><strong>{from}</strong> が {what}</span><span class="muted small">{title} · {ago}</span><span class="excerpt">{excerpt}</span></button></li>"#,
                unread = if n.read { "" } else { "unread" },
                id = n.id,
                from = escape(&n.from_name),
                title = escape(&n.board_title),
                ago = ago(n.created),
                excerpt = escape(&n.excerpt),
            ));
        }
        html.push_str("</ul>");
        c.notif_menu.set_inner_html(&html);
    }

    /// ピン・一覧・吹き出しを描き直す(中身が変わったとき)。
    fn render_comments(&mut self) {
        let unresolved = self.comments.roots().filter(|c| !c.resolved).count();
        self.comments.toggle.set_inner_html(&format!(
            r#"コメント{}"#,
            if unresolved > 0 {
                format!(r#" <span class="count">{unresolved}</span>"#)
            } else {
                String::new()
            }
        ));
        let _ = self.comments.toggle.set_attribute(
            "aria-pressed",
            if self.comments.placing || !self.comments.panel.hidden() {
                "true"
            } else {
                "false"
            },
        );

        // ピン
        let visible: Vec<(u64, String, bool, usize)> = self
            .comments
            .roots()
            .filter(|c| self.comments.show_resolved || !c.resolved)
            .map(|c| {
                (
                    c.id,
                    c.author_name.clone(),
                    c.resolved,
                    self.comments.replies(c.id).count(),
                )
            })
            .collect();
        let keep: Vec<u64> = visible.iter().map(|v| v.0).collect();
        self.comments.pin_els.retain(|id, el| {
            let k = keep.contains(id);
            if !k {
                el.remove();
            }
            k
        });
        for (id, name, resolved, replies) in visible {
            let el = match self.comments.pin_els.get(&id) {
                Some(el) => el.clone(),
                None => {
                    let Some(el) = self
                        .document
                        .create_element("button")
                        .ok()
                        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
                    else {
                        continue;
                    };
                    el.set_class_name("comment-pin");
                    let _ = el.set_attribute("data-thread", &id.to_string());
                    let _ = self.comments.pins.append_child(&el);
                    self.comments.pin_els.insert(id, el.clone());
                    el
                }
            };
            el.set_text_content(Some(&initial(&name)));
            let _ = el.set_attribute(
                "title",
                &format!(
                    "{name}{}",
                    if replies > 0 {
                        format!("(返信 {replies} 件)")
                    } else {
                        String::new()
                    }
                ),
            );
            let open = self.comments.open == Some(Open::Thread(id));
            let _ = el.set_attribute(
                "data-state",
                if resolved {
                    "resolved"
                } else if open {
                    "open"
                } else {
                    ""
                },
            );
        }

        self.render_panel();
        self.render_popover();
    }

    fn render_panel(&self) {
        let c = &self.comments;
        if c.panel.hidden() {
            return;
        }
        let mut html = format!(
            r#"<header class="row"><strong class="grow">コメント</strong><label class="check small"><input type="checkbox" data-action="show-resolved" {}/> 解決済みも表示</label><button class="quiet" data-action="close-panel" aria-label="閉じる">×</button></header>"#,
            if c.show_resolved { "checked" } else { "" }
        );
        let hint = if c.placing {
            "ボードをクリックするとコメントを置けます(Esc でやめる)"
        } else {
            "「＋ 追加」を押してからボードをクリックすると、その場所にコメントを置けます"
        };
        html.push_str(&format!(
            r#"<div class="row"><button class="primary" data-action="place" {}>＋ 追加</button><span class="muted small grow">{hint}</span></div><ul class="threads">"#,
            if c.placing { "aria-pressed=\"true\"" } else { "" }
        ));
        let mut roots: Vec<&Comment> = c
            .roots()
            .filter(|r| c.show_resolved || !r.resolved)
            .collect();
        roots.sort_by_key(|r| std::cmp::Reverse(r.created));
        if roots.is_empty() {
            html.push_str(r#"<li class="muted">コメントはまだありません</li>"#);
        }
        for r in roots {
            let n = c.replies(r.id).count();
            html.push_str(&format!(
                r#"<li><button class="thread {state}" data-action="open-thread" data-id="{id}"><span class="row"><strong>{name}</strong><span class="muted small">{ago}</span>{resolved}</span><span class="excerpt">{body}</span>{replies}</button></li>"#,
                state = if c.open == Some(Open::Thread(r.id)) { "open" } else { "" },
                id = r.id,
                name = escape(&r.author_name),
                ago = ago(r.created),
                resolved = if r.resolved { r#"<span class="badge">解決済み</span>"# } else { "" },
                body = escape(&r.body.chars().take(120).collect::<String>()),
                replies = if n > 0 { format!(r#"<span class="muted small">返信 {n} 件</span>"#) } else { String::new() },
            ));
        }
        html.push_str("</ul>");
        c.panel.set_inner_html(&html);
    }

    fn render_popover(&mut self) {
        let ctx = self.comment_ctx();
        let me = ctx.as_ref().and_then(|c| c.2.clone());
        let role = ctx.as_ref().map_or(Role::Viewer, |c| c.3);
        let can_post = me.is_some() && role.can_comment();
        let c = &self.comments;
        let login_link = || {
            let next = self
                .chrome
                .as_ref()
                .map(|ch| {
                    let ch = ch.borrow();
                    board_url(&ch.room, ch.key.as_deref())
                })
                .unwrap_or_default();
            format!(
                r#"<p class="muted small"><a href="?next={}">ログイン</a>するとコメントできます</p>"#,
                http::encode(&next)
            )
        };
        let message = c
            .message
            .as_ref()
            .map(|m| format!(r#"<p class="form-error small">{}</p>"#, escape(m)))
            .unwrap_or_default();
        let compose = |placeholder: &str, label: &str| {
            if can_post {
                format!(
                    r#"<form data-form="comment"><textarea name="body" rows="2" placeholder="{placeholder}" autofocus></textarea><div class="row"><span class="muted small grow">@ログイン名 でメンション · Ctrl/⌘+Enter で送信</span><button type="submit" class="primary">{label}</button></div></form>"#
                )
            } else if me.is_none() {
                login_link()
            } else {
                r#"<p class="muted small">このボードでは閲覧のみです</p>"#.to_string()
            }
        };
        let html = match &c.open {
            None => String::new(),
            Some(Open::Draft { .. }) => format!(
                r#"<header class="row"><strong class="grow">新しいコメント</strong><button class="quiet" data-action="close-popover" aria-label="閉じる">×</button></header>{message}{}"#,
                compose("コメントを書く", "送信")
            ),
            Some(Open::Thread(id)) => {
                let Some(root) = c.root(*id) else {
                    return;
                };
                let mut items = String::new();
                for (i, cm) in std::iter::once(root).chain(c.replies(*id)).enumerate() {
                    let mine = me.as_deref() == Some(cm.author.as_str());
                    let can_delete = mine || role == Role::Owner;
                    items.push_str(&format!(
                        r#"<li><div class="row"><span class="avatar">{ini}</span><strong translate="no">{name}</strong><span class="muted small grow">{ago}{edited}</span>{del}</div><div class="body" translate="no">{body}</div></li>"#,
                        ini = escape(&initial(&cm.author_name)),
                        name = escape(&cm.author_name),
                        ago = ago(cm.created),
                        edited = if cm.edited.is_some() { "(編集済み)" } else { "" },
                        del = if can_delete {
                            format!(
                                r#"<button class="quiet small" data-action="delete-comment" data-id="{}" title="{}">削除</button>"#,
                                cm.id,
                                if i == 0 { "スレッドごと削除" } else { "削除" }
                            )
                        } else {
                            String::new()
                        },
                        body = body_html(&cm.body),
                    ));
                }
                let resolve = if can_post {
                    format!(
                        r#"<button class="small" data-action="resolve" data-id="{}">{}</button>"#,
                        root.id,
                        if root.resolved { "再開" } else { "解決" }
                    )
                } else {
                    String::new()
                };
                format!(
                    r#"<header class="row"><strong class="grow">{title}</strong>{resolve}<button class="quiet" data-action="close-popover" aria-label="閉じる">×</button></header><ul class="thread-items">{items}</ul>{message}{compose}"#,
                    title = if root.resolved {
                        "解決済みのスレッド"
                    } else {
                        "スレッド"
                    },
                    compose = compose("返信する", "返信"),
                )
            }
        };
        // 入力中の文字を消さないよう、変わったときだけ描き直す。
        if self.comments.rendered_popover.as_deref() == Some(html.as_str()) {
            return;
        }
        let draft = self
            .comments
            .popover
            .query_selector("textarea")
            .ok()
            .flatten()
            .and_then(|t| t.dyn_into::<HtmlTextAreaElement>().ok())
            .map(|t| t.value());
        self.comments.popover.set_inner_html(&html);
        self.comments.popover.set_hidden(html.is_empty());
        if let Some(t) = self
            .comments
            .popover
            .query_selector("textarea")
            .ok()
            .flatten()
            .and_then(|t| t.dyn_into::<HtmlTextAreaElement>().ok())
        {
            if let Some(d) = draft {
                t.set_value(&d);
            }
            let _ = t.focus();
        }
        self.comments.rendered_popover = Some(html);
        self.layout_comments();
    }

    /// カメラに合わせてピンと吹き出しを動かす。
    pub(super) fn layout_comments(&self) {
        if !self.comments.enabled {
            return;
        }
        let rect = self.canvas.get_bounding_client_rect();
        let dpr = self.window.device_pixel_ratio() as f32;
        let origin = Vec2::new(rect.left() as f32, rect.top() as f32);
        let to_screen = |w: Vec2| self.camera.world_to_screen(w) / dpr + origin;
        for (id, el) in &self.comments.pin_els {
            let Some(c) = self.comments.root(*id) else {
                continue;
            };
            let s = to_screen(world_pos(c, &self.board));
            let _ = el
                .style()
                .set_property("transform", &format!("translate({}px, {}px)", s.x, s.y));
        }
        let anchor = match &self.comments.open {
            Some(Open::Thread(id)) => self.comments.root(*id).map(|c| world_pos(c, &self.board)),
            Some(Open::Draft { world, .. }) => Some(*world),
            None => None,
        };
        if let Some(w) = anchor {
            let s = to_screen(w);
            let vw = self
                .window
                .inner_width()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let vh = self
                .window
                .inner_height()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let pw = self.comments.popover.offset_width() as f32;
            let ph = self.comments.popover.offset_height() as f32;
            let x = (s.x + 20.0).min(vw - pw - 8.0).max(8.0);
            let y = (s.y - 12.0).min(vh - ph - 8.0).max(8.0);
            let style = self.comments.popover.style();
            let _ = style.set_property("left", &format!("{x}px"));
            let _ = style.set_property("top", &format!("{y}px"));
        }
    }

    /// コメントを置く状態なら、クリックした位置に下書きを開く。扱ったら true。
    pub(super) fn comment_pointer_down(&mut self, screen: Vec2) -> bool {
        if !self.comments.placing {
            return false;
        }
        let world = self.camera.screen_to_world(screen);
        let anchor = self.board.hit_test(world, 0.0).map(|e| {
            let min = e.bounds().min;
            Anchor {
                element: format!("{:x}", e.id().0),
                dx: world.x - min.x,
                dy: world.y - min.y,
            }
        });
        self.comments.placing = false;
        self.comments.message = None;
        self.comments.open = Some(Open::Draft { world, anchor });
        self.set_cursor("default");
        self.render_comments();
        true
    }

    fn open_thread(&mut self, id: u64, center: bool) {
        let Some(c) = self.comments.root(id) else {
            return;
        };
        let resolved = c.resolved;
        let w = world_pos(c, &self.board);
        if resolved {
            self.comments.show_resolved = true;
        }
        self.comments.open = Some(Open::Thread(id));
        self.comments.message = None;
        if center {
            let target = self.camera.viewport * 0.5;
            let now = self.camera.world_to_screen(w);
            self.camera.pan_by_screen(target - now);
        }
        self.render_comments();
        self.dirty = true;
    }

    /// Esc: 置く状態や吹き出しを閉じる。閉じたら true。
    pub(super) fn comments_escape(&mut self) -> bool {
        if !self.comments.notif_menu.hidden() {
            self.comments.notif_menu.set_hidden(true);
            return true;
        }
        if self.comments.placing || self.comments.open.is_some() {
            self.comments.placing = false;
            self.comments.open = None;
            self.render_comments();
            return true;
        }
        false
    }

    fn submit_comment(&mut self, body: String) {
        let Some((room, key, _, _)) = self.comment_ctx() else {
            return;
        };
        let req = match &self.comments.open {
            Some(Open::Draft { world, anchor }) => NewComment {
                body,
                x: world.x,
                y: world.y,
                anchor: anchor.clone(),
                parent: None,
            },
            Some(Open::Thread(id)) => NewComment {
                body,
                x: 0.0,
                y: 0.0,
                anchor: None,
                parent: Some(*id),
            },
            None => return,
        };
        let inbox = self.comments.inbox.clone();
        self.comments.rendered_popover = None;
        let path = http::board_path(&room, key.as_deref(), "/comments");
        wasm_bindgen_futures::spawn_local(async move {
            match http::send::<_, Posted>("POST", &path, &req).await {
                Ok(p) => {
                    let mut ib = inbox.borrow_mut();
                    ib.open = Some(p.comment.thread.unwrap_or(p.comment.id));
                    ib.message = (!p.unreachable.is_empty()).then(|| {
                        format!(
                            "{} はこのボードを開けないため通知していません",
                            p.unreachable
                                .iter()
                                .map(|l| format!("@{l}"))
                                .collect::<Vec<_>>()
                                .join("、")
                        )
                    });
                }
                Err(e) => inbox.borrow_mut().message = Some(e.message()),
            }
            if let Ok(list) = http::get::<Vec<Comment>>(&path).await {
                inbox.borrow_mut().comments = Some(list);
            }
        });
    }

    fn comment_request(&self, method: &'static str, id: u64, body: Option<EditComment>) {
        let Some((room, key, _, _)) = self.comment_ctx() else {
            return;
        };
        let inbox = self.comments.inbox.clone();
        let one = http::board_path(&room, key.as_deref(), &format!("/comments/{id}"));
        let all = http::board_path(&room, key.as_deref(), "/comments");
        wasm_bindgen_futures::spawn_local(async move {
            let res = match body {
                Some(b) => http::send::<_, Comment>(method, &one, &b).await.map(|_| ()),
                None => http::send_empty(method, &one).await,
            };
            if let Err(e) = res {
                inbox.borrow_mut().message = Some(e.message());
            }
            if let Ok(list) = http::get::<Vec<Comment>>(&all).await {
                inbox.borrow_mut().comments = Some(list);
            }
        });
    }

    fn on_comment_action(&mut self, action: &str, el: &Element) {
        let id = el
            .get_attribute("data-id")
            .and_then(|i| i.parse::<u64>().ok());
        match action {
            "close-popover" => {
                self.comments.open = None;
                self.render_comments();
            }
            "close-panel" => {
                self.comments.panel.set_hidden(true);
                self.comments.placing = false;
                self.render_comments();
            }
            "place" => {
                self.comments.placing = !self.comments.placing;
                self.comments.open = None;
                self.set_cursor(if self.comments.placing {
                    "crosshair"
                } else {
                    "default"
                });
                self.render_comments();
            }
            "show-resolved" => {
                self.comments.show_resolved = !self.comments.show_resolved;
                self.render_comments();
                self.dirty = true;
            }
            "open-thread" => {
                if let Some(id) = id {
                    self.open_thread(id, true);
                }
            }
            "resolve" => {
                if let Some(id) = id {
                    let resolved = self.comments.root(id).is_some_and(|c| !c.resolved);
                    self.comment_request(
                        "PATCH",
                        id,
                        Some(EditComment {
                            body: None,
                            resolved: Some(resolved),
                        }),
                    );
                    if resolved {
                        self.comments.open = None;
                    }
                }
            }
            "delete-comment" => {
                if let Some(id) = id {
                    if self.comments.root(id).is_some() {
                        self.comments.open = None;
                    }
                    self.comment_request("DELETE", id, None);
                }
            }
            "read-all" => {
                for n in &mut self.comments.notifications {
                    n.read = true;
                }
                self.render_bell();
                wasm_bindgen_futures::spawn_local(async {
                    let _ = http::send::<_, serde_json::Value>(
                        "POST",
                        "/notifications/read",
                        &MarkRead { ids: None },
                    )
                    .await;
                });
            }
            "open-notif" => {
                let Some(n) = id.and_then(|id| {
                    self.comments
                        .notifications
                        .iter()
                        .find(|n| n.id == id)
                        .cloned()
                }) else {
                    return;
                };
                let ids = vec![n.id];
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = http::send::<_, serde_json::Value>(
                        "POST",
                        "/notifications/read",
                        &MarkRead { ids: Some(ids) },
                    )
                    .await;
                });
                if let Some(x) = self
                    .comments
                    .notifications
                    .iter_mut()
                    .find(|x| x.id == n.id)
                {
                    x.read = true;
                }
                self.comments.notif_menu.set_hidden(true);
                self.render_bell();
                let same = self.comment_ctx().is_some_and(|c| c.0 == n.board);
                if same {
                    self.open_thread(n.thread, true);
                } else {
                    let url = format!(
                        "{}&comment={}",
                        board_url(&n.board, n.key.as_deref()),
                        n.thread
                    );
                    let _ = self.window.location().set_search(&url);
                }
            }
            _ => {}
        }
    }
}

/// クリックされた `[data-action]` を探す。
fn action_of(e: &web_sys::Event) -> Option<(String, Element)> {
    let el = e
        .target()?
        .dyn_into::<Element>()
        .ok()?
        .closest("[data-action]")
        .ok()??;
    Some((el.get_attribute("data-action")?, el))
}

pub fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    let (toggle, panel, popover, pins, bell, menu) = {
        let a = app.borrow();
        let c = &a.comments;
        (
            c.toggle.clone(),
            c.panel.clone(),
            c.popover.clone(),
            c.pins.clone(),
            c.bell.clone(),
            c.notif_menu.clone(),
        )
    };
    {
        let app = app.clone();
        listen(
            toggle.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                let mut a = app.borrow_mut();
                let open = a.comments.panel.hidden();
                a.comments.panel.set_hidden(!open);
                a.comments.placing = false;
                a.render_comments();
            },
        )?;
    }
    for target in [&panel, &popover, &menu] {
        let app = app.clone();
        listen(
            target.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                if let Some((action, el)) = action_of(&e) {
                    app.borrow_mut().on_comment_action(&action, &el);
                }
            },
        )?;
    }
    {
        let app = app.clone();
        listen(
            pins.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                let Some(id) = e
                    .target()
                    .and_then(|t| t.dyn_into::<Element>().ok())
                    .and_then(|t| t.closest("[data-thread]").ok().flatten())
                    .and_then(|t| t.get_attribute("data-thread"))
                    .and_then(|t| t.parse::<u64>().ok())
                else {
                    return;
                };
                let mut a = app.borrow_mut();
                if a.comments.open == Some(Open::Thread(id)) {
                    a.comments.open = None;
                    a.render_comments();
                } else {
                    a.open_thread(id, false);
                }
            },
        )?;
    }
    {
        let app = app.clone();
        listen(
            popover.as_ref(),
            "submit",
            false,
            move |e: web_sys::Event| {
                e.prevent_default();
                submit_from(&app, &e);
            },
        )?;
    }
    {
        // Ctrl/⌘+Enter で送信。
        let app = app.clone();
        listen(
            popover.as_ref(),
            "keydown",
            false,
            move |e: web_sys::KeyboardEvent| {
                if e.key() == "Enter" && (e.ctrl_key() || e.meta_key()) && !e.is_composing() {
                    e.prevent_default();
                    submit_from(&app, &e);
                } else if e.key() == "Escape" {
                    app.borrow_mut().comments_escape();
                }
            },
        )?;
    }
    {
        let app = app.clone();
        listen(
            bell.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                let a = app.borrow();
                let open = a.comments.notif_menu.hidden();
                a.comments.notif_menu.set_hidden(!open);
                if open {
                    a.render_bell();
                    a.fetch_notifications();
                }
            },
        )?;
    }
    let _ = doc;
    Ok(())
}

fn submit_from(app: &Rc<RefCell<App>>, e: &web_sys::Event) {
    let Some(form) = e
        .target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .and_then(|t| t.closest("form").ok().flatten())
    else {
        return;
    };
    let Some(text) = form
        .query_selector("textarea")
        .ok()
        .flatten()
        .and_then(|t| t.dyn_into::<HtmlTextAreaElement>().ok())
    else {
        return;
    };
    let body = text.value();
    if body.trim().is_empty() {
        return;
    }
    text.set_value("");
    app.borrow_mut().submit_comment(body);
}
