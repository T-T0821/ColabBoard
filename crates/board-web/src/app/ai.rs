//! AI による付箋の要約とグループ分け。サーバー(`/api/boards/{id}/ai/...`)が Claude API を呼ぶ。
//!
//! 対象は選択中の付箋(2 枚以上)、なければボード上の全付箋。
//! - 要約: 結果を新しい付箋にして、対象の右に置く
//! - グループ分け: 付箋をグループごとに並べ直し、各グループをフレームで囲んで見出しを付ける
//!
//! どちらも普通の編集として書くので、1 回の Undo で戻せて、全員に同期される。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::arrange::{Order, reorder};
use board_core::color::STICKY_COLORS;
use board_core::{ElementId, Shape, Vec2, tidy};
use js_sys::{Array, JSON, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Document, HtmlElement, RequestInit, Response};

use super::{App, by_id, js_err, on_click};

/// グループの見出し(フレームの上)に空ける高さ。
const HEADER: f32 = 48.0;
/// フレームの内側の余白。
const PAD: f32 = 32.0;

pub struct AiUi {
    status: HtmlElement,
    busy: bool,
}

impl AiUi {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            status: by_id(document, "ai-status")?,
            busy: false,
        })
    }

    fn set_status(&self, text: &str, state: &str) {
        self.status.set_text_content(Some(text));
        let _ = self.status.set_attribute("data-state", state);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Summarize,
    Cluster,
}

impl Action {
    fn path(self) -> &'static str {
        match self {
            Action::Summarize => "summarize",
            Action::Cluster => "cluster",
        }
    }
}

/// API の URL。`?server=ws://host` ならそのホスト、なければページと同じ場所。
/// 共有リンクの `?key=` はそのまま付ける。
pub(super) fn api_url(window: &web_sys::Window, room: &str, action: &str) -> Option<String> {
    let params = window
        .location()
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())?;
    let base = match params.get("server") {
        Some(s) => s
            .trim_end_matches('/')
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1),
        None => String::new(),
    };
    let mut url = format!("{base}/api/boards/{room}/ai/{action}");
    if let Some(key) = params.get("key") {
        url.push_str("?key=");
        url.push_str(&String::from(js_sys::encode_uri_component(&key)));
    }
    Some(url)
}

/// POST して JSON を受け取る。失敗したらサーバーのエラー文言を返す。
pub(super) async fn post_json(
    window: &web_sys::Window,
    url: &str,
    body: &str,
) -> Result<JsValue, String> {
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(body));
    let headers = web_sys::Headers::new().map_err(js_err)?;
    headers
        .set("Content-Type", "application/json")
        .map_err(js_err)?;
    init.set_headers(&headers);
    let resp: Response = JsFuture::from(window.fetch_with_str_and_init(url, &init))
        .await
        .map_err(|_| "サーバーにつながりません".to_string())?
        .dyn_into()
        .map_err(|_| "応答を読めません".to_string())?;
    let text = JsFuture::from(resp.text().map_err(js_err)?)
        .await
        .map_err(js_err)?
        .as_string()
        .unwrap_or_default();
    let value = JSON::parse(&text).unwrap_or(JsValue::NULL);
    if !resp.ok() {
        let msg = Reflect::get(&value, &"error".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| format!("エラー(HTTP {})", resp.status()));
        return Err(msg);
    }
    Ok(value)
}

fn parse_id(v: &JsValue) -> Option<ElementId> {
    v.as_string()
        .and_then(|h| u64::from_str_radix(&h, 16).ok())
        .map(ElementId)
}

/// `{"clusters":[{"title","ids"}]}` を読む。
fn parse_clusters(v: &JsValue) -> Vec<(String, Vec<ElementId>)> {
    let Ok(list) = Reflect::get(v, &"clusters".into()) else {
        return Vec::new();
    };
    Array::from(&list)
        .iter()
        .map(|c| {
            let title = Reflect::get(&c, &"title".into())
                .ok()
                .and_then(|t| t.as_string())
                .unwrap_or_default();
            let ids = Reflect::get(&c, &"ids".into())
                .map(|a| {
                    Array::from(&a)
                        .iter()
                        .filter_map(|i| parse_id(&i))
                        .collect()
                })
                .unwrap_or_default();
            (title, ids)
        })
        .collect()
}

/// 要約の付箋の高さ(文字量からおおまかに決める)。
fn summary_height(text: &str) -> f32 {
    let lines: usize = text
        .lines()
        .map(|l| l.chars().count().div_ceil(16).max(1))
        .sum();
    (lines as f32 * 28.0 + 64.0).clamp(200.0, 1600.0)
}

impl App {
    fn ai_targets(&self) -> Vec<ElementId> {
        tidy::targets(&self.board, self.editor.selection())
    }

