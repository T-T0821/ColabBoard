//! フォローとプレゼン: 他の参加者の表示範囲に自分の画面を合わせる。
//!
//! 各自は表示範囲(`protocol::View`)を送り合う。参加者一覧の名前を押すとその人をフォローし、
//! 誰かが「プレゼン開始」すると全員が自動でフォローする。自分でパン/ズームするとフォローをやめる。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use board_core::protocol::{Message, View};
use board_core::{Camera, Vec2};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement};

use super::{App, by_id, listen, on_click};

/// 表示範囲を送る最短間隔(ミリ秒)。
const VIEW_INTERVAL_MS: f64 = 80.0;

pub struct Follow {
    document: Document,
    banner: HtmlElement,
    banner_text: HtmlElement,
    banner_button: HtmlElement,
    present_button: HtmlElement,
    peer_list: HtmlElement,
    views: HashMap<u32, View>,
    following: Option<u32>,
    /// プレゼンで自動的にフォローしているか(プレゼンが終わったら外す)。
    auto: bool,
    /// 自分でフォローをやめたプレゼンター(勝手に戻らないように)。
    dismissed: Option<u32>,
    /// 最後に合わせたカメラ。ずれていたら自分で動かしたとみなす。
    applied: Option<(Vec2, f32)>,
    presenting: bool,
    /// フォロー先の最新の表示範囲。毎フレーム少しずつ近づける(届く間隔より滑らかに動かす)。
    target: Option<View>,
    sent: Option<View>,
    sent_at: f64,
    last_banner: Option<(String, &'static str, &'static str)>,
    last_peers: Vec<(u32, String, u32)>,
}

impl Follow {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            document: document.clone(),
            banner: by_id(document, "follow-banner")?,
            banner_text: by_id(document, "follow-text")?,
            banner_button: by_id(document, "follow-action")?,
            present_button: by_id(document, "present")?,
            peer_list: by_id(document, "peer-list")?,
            views: HashMap::new(),
            following: None,
            auto: false,
            dismissed: None,
            applied: None,
            presenting: false,
            target: None,
            sent: None,
            sent_at: 0.0,
            last_banner: None,
            last_peers: Vec::new(),
        })
    }

    fn presenter(&self) -> Option<u32> {
        let mut ps: Vec<u32> = self
            .views
            .values()
            .filter(|v| v.presenting)
            .map(|v| v.peer)
            .collect();
        ps.sort_unstable();
        ps.first().copied()
    }
}

/// 1 フレームで残りの何割だけ近づけるか。
const SMOOTHING: f32 = 0.35;

/// `camera` を表示範囲 `v` へ少し近づける。十分近ければぴったり合わせて `true`。
fn approach_view(camera: &mut Camera, v: &View) -> bool {
    let mut target = *camera;
    apply_view(&mut target, v);
    let center = |c: &Camera| c.origin + c.viewport * 0.5 / c.zoom;
    let (c0, c1) = (center(camera), center(&target));
    let close =
        (c1 - c0).length() * target.zoom < 0.5 && (target.zoom / camera.zoom - 1.0).abs() < 1e-3;
    if close {
        *camera = target;
        return true;
    }
    let zoom = (camera.zoom.ln() + (target.zoom.ln() - camera.zoom.ln()) * SMOOTHING).exp();
    let c = c0.lerp(c1, SMOOTHING);
    camera.zoom = zoom;
    camera.origin = c - camera.viewport * 0.5 / zoom;
    false
}

fn apply_view(camera: &mut Camera, v: &View) {
    camera.fit(
        Vec2::new(v.min_x, v.min_y),
        Vec2::new(v.max_x, v.max_y),
        0.0,
    );
}

