//! GitHub・Jira の課題カード。
//!
//! 課題の URL(1 行に 1 つ)を貼るとカードを置き、サーバー(`/api/boards/{id}/issues`)に
//! 状態を引いてもらって書き込む。ボードを開いている間は [`REFRESH_MS`] ごとに引き直し、
//! 状態が変わったカードだけ書き換える(全員に同期される)。カードの形は [`board_core::issue`]。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::api::{IssueLookup, IssueProvider, IssueQuery};
use board_core::issue::{self, CARD_SIZE};
use board_core::{Element, ElementId, Vec2};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use super::App;
use crate::http;

/// 開いている間に引き直す間隔。
const REFRESH_MS: i32 = 2 * 60 * 1000;
/// カードを縦に並べるときの間隔。
const GAP: f32 = 24.0;

impl App {
    /// 課題カードの一覧(ID と URL)。
    fn issue_cards(&self, only: Option<&[ElementId]>) -> Vec<(ElementId, String)> {
        self.board
            .elements()
            .iter()
            .filter_map(|e| match e {
                Element::Shape(s) if issue::is_issue_card(s) => Some((s.id, s.meta.link.clone())),
                _ => None,
            })
            .filter(|(id, _)| only.is_none_or(|ids| ids.contains(id)))
            .collect()
    }

    /// 引いた結果をカードに書く。
    fn apply_issues(&mut self, cards: &[(ElementId, String)], results: &[IssueLookup]) {
        let mut changed = false;
        for (id, url) in cards {
            let Some(r) = results.iter().find(|r| &r.url == url) else {
                continue;
            };
            changed |= self
                .board
                .update(*id, |e| match e {
                    Element::Shape(s) => match (&r.issue, &r.error) {
                        (Some(info), _) => issue::apply(s, info),
                        (None, Some(err)) => issue::mark_error(s, err),
                        _ => false,
                    },
                    _ => false,
                })
                .unwrap_or(false);
        }
        if changed {
            // 同期による書き換えは、利用者の操作と別の Undo 単位にする。
            self.board.end_gesture();
            self.dirty = true;
        }
    }

    /// 課題の URL をカードにして置く。
    pub(super) fn insert_issue_cards(&mut self, urls: &[String]) -> Vec<ElementId> {
        if !self.can_edit() {
            return Vec::new();
        }
        let target = self
            .cursor_world
            .unwrap_or_else(|| self.camera.screen_to_world(self.camera.viewport * 0.5));
        let total = urls.len() as f32 * (CARD_SIZE.y + GAP) - GAP;
        let mut pos = target - Vec2::new(CARD_SIZE.x * 0.5, total * 0.5);
        self.board.end_gesture();
        let mut ids = Vec::new();
        for url in urls {
            let Some(r) = issue::parse_url(url) else {
                continue;
            };
            let id = self.board.alloc_id();
            self.board.push(issue::placeholder(id, pos, url, &r));
            ids.push(id);
            pos.y += CARD_SIZE.y + GAP;
        }
        self.board.end_gesture();
        self.editor.set_selection(ids.clone());
        self.dirty = true;
        ids
    }
}

/// カードの状態を引き直す(`only` が `None` ならボード上の全部)。
pub(super) fn refresh(app: &Rc<RefCell<App>>, only: Option<Vec<ElementId>>) {
    let (cards, path) = {
        let a = app.borrow();
        if !a.can_edit() {
            return;
        }
        let Some(room) = a.room.clone() else { return };
        let key = a.chrome.as_ref().and_then(|c| c.borrow().key.clone());
        let cards = a.issue_cards(only.as_deref());
        (cards, http::board_path(&room, key.as_deref(), "/issues"))
    };
    if cards.is_empty() {
        return;
    }
    let mut urls: Vec<String> = cards.iter().map(|(_, u)| u.clone()).collect();
    urls.sort();
    urls.dedup();
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        match http::send::<_, Vec<IssueLookup>>("POST", &path, &IssueQuery { urls }).await {
            Ok(results) => app.borrow_mut().apply_issues(&cards, &results),
            Err(e) => {
                log::info!("課題の状態を引けません: {}", e.message());
                let results: Vec<IssueLookup> = cards
                    .iter()
                    .map(|(_, url)| IssueLookup {
                        url: url.clone(),
                        issue: None,
                        error: Some(e.message()),
                    })
                    .collect();
                app.borrow_mut().apply_issues(&cards, &results);
            }
        }
    });
}

/// 貼り付けた文字が課題の URL だけならカードにする。
pub(super) fn paste(app: &Rc<RefCell<App>>, text: &str) -> bool {
    let Some(urls) = issue::urls_in_paste(text) else {
        return false;
    };
    let ids = app.borrow_mut().insert_issue_cards(&urls);
    if ids.is_empty() {
        return false;
    }
    refresh(app, Some(ids));
    true
}

pub(super) fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    {
        let mut a = app.borrow_mut();
        for p in [IssueProvider::Github, IssueProvider::Jira] {
            let png = provider_icon(p);
            if !a.renderer.register_icon(&issue::icon_name(p), &png) {
                log::warn!("課題カードのアイコンを登録できません");
            }
        }
    }
    let window = app.borrow().window.clone();
    let tick = {
        let app = app.clone();
        Closure::<dyn FnMut()>::new(move || {
            let hidden = app.borrow().document.hidden();
            if !hidden {
                refresh(&app, None);
            }
        })
    };
    window
        .set_interval_with_callback_and_timeout_and_arguments_0(
            tick.as_ref().unchecked_ref(),
            REFRESH_MS,
        )
        .map_err(super::js_err)?;
    tick.forget();
    // 開いた直後にも一度(接続して中身が届くのを少し待つ)。
    let first = {
        let app = app.clone();
        Closure::once_into_js(move || refresh(&app, None))
    };
    let _ =
        window.set_timeout_with_callback_and_timeout_and_arguments_0(first.unchecked_ref(), 3000);
    Ok(())
}

/// カードの左に描く小さな印(GitHub は黒い丸、Jira は青いひし形)。
fn provider_icon(p: IssueProvider) -> Vec<u8> {
    const N: u32 = 64;
    let img = image::RgbaImage::from_fn(N, N, |x, y| {
        let c = (N as f32 - 1.0) * 0.5;
        let (dx, dy) = (x as f32 - c, y as f32 - c);
        let inside = match p {
            IssueProvider::Github => {
                let r = (dx * dx + dy * dy).sqrt();
                // 輪の中に小さな丸(目印)。
                r <= 30.0 && !(14.0..=20.0).contains(&r)
            }
            IssueProvider::Jira => dx.abs() + dy.abs() <= 30.0,
        };
        match (inside, p) {
            (false, _) => image::Rgba([0, 0, 0, 0]),
            (true, IssueProvider::Github) => image::Rgba([0x24, 0x29, 0x2f, 0xff]),
            (true, IssueProvider::Jira) => {
                // 真ん中は白く抜く。
                if dx.abs() + dy.abs() <= 12.0 {
                    image::Rgba([0xff, 0xff, 0xff, 0xff])
                } else {
                    image::Rgba([0x26, 0x84, 0xff, 0xff])
                }
            }
        }
    });
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}