    /// 要約を新しい付箋にして、対象の右に置く。
    fn apply_summary(&mut self, targets: &[ElementId], summary: &str) {
        let Some(b) = self.board.bounds_of(targets) else {
            return;
        };
        let text = format!("AI 要約\n{summary}");
        let id = self.board.alloc_id();
        let mut note = Shape::sticky(id, Vec2::new(b.max.x + 80.0, b.min.y), STICKY_COLORS[4]);
        note.size = Vec2::new(400.0, summary_height(&text));
        note.text = text;
        self.board.end_gesture();
        self.board.push(note);
        self.board.end_gesture();
        self.editor.set_selection(vec![id]);
        self.dirty = true;
    }

    /// グループごとに並べ直し、フレームで囲む。
    fn apply_clusters(&mut self, targets: &[ElementId], clusters: &[(String, Vec<ElementId>)]) {
        let Some(b) = self.board.bounds_of(targets) else {
            return;
        };
        let groups: Vec<Vec<(ElementId, Vec2)>> = clusters
            .iter()
            .map(|(_, ids)| {
                ids.iter()
                    .filter_map(|id| self.board.get(*id).map(|e| (*id, e.bounds().size())))
                    .collect()
            })
            .collect();
        let titles: Vec<&str> = clusters
            .iter()
            .zip(&groups)
            .filter(|(_, g)| !g.is_empty())
            .map(|((t, _), _)| t.as_str())
            .collect();
        // 付箋の格子が今の位置から始まるようにする(フレームと見出しは上にはみ出す)。
        let origin = b.min - Vec2::new(-PAD, HEADER + PAD);
        let (moves, rects) = tidy::groups(&groups, origin, HEADER + PAD);

        self.board.end_gesture();
        for (id, pos) in &moves {
            self.board.update(*id, |e| {
                let d = *pos - e.bounds().min;
                if d != Vec2::ZERO {
                    e.translate(d);
                }
            });
        }
        let mut frames = Vec::new();
        for (rect, title) in rects.iter().zip(titles) {
            let id = self.board.alloc_id();
            let min = rect.min - Vec2::splat(PAD);
            let mut frame = Shape::frame(id, min, rect.size() + Vec2::splat(PAD * 2.0));
            frame.text = title.to_string();
            self.board.push(frame);
            frames.push(id);
        }
        // フレームは付箋の後ろに置く。
        reorder(&mut self.board, &frames, Order::Back);
        self.board.end_gesture();
        self.editor.set_selection(frames);
        self.dirty = true;
    }
}

pub(super) fn start(app: &Rc<RefCell<App>>, action: Action) {
    let (window, url, targets, body) = {
        let a = app.borrow();
        if a.ai.busy {
            return;
        }
        let Some(room) = a.room.clone() else {
            a.ai.set_status("AI 機能は共同編集のボードでだけ使えます", "error");
            return;
        };
        let targets = a.ai_targets();
        if targets.len() < usize::from(action == Action::Cluster) + 1 {
            a.ai.set_status("付箋がありません", "error");
            return;
        }
        let Some(url) = api_url(&a.window, &room, action.path()) else {
            return;
        };
        let ids: Vec<String> = targets.iter().map(|id| format!("\"{:x}\"", id.0)).collect();
        let body = format!("{{\"ids\":[{}]}}", ids.join(","));
        (a.window.clone(), url, targets, body)
    };
    {
        let mut a = app.borrow_mut();
        a.ai.busy = true;
        a.ai.set_status(
            &format!("AI が {} 枚の付箋を読んでいます…", targets.len()),
            "busy",
        );
    }
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = post_json(&window, &url, &body).await;
        let mut a = app.borrow_mut();
        a.ai.busy = false;
        match result {
            Ok(v) if action == Action::Summarize => {
                let summary = Reflect::get(&v, &"summary".into())
                    .ok()
                    .and_then(|s| s.as_string())
                    .unwrap_or_default();
                a.apply_summary(&targets, &summary);
                a.ai.set_status("要約を付箋にしました", "done");
            }
            Ok(v) => {
                let clusters = parse_clusters(&v);
                a.apply_clusters(&targets, &clusters);
                a.ai.set_status(
                    &format!("{} 個のグループに分けました", clusters.len()),
                    "done",
                );
            }
            Err(e) => a.ai.set_status(&e, "error"),
        }
    });
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    {
        let app2 = app.clone();
        on_click(doc, "ai-summarize", app, move |_| {
            let app = app2.clone();
            // `on_click` は App を借りたまま呼ぶので、借用が終わってから始める。
            wasm_bindgen_futures::spawn_local(async move { start(&app, Action::Summarize) });
        })?;
    }
    {
        let app2 = app.clone();
        on_click(doc, "ai-cluster", app, move |_| {
            let app = app2.clone();
            wasm_bindgen_futures::spawn_local(async move { start(&app, Action::Cluster) });
        })?;
    }
    Ok(())
}