impl App {
    pub(super) fn peer_name(&self, peer: u32) -> String {
        self.cursors
            .get(peer)
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("参加者 {peer}"))
    }

    fn start_follow(&mut self, peer: u32, auto: bool) {
        let f = &mut self.follow;
        f.following = Some(peer);
        f.auto = auto;
        if !auto {
            f.dismissed = None;
        }
        if let Some(v) = f.views.get(&peer).copied() {
            apply_view(&mut self.camera, &v);
            self.follow.applied = Some((self.camera.origin, self.camera.zoom));
            self.dirty = true;
        }
    }

    /// 自分が今プレゼン中か。
    pub(super) fn presenting(&self) -> bool {
        self.follow.presenting
    }

    /// プレゼンを始める/やめる(スライド表示から使う)。共同編集でなければ何もしない。
    pub(super) fn set_presenting(&mut self, on: bool) -> bool {
        if self.net.is_none() {
            return false;
        }
        self.follow.presenting = on;
        if on {
            self.stop_follow();
        }
        true
    }

    fn stop_follow(&mut self) {
        let f = &mut self.follow;
        f.target = None;
        if f.auto {
            f.dismissed = f.following;
        }
        f.following = None;
        f.auto = false;
        f.applied = None;
    }

    /// 他の参加者の表示範囲が届いた。
    pub(super) fn on_view(&mut self, v: View) {
        if !v.is_valid() {
            return;
        }
        let was_presenting = self.follow.views.get(&v.peer).is_some_and(|o| o.presenting);
        self.follow.views.insert(v.peer, v);
        if was_presenting && !v.presenting && self.follow.dismissed == Some(v.peer) {
            self.follow.dismissed = None;
        }
        if self.follow.following == Some(v.peer) {
            if self.follow.auto && !v.presenting {
                // プレゼンが終わった。
                self.stop_follow();
                self.follow.dismissed = None;
                return;
            }
            self.follow.target = Some(v);
            self.dirty = true;
        } else if v.presenting
            && !self.follow.presenting
            && self.follow.following.is_none()
            && self.follow.dismissed != Some(v.peer)
        {
            self.start_follow(v.peer, true);
        }
    }

    pub(super) fn on_peer_leave(&mut self, peer: u32) {
        self.follow.views.remove(&peer);
        if self.follow.following == Some(peer) {
            self.stop_follow();
        }
        if self.follow.dismissed == Some(peer) {
            self.follow.dismissed = None;
        }
    }

    /// 接続が切れたら他の人の範囲は忘れる。
    pub(super) fn follow_reset(&mut self) {
        self.follow.views.clear();
        self.stop_follow();
        self.follow.dismissed = None;
        self.follow.sent = None;
    }

    /// 毎フレーム: 自分で動かしたらフォローをやめ、表示範囲を送り、表示を更新する。
    pub(super) fn follow_frame(&mut self, now: f64) {
        if self.follow.following.is_some()
            && self
                .follow
                .applied
                .is_some_and(|a| a != (self.camera.origin, self.camera.zoom))
        {
            self.stop_follow();
        }
        if self.follow.following.is_some()
            && let Some(v) = self.follow.target
        {
            if approach_view(&mut self.camera, &v) {
                self.follow.target = None;
            }
            self.follow.applied = Some((self.camera.origin, self.camera.zoom));
            self.dirty = true;
        }

        let online = self
            .net
            .as_ref()
            .is_some_and(|n| n.status == crate::net::Status::Online && n.is_open());
        if online {
            let (min, max) = self.camera.visible_world_rect();
            let view = View {
                peer: 0,
                min_x: min.x,
                min_y: min.y,
                max_x: max.x,
                max_y: max.y,
                presenting: self.follow.presenting,
            };
            let urgent = self.follow.sent.map(|s| s.presenting) != Some(view.presenting);
            if self.follow.sent != Some(view)
                && (urgent || now - self.follow.sent_at >= VIEW_INTERVAL_MS)
                && view.is_valid()
                && let Some(net) = self.net.as_ref()
            {
                net.send(&Message::View(view));
                self.follow.sent = Some(view);
                self.follow.sent_at = now;
            }
        }

        self.update_follow_ui();
    }

    fn update_follow_ui(&mut self) {
        // 帯: (文言, ボタン, 状態)
        let banner = if self.follow.presenting {
            Some((
                "プレゼン中: 参加者はあなたの画面に追従しています".to_string(),
                "終了",
                "presenting",
            ))
        } else if let Some(peer) = self.follow.following {
            Some((
                format!("{} さんの画面をフォロー中", self.peer_name(peer)),
                "やめる",
                "following",
            ))
        } else {
            self.follow.presenter().map(|peer| {
                (
                    format!("{} さんがプレゼン中", self.peer_name(peer)),
                    "見る",
                    "available",
                )
            })
        };
        if banner != self.follow.last_banner {
            match &banner {
                Some((text, action, state)) => {
                    self.follow.banner_text.set_text_content(Some(text));
                    self.follow.banner_button.set_text_content(Some(action));
                    let _ = self.follow.banner.set_attribute("data-state", state);
                    let _ = self.follow.banner.remove_attribute("hidden");
                }
                None => {
                    let _ = self.follow.banner.set_attribute("hidden", "");
                }
            }
            self.follow
                .present_button
                .set_text_content(Some(if self.follow.presenting {
                    "プレゼン終了"
                } else {
                    "プレゼン開始"
                }));
            self.follow.last_banner = banner;
        }

        // 参加者一覧(名前を押すとフォロー)。
        let mut peers: Vec<(u32, String, u32)> = self
            .cursors
            .peers()
            .map(|p| (p.peer, p.name.clone(), p.color))
            .collect();
        peers.sort_by_key(|p| p.0);
        if peers != self.follow.last_peers {
            self.follow.peer_list.set_inner_html("");
            if peers.is_empty() {
                self.follow
                    .peer_list
                    .set_text_content(Some("ほかに参加者はいません"));
            }
            for (peer, name, color) in &peers {
                let Ok(btn) = self.follow.document.create_element("button") else {
                    continue;
                };
                let _ = btn.set_attribute("data-peer", &peer.to_string());
                let _ = btn.set_attribute("title", "この人の画面をフォロー");
                let _ = btn.set_attribute("style", &format!("--c:#{:06x}", color & 0xff_ffff));
                btn.set_class_name("peer");
                btn.set_text_content(Some(name));
                let _ = self.follow.peer_list.append_child(&btn);
            }
            self.follow.last_peers = peers;
        }
    }

    fn toggle_present(&mut self) {
        if self.net.is_none() {
            return;
        }
        self.follow.presenting = !self.follow.presenting;
        if self.follow.presenting {
            self.stop_follow();
        }
    }

    fn follow_banner_action(&mut self) {
        if self.follow.presenting {
            self.follow.presenting = false;
        } else if self.follow.following.is_some() {
            self.stop_follow();
        } else if let Some(peer) = self.follow.presenter() {
            self.start_follow(peer, true);
        }
    }
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    on_click(doc, "present", app, |a| a.toggle_present())?;
    on_click(doc, "follow-action", app, |a| a.follow_banner_action())?;
    let list: HtmlElement = by_id(doc, "peer-list")?;
    let app = app.clone();
    listen(
        list.as_ref(),
        "click",
        false,
        move |e: web_sys::MouseEvent| {
            let peer = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|el| el.closest("[data-peer]").ok().flatten())
                .and_then(|el| el.get_attribute("data-peer"))
                .and_then(|p| p.parse::<u32>().ok());
            if let Some(peer) = peer {
                app.borrow_mut().start_follow(peer, false);
            }
        },
    )
}
