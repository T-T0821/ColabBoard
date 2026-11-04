//! サーバーを通さない直接接続(P2P)の画面と、ボードとのつなぎ込み。
//!
//! - 上の「直接つなぐ」でパネルを開き、招待コード(リンクと QR コード)を作る → 相手が開くと
//!   応答コードが出る → それを貼るとつながる。接続そのものは [`crate::p2p`]。
//! - つながった相手とは yrs の更新を直接やり取りする。サーバーにもつながっている人は橋になり、
//!   サーバーにいない相手の編集をサーバーへ、サーバーからの編集をその相手へ回す
//!   ([`board_core::p2p::route`])。
//! - 悪用への備え: 使ってよいかはサーバー(チームの管理者)が決め、最後に聞いた答えを端末に覚える。
//!   招待・参加・接続・切断は操作ログに残す(中身は送らない。オフラインの間はためて後で送る)。
//!   相手を通報でき、通報も操作ログに残る。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::Vec2;
use board_core::api::{
    MAX_REPORT_NOTE_CHARS, P2pEvent, P2pEventKind, P2pPolicy, P2pReport, ReportReason,
};
use board_core::p2p::{self as core, Code, CodeKind, LINK_PARAM, PEER_ID_BASE, PeerMsg, Source};
use board_core::protocol::Presence;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, HtmlSelectElement, HtmlTextAreaElement};

use super::{App, by_id, listen};
use crate::http::{self, HttpError, escape};
use crate::net::Status;
use crate::p2p::{Event, LinkInfo, LinkState, P2p};

/// 端末に覚えておく、まだサーバーに送っていない記録。
const LOG_KEY: &str = "colabboard.p2p.pending";
/// ボードごとに最後に聞いた「使ってよいか」。
const POLICY_KEY: &str = "colabboard.p2p.policy.";
/// ためておく記録の上限(古いものから捨てる)。
const MAX_PENDING: usize = 200;
const PRESENCE_INTERVAL_MS: f64 = 50.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    room: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    event: Option<P2pEvent>,
    #[serde(default)]
    report: Option<P2pReport>,
}

pub struct P2pUi {
    key: Option<String>,
    policy: Option<P2pPolicy>,
    /// 招待中(接続の番号と、渡すリンク)。
    invite: Option<(u32, String)>,
    /// 招待されて作った応答コード。
    answer: Option<(u32, String)>,
    busy: bool,
    /// (メッセージ, エラーか)
    status: Option<(String, bool)>,
    /// 通報の画面を開いている相手。
    report_for: Option<u32>,
    last_version: Option<u64>,
    last_online: Option<bool>,
    presence_sent: Option<Option<Vec2>>,
    presence_at: f64,
    needs_render: bool,
    flushing: bool,
}

impl P2pUi {
    pub fn new(key: Option<String>) -> Self {
        Self {
            key,
            policy: None,
            invite: None,
            answer: None,
            busy: false,
            status: None,
            report_for: None,
            last_version: None,
            last_online: None,
            presence_sent: None,
            presence_at: 0.0,
            needs_render: true,
            flushing: false,
        }
    }

    fn allowed(&self) -> bool {
        self.policy.as_ref().is_none_or(|p| p.allowed)
    }
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn load_pending() -> Vec<Pending> {
    storage()
        .and_then(|s| s.get_item(LOG_KEY).ok().flatten())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_pending(list: &[Pending]) {
    if let Some(s) = storage() {
        let start = list.len().saturating_sub(MAX_PENDING);
        if let Ok(t) = serde_json::to_string(&list[start..]) {
            let _ = s.set_item(LOG_KEY, &t);
        }
    }
}

fn cached_policy(room: &str) -> Option<P2pPolicy> {
    storage()
        .and_then(|s| s.get_item(&format!("{POLICY_KEY}{room}")).ok().flatten())
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn cache_policy(room: &str, p: &P2pPolicy) {
    if let (Some(s), Ok(t)) = (storage(), serde_json::to_string(p)) {
        let _ = s.set_item(&format!("{POLICY_KEY}{room}"), &t);
    }
}

/// 招待コードを入れたリンク。`#` 以降はサーバーに送られない。
fn invite_link(room: &str, code: &str) -> String {
    let Some(w) = web_sys::window() else {
        return code.to_string();
    };
    let loc = w.location();
    format!(
        "{}{}?room={}#{LINK_PARAM}={code}",
        loc.origin().unwrap_or_default(),
        loc.pathname().unwrap_or_default(),
        http::encode(room)
    )
}

/// QR コード(SVG)。読めない長さなら空。
fn qr_svg(text: &str) -> String {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};
    match QrCode::with_error_correction_level(text.as_bytes(), EcLevel::L) {
        Ok(code) => code
            .render::<svg::Color>()
            .min_dimensions(220, 220)
            .quiet_zone(true)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build(),
        Err(_) => String::new(),
    }
}

fn state_label(l: &LinkInfo) -> &'static str {
    match l.state {
        LinkState::Waiting => "応答コードを待っています",
        LinkState::Connecting => "つないでいます…",
        LinkState::Open if l.online => "つながっています(相手はサーバーにも接続中)",
        LinkState::Open => "つながっています",
        LinkState::Closed => "切れました",
    }
}

impl App {
    fn live_board(&self) -> &board_core::Board {
        self.live_board.as_ref().unwrap_or(&self.board)
    }

    pub(super) fn live_state_vector(&self) -> Vec<u8> {
        self.live_board().state_vector()
    }

    fn net_online(&self) -> bool {
        self.net
            .as_ref()
            .is_some_and(|n| n.status == Status::Online && n.is_open())
    }

    /// 更新をサーバーと直接つないだ相手へ回す([`core::route`])。
    pub(super) fn p2p_forward(&self, source: Source, new_info: bool, update: &[u8]) {
        let me_online = self.net_online();
        let views = self.p2p.as_ref().map(P2p::views).unwrap_or_default();
        let route = core::route(source, new_info, me_online, &views);
        if route.server
            && let Some(net) = &self.net
        {
            // 暗号化ボードなら暗号化してから送る。
            net.send(&self.wire_update(update.to_vec()));
        }
        if let Some(p) = &self.p2p {
            if route.peers.is_empty() {
                return;
            }
            let msg = PeerMsg::Update(update.to_vec());
            for id in route.peers {
                p.send(id, &msg);
            }
        }
    }

    /// 自分の編集を送る。サーバーにも直接の相手にもつながっていなければ、ためたままにする
    /// (つながったときに状態の差分で送る)。
    pub(super) fn flush_outgoing(&mut self) {
        let online = self.net_online();
        let direct = self.p2p.as_ref().is_some_and(|p| p.open_count() > 0);
        if !online && !direct {
            return;
        }
        for u in self.board.take_outgoing() {
            self.p2p_forward(Source::Local, true, &u);
        }
    }

    fn hello(&self) -> PeerMsg {
        PeerMsg::Hello {
            state_vector: self.live_state_vector(),
            online: self.net_online(),
        }
    }

    pub(super) fn pump_p2p(&mut self, now: f64) {
        let Some(p2p) = self.p2p.clone() else {
            return;
        };
        // サーバーへの接続状態が変わったら知らせ直す(差分も取り直す)。
        let online = self.net_online();
        if self.p2p_ui.last_online != Some(online) {
            self.p2p_ui.last_online = Some(online);
            if p2p.open_count() > 0 {
                let hello = self.hello();
                for l in p2p.links() {
                    p2p.send(l.id, &hello);
                }
            }
        }
        for e in p2p.drain() {
            match e {
                Event::Open(id) => {
                    log::info!("直接つながりました({id})");
                    p2p.send(id, &self.hello());
                    self.p2p_ui.presence_sent = None;
                    let name = p2p.name_of(id);
                    if self.p2p_ui.invite.as_ref().is_some_and(|(i, _)| *i == id) {
                        self.p2p_ui.invite = None;
                    }
                    if self.p2p_ui.answer.as_ref().is_some_and(|(i, _)| *i == id) {
                        self.p2p_ui.answer = None;
                    }
                    self.p2p_ui.status = Some((
                        match name.filter(|n| !n.is_empty()) {
                            Some(n) => format!("{n}さんとつながりました"),
                            None => "相手とつながりました".to_string(),
                        },
                        false,
                    ));
                    self.p2p_ui.needs_render = true;
                    self.p2p_log(P2pEventKind::Connect, p2p.name_of(id));
                }
                Event::Message(
                    id,
                    PeerMsg::Hello {
                        state_vector,
                        online,
                    },
                ) => {
                    p2p.set_online(id, online);
                    match self.live_board().encode_diff(Some(&state_vector)) {
                        Ok(diff) => p2p.send(id, &PeerMsg::Update(diff)),
                        Err(e) => log::warn!("直接接続: 差分を作れません: {e}"),
                    }
                }
                Event::Message(id, PeerMsg::Update(u)) => {
                    let before = self.live_state_vector();
                    if let Err(e) = self.live_board_mut().apply_remote(&u) {
                        log::warn!("直接接続: 更新を取り込めません: {e}");
                        continue;
                    }
                    let new_info = self.live_state_vector() != before;
                    if new_info {
                        self.editor.prune_selection(&self.board);
                        self.dirty = true;
                    }
                    self.p2p_forward(Source::Peer(id), new_info, &u);
                }
                Event::Message(id, PeerMsg::Presence(mut p)) => {
                    p.peer = PEER_ID_BASE + id;
                    p2p.set_name(id, &p.name);
                    self.cursors.upsert(p);
                    self.cursors_dirty = true;
                }
                Event::Message(id, PeerMsg::Bye) => p2p.close(id),
                Event::Closed(id) => {
                    let peer = PEER_ID_BASE + id;
                    self.cursors.remove(peer);
                    self.on_peer_leave(peer);
                    self.p2p_log(P2pEventKind::Disconnect, p2p.name_of(id));
                    self.p2p_ui.needs_render = true;
                }
            }
        }
        // カーソルと名前。
        if p2p.open_count() > 0
            && self.p2p_ui.presence_sent != Some(self.cursor_world)
            && now - self.p2p_ui.presence_at >= PRESENCE_INTERVAL_MS
        {
            let (x, y) = self
                .cursor_world
                .map_or((f32::NAN, f32::NAN), |w| (w.x, w.y));
            let msg = PeerMsg::Presence(Presence {
                peer: 0,
                x,
                y,
                color: self.identity.color,
                name: self.identity.name.clone(),
            });
            for l in p2p.links() {
                p2p.send(l.id, &msg);
            }
            self.p2p_ui.presence_sent = Some(self.cursor_world);
            self.p2p_ui.presence_at = now;
        }
    }

    /// 使った記録をためて、送れるなら送る。
    fn p2p_log(&mut self, kind: P2pEventKind, peer: Option<String>) {
        let Some(room) = self.room.clone() else {
            return;
        };
        let mut list = load_pending();
        list.push(Pending {
            room,
            key: self.p2p_ui.key.clone(),
            event: Some(P2pEvent {
                kind,
                at: now_ms(),
                peer: peer.filter(|p| !p.is_empty()),
            }),
            report: None,
        });
        save_pending(&list);
        self.p2p_flush_log();
    }

    /// ためた記録をサーバーへ送る(ほかのボードの分も)。
    pub(super) fn p2p_flush_log(&mut self) {
        if self.p2p_ui.flushing || load_pending().is_empty() {
            return;
        }
        self.p2p_ui.flushing = true;
        let Some(app) = self.self_ref() else {
            self.p2p_ui.flushing = false;
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            let room = app.borrow().room.clone().unwrap_or_default();
            let denied = flush(&room).await;
            let mut a = app.borrow_mut();
            a.p2p_ui.flushing = false;
            if let Some(reason) = denied {
                a.p2p_denied(reason);
            }
        });
    }

    /// サーバーが「使えない」と答えた。つないでいる相手とは切る。
    fn p2p_denied(&mut self, reason: String) {
        let policy = P2pPolicy {
            allowed: false,
            reason: Some(reason.clone()),
        };
        if let Some(room) = &self.room {
            cache_policy(room, &policy);
        }
        self.p2p_ui.policy = Some(policy);
        self.p2p_ui.invite = None;
        self.p2p_ui.answer = None;
        if let Some(p) = &self.p2p
            && !p.links().is_empty()
        {
            p.close_all();
            self.p2p_ui.status = Some((format!("切断しました: {reason}"), true));
        }
        self.p2p_ui.needs_render = true;
    }

    fn self_ref(&self) -> Option<Rc<RefCell<App>>> {
        SELF.with(|s| s.borrow().as_ref().and_then(std::rc::Weak::upgrade))
    }

    /// 画面を合わせる(状態が変わったときだけ描き直す)。
    pub(super) fn p2p_frame(&mut self) {
        let Some(p2p) = &self.p2p else { return };
        let v = p2p.version();
        if self.p2p_ui.last_version != Some(v) {
            self.p2p_ui.last_version = Some(v);
            self.p2p_ui.needs_render = true;
            p2p.forget_closed();
        }
        if !self.p2p_ui.needs_render {
            return;
        }
        self.p2p_ui.needs_render = false;
        self.render_p2p();
    }

    fn render_p2p(&self) {
        let (Some(p2p), Some(room)) = (&self.p2p, &self.room) else {
            return;
        };
        let doc = &self.document;
        let Some(toggle) = doc
            .get_element_by_id("p2p-toggle")
            .and_then(|e| e.dyn_into::<HtmlElement>().ok())
        else {
            return;
        };
        let Some(panel) = doc
            .get_element_by_id("p2p-panel")
            .and_then(|e| e.dyn_into::<HtmlElement>().ok())
        else {
            return;
        };
        let links = p2p.links();
        let open = links.iter().filter(|l| l.state == LinkState::Open).count();
        toggle.set_text_content(Some(&if open > 0 {
            format!("直接つなぐ · {open}台")
        } else {
            "直接つなぐ".to_string()
        }));
        let _ = toggle.set_attribute("data-live", if open > 0 { "true" } else { "false" });
        let _ = toggle.set_attribute(
            "aria-pressed",
            if panel.hidden() { "false" } else { "true" },
        );
        if panel.hidden() {
            return;
        }
        let ui = &self.p2p_ui;
        // 打ちかけの文字を描き直しで消さない。
        let keep = |id: &str| -> String {
            doc.get_element_by_id(id)
                .and_then(|e| e.dyn_into::<HtmlTextAreaElement>().ok())
                .map(|t| t.value())
                .unwrap_or_default()
        };
        let typed_offer = keep("p2p-offer-input");
        let typed_answer = keep("p2p-answer-input");
        let typed_note = keep("p2p-report-note");

        let mut h = String::from(
            r#"<header class="row"><strong class="grow">直接つなぐ(P2P)</strong><button class="quiet" data-action="p2p-close" aria-label="閉じる">×</button></header>
<p class="small muted">サーバーを通さず、同じ Wi-Fi・LAN の端末や同じパソコンのタブと直接つないで一緒に編集します。サーバーが止まっていても使えます。ボードの中身はサーバーに送りません(使った記録だけが操作ログに残ります)。</p>"#,
        );
        if let Some((m, err)) = &ui.status {
            h.push_str(&format!(
                r#"<p class="p2p-status" data-kind="{}" role="{}">{}</p>"#,
                if *err { "error" } else { "info" },
                if *err { "alert" } else { "status" },
                escape(m)
            ));
        }
        let busy = if ui.busy { "disabled" } else { "" };
        if !ui.allowed() {
            let reason = ui
                .policy
                .as_ref()
                .and_then(|p| p.reason.clone())
                .unwrap_or_else(|| "このボードでは直接接続を使えません".into());
            h.push_str(&format!(
                r#"<p class="p2p-status" data-kind="error" role="alert">{}</p>"#,
                escape(&reason)
            ));
        } else {
            // 招待する
            h.push_str(r#"<section class="p2p-step"><h3>招待する</h3>"#);
            match &ui.invite {
                None => h.push_str(&format!(
                    r#"<button data-action="p2p-invite" {busy}>招待コードを作る</button>"#
                )),
                Some((_, code)) => {
                    let link = invite_link(room, code);
                    h.push_str(&format!(
                        r#"<p class="small">1. このリンクを相手に渡します(相手のスマホのカメラで QR コードを読んでも開けます)。</p>
<div class="p2p-qr" aria-hidden="true">{qr}</div>
<textarea id="p2p-offer" class="p2p-code" readonly rows="3" aria-label="招待リンク" translate="no">{link}</textarea>
<div class="row"><button data-action="p2p-copy" data-target="p2p-offer">リンクをコピー</button><button class="quiet" data-action="p2p-cancel">招待をやめる</button></div>
<p class="small">2. 相手の画面に出た応答コードを貼って、つなぎます。</p>
<textarea id="p2p-answer-input" class="p2p-code" rows="3" placeholder="応答コード(GB1. で始まる文字)" aria-label="応答コード" translate="no">{typed}</textarea>
<button data-action="p2p-accept" {busy}>つなぐ</button>"#,
                        qr = qr_svg(&link),
                        link = escape(&link),
                        typed = escape(&typed_answer),
                    ));
                }
            }
            h.push_str("</section>");
            // 招待された
            h.push_str(r#"<section class="p2p-step"><h3>招待された</h3>"#);
            match &ui.answer {
                None => h.push_str(&format!(
                    r#"<textarea id="p2p-offer-input" class="p2p-code" rows="3" placeholder="受け取った招待リンクかコードを貼る" aria-label="招待リンクかコード" translate="no">{}</textarea>
<button data-action="p2p-join" {busy}>参加する</button>"#,
                    escape(&typed_offer)
                )),
                Some((_, code)) => h.push_str(&format!(
                    r#"<p class="small">この応答コードを、招待した人に渡してください(チャットで送るか、相手の画面の欄に貼ってもらいます)。</p>
<div class="p2p-qr" aria-hidden="true">{qr}</div>
<textarea id="p2p-answer" class="p2p-code" readonly rows="3" aria-label="応答コード" translate="no">{code}</textarea>
<div class="row"><button data-action="p2p-copy" data-target="p2p-answer">応答コードをコピー</button><button class="quiet" data-action="p2p-cancel-join">やめる</button></div>"#,
                    qr = qr_svg(code),
                    code = escape(code),
                )),
            }
            h.push_str("</section>");
        }
        // つながっている端末
        let shown: Vec<&LinkInfo> = links
            .iter()
            .filter(|l| l.state != LinkState::Waiting)
            .collect();
        if !shown.is_empty() {
            h.push_str(
                r#"<section class="p2p-step"><h3>直接つないだ端末</h3><ul class="p2p-links">"#,
            );
            for l in shown {
                let name = if l.name.is_empty() {
                    "名前のない端末".to_string()
                } else {
                    l.name.clone()
                };
                h.push_str(&format!(
                    r#"<li class="p2p-link" data-state="{st:?}"><div class="row"><span class="grow"><strong translate="no">{name}</strong><br><span class="small muted">{label}</span></span><button class="quiet" data-action="p2p-disconnect" data-id="{id}">切断</button><button class="quiet" data-action="p2p-report" data-id="{id}">通報</button></div>"#,
                    st = l.state,
                    name = escape(&name),
                    label = state_label(l),
                    id = l.id,
                ));
                if ui.report_for == Some(l.id) {
                    let opts: String = ReportReason::ALL
                        .iter()
                        .map(|r| format!(r#"<option value="{}">{}</option>"#, r.id(), r.label()))
                        .collect();
                    h.push_str(&format!(
                        r#"<div class="p2p-report"><label class="small">理由<select id="p2p-report-reason">{opts}</select></label>
<textarea id="p2p-report-note" rows="2" maxlength="{max}" placeholder="補足(任意)。ボードの中身は送られません" aria-label="補足">{note}</textarea>
<div class="row"><button class="danger" data-action="p2p-report-send" data-id="{id}">通報して切断</button><button class="quiet" data-action="p2p-report-cancel">やめる</button></div>
<p class="small muted">通報は管理者の操作ログに残ります。</p></div>"#,
                        max = MAX_REPORT_NOTE_CHARS,
                        note = escape(&typed_note),
                        id = l.id,
                    ));
                }
                h.push_str("</li>");
            }
            h.push_str("</ul></section>");
        }
        h.push_str(r#"<p class="small muted">招待した相手は、このボードをあなたと同じように見て編集できます。知らない人にはリンクやコードを渡さないでください。つながるのは同じネットワークの端末どうしです。</p>"#);
        panel.set_inner_html(&h);
    }
}

thread_local! {
    static SELF: RefCell<Option<std::rc::Weak<RefCell<App>>>> = const { RefCell::new(None) };
}

/// ためた記録を送る。サーバーに「使えない」と言われたら、その理由を返す(今のボードのとき)。
async fn flush(current_room: &str) -> Option<String> {
    let list = load_pending();
    let sent = list.len();
    let mut keep = Vec::new();
    let mut denied = None;
    for p in list {
        let res = if let Some(r) = &p.report {
            http::send::<_, ()>(
                "POST",
                &http::board_path(&p.room, p.key.as_deref(), "/p2p/report"),
                r,
            )
            .await
        } else if let Some(e) = &p.event {
            http::send::<_, ()>(
                "POST",
                &http::board_path(&p.room, p.key.as_deref(), "/p2p/events"),
                &vec![e.clone()],
            )
            .await
        } else {
            Ok(())
        };
        match res {
            Ok(()) => {}
            // サーバーがない(オフライン): 次につながったときに送る。
            Err(HttpError::Unavailable) => keep.push(p),
            // 記録はされたが、止められている。
            Err(HttpError::Status(403, msg)) => {
                if p.room == current_room {
                    denied = Some(msg);
                }
            }
            // ボードがない・権限がないなどは送り直しても同じなので捨てる。
            Err(HttpError::Status(status, msg)) => {
                log::warn!("直接接続の記録を送れません({status}): {msg}");
            }
        }
    }
    // 送っている間に増えた分は残す(この関数だけが消すので、先頭の送った分を除けばよい)。
    let added: Vec<Pending> = load_pending().into_iter().skip(sent).collect();
    keep.extend(added);
    save_pending(&keep);
    denied
}

fn action_of(e: &web_sys::Event) -> Option<Element> {
    e.target()?
        .dyn_into::<Element>()
        .ok()?
        .closest("[data-action]")
        .ok()?
}

fn text_of(doc: &Document, id: &str) -> String {
    doc.get_element_by_id(id)
        .and_then(|e| e.dyn_into::<HtmlTextAreaElement>().ok())
        .map(|t| t.value())
        .unwrap_or_default()
}

fn set_status(app: &Rc<RefCell<App>>, msg: impl Into<String>, error: bool) {
    let mut a = app.borrow_mut();
    a.p2p_ui.status = Some((msg.into(), error));
    a.p2p_ui.needs_render = true;
}

/// URL の `#p2p=…` を取り出して消す(読み込み直しで二度参加しないように)。
fn take_hash_code(window: &web_sys::Window) -> Option<String> {
    let hash = window.location().hash().ok()?;
    let prefix = format!("#{LINK_PARAM}=");
    let code = hash.strip_prefix(&prefix)?.to_string();
    if let Ok(h) = window.history() {
        let loc = window.location();
        let url = format!(
            "{}{}",
            loc.pathname().unwrap_or_default(),
            loc.search().unwrap_or_default()
        );
        let _ = h.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url));
    }
    Some(code)
}

fn start_invite(app: &Rc<RefCell<App>>) {
    let (p2p, room, name) = {
        let mut a = app.borrow_mut();
        let (Some(p2p), Some(room)) = (a.p2p.clone(), a.room.clone()) else {
            return;
        };
        if !a.p2p_ui.allowed() || a.p2p_ui.busy {
            return;
        }
        a.p2p_ui.busy = true;
        a.p2p_ui.status = Some(("招待コードを作っています…".into(), false));
        a.p2p_ui.needs_render = true;
        (p2p, room, a.identity.name.clone())
    };
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let res = p2p.invite(&room, &name).await;
        let mut a = app.borrow_mut();
        a.p2p_ui.busy = false;
        a.p2p_ui.needs_render = true;
        match res {
            Ok((id, code)) => {
                a.p2p_ui.invite = Some((id, code));
                a.p2p_ui.status = None;
                a.p2p_log(P2pEventKind::Invite, None);
            }
            Err(e) => a.p2p_ui.status = Some((e, true)),
        }
    });
}

fn start_join(app: &Rc<RefCell<App>>, text: &str) {
    let code = match Code::decode(text) {
        Ok(c) => c,
        Err(e) => return set_status(app, e.to_string(), true),
    };
    if code.kind == CodeKind::Answer {
        return set_status(
            app,
            "これは応答コードです。「招待する」の欄に貼ってください",
            true,
        );
    }
    let (p2p, name) = {
        let mut a = app.borrow_mut();
        let Some(p2p) = a.p2p.clone() else { return };
        if a.room.as_deref() != Some(code.room.as_str()) {
            // 別のボードへの招待: そのボードを開き直してから参加する。
            let link = invite_link(&code.room, &code.encode());
            let _ = a.window.location().set_href(&link);
            return;
        }
        if !a.p2p_ui.allowed() || a.p2p_ui.busy {
            return;
        }
        a.p2p_ui.busy = true;
        a.p2p_ui.status = Some(("応答コードを作っています…".into(), false));
        a.p2p_ui.needs_render = true;
        (p2p, a.identity.name.clone())
    };
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let res = p2p.join(&code, &name).await;
        let mut a = app.borrow_mut();
        a.p2p_ui.busy = false;
        a.p2p_ui.needs_render = true;
        match res {
            Ok((id, answer)) => {
                a.p2p_ui.answer = Some((id, answer));
                a.p2p_ui.status = Some((
                    if code.name.is_empty() {
                        "招待を受け取りました".to_string()
                    } else {
                        format!("{}さんからの招待を受け取りました", code.name)
                    },
                    false,
                ));
                a.p2p_log(P2pEventKind::Join, Some(code.name.clone()));
            }
            Err(e) => a.p2p_ui.status = Some((e, true)),
        }
    });
}

fn start_accept(app: &Rc<RefCell<App>>, text: &str) {
    let code = match Code::decode(text) {
        Ok(c) => c,
        Err(e) => return set_status(app, e.to_string(), true),
    };
    let Some(p2p) = app.borrow().p2p.clone() else {
        return;
    };
    set_status(app, "つないでいます…", false);
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = p2p.accept(&code).await {
            set_status(&app, e, true);
        }
    });
}

fn send_report(app: &Rc<RefCell<App>>, id: u32) {
    let doc = app.borrow().document.clone();
    let reason = doc
        .get_element_by_id("p2p-report-reason")
        .and_then(|e| e.dyn_into::<HtmlSelectElement>().ok())
        .and_then(|s| ReportReason::parse(&s.value()))
        .unwrap_or(ReportReason::Other);
    let note: String = text_of(&doc, "p2p-report-note")
        .chars()
        .take(MAX_REPORT_NOTE_CHARS)
        .collect();
    let mut a = app.borrow_mut();
    let Some(room) = a.room.clone() else { return };
    let peer = a
        .p2p
        .as_ref()
        .and_then(|p| p.name_of(id))
        .unwrap_or_default();
    let mut list = load_pending();
    list.push(Pending {
        room,
        key: a.p2p_ui.key.clone(),
        event: None,
        report: Some(P2pReport {
            reason,
            peer,
            note,
            at: now_ms(),
        }),
    });
    save_pending(&list);
    if let Some(p) = &a.p2p {
        p.close(id);
    }
    a.p2p_ui.report_for = None;
    a.p2p_ui.status = Some((
        if a.net_online() {
            "通報して切断しました。管理者が操作ログで確認します".into()
        } else {
            "切断しました。通報はサーバーにつながったときに送ります".into()
        },
        false,
    ));
    a.p2p_ui.needs_render = true;
    a.p2p_flush_log();
}

fn on_action(app: &Rc<RefCell<App>>, el: &Element) {
    let action = el.get_attribute("data-action").unwrap_or_default();
    let id = el
        .get_attribute("data-id")
        .and_then(|s| s.parse::<u32>().ok());
    let doc = app.borrow().document.clone();
    match action.as_str() {
        "p2p-close" => {
            if let Some(panel) = doc
                .get_element_by_id("p2p-panel")
                .and_then(|e| e.dyn_into::<HtmlElement>().ok())
            {
                panel.set_hidden(true);
            }
            app.borrow_mut().p2p_ui.needs_render = true;
        }
        "p2p-invite" => start_invite(app),
        "p2p-join" => start_join(app, &text_of(&doc, "p2p-offer-input")),
        "p2p-accept" => start_accept(app, &text_of(&doc, "p2p-answer-input")),
        "p2p-cancel" | "p2p-cancel-join" => {
            let mut a = app.borrow_mut();
            let slot = if action == "p2p-cancel" {
                a.p2p_ui.invite.take()
            } else {
                a.p2p_ui.answer.take()
            };
            if let (Some((id, _)), Some(p)) = (slot, &a.p2p) {
                p.close(id);
            }
            a.p2p_ui.status = None;
            a.p2p_ui.needs_render = true;
        }
        "p2p-copy" => {
            let target = el.get_attribute("data-target").unwrap_or_default();
            let text = text_of(&doc, &target);
            if let Some(w) = web_sys::window() {
                let _ = w.navigator().clipboard().write_text(&text);
            }
            set_status(app, "コピーしました", false);
        }
        "p2p-disconnect" => {
            if let (Some(id), Some(p)) = (id, app.borrow().p2p.clone()) {
                p.close(id);
            }
        }
        "p2p-report" => {
            let mut a = app.borrow_mut();
            a.p2p_ui.report_for = id;
            a.p2p_ui.needs_render = true;
        }
        "p2p-report-cancel" => {
            let mut a = app.borrow_mut();
            a.p2p_ui.report_for = None;
            a.p2p_ui.needs_render = true;
        }
        "p2p-report-send" => {
            if let Some(id) = id {
                send_report(app, id);
            }
        }
        _ => {}
    }
}

/// サーバーに「使ってよいか」を聞く。サーバーがなければ、最後に聞いた答えのまま。
fn fetch_policy(app: &Rc<RefCell<App>>, room: String) {
    let key = app.borrow().p2p_ui.key.clone();
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let res = http::get::<P2pPolicy>(&http::board_path(&room, key.as_deref(), "/p2p")).await;
        let Ok(policy) = res else { return };
        cache_policy(&room, &policy);
        let mut a = app.borrow_mut();
        if policy.allowed {
            a.p2p_ui.policy = Some(policy);
            a.p2p_ui.needs_render = true;
        } else {
            a.p2p_denied(policy.reason.unwrap_or_default());
        }
    });
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    SELF.with(|s| *s.borrow_mut() = Some(Rc::downgrade(app)));
    let toggle: HtmlElement = by_id(doc, "p2p-toggle")?;
    let panel: HtmlElement = by_id(doc, "p2p-panel")?;
    let (room, window) = {
        let a = app.borrow();
        (a.p2p.as_ref().and(a.room.clone()), a.window.clone())
    };
    toggle.set_hidden(room.is_none());
    let Some(room) = room else {
        return Ok(());
    };
    app.borrow_mut().p2p_ui.policy = cached_policy(&room);
    fetch_policy(app, room);
    app.borrow_mut().p2p_flush_log();
    {
        let app = app.clone();
        let panel = panel.clone();
        listen(
            toggle.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                panel.set_hidden(!panel.hidden());
                let mut a = app.borrow_mut();
                a.p2p_ui.needs_render = true;
                // パネルを開くたびに設定を聞き直す(管理者が変えたかもしれない)。
                if !panel.hidden()
                    && let Some(room) = a.room.clone()
                {
                    drop(a);
                    fetch_policy(&app, room);
                }
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
                if let Some(el) = action_of(&e) {
                    on_action(&app, &el);
                }
            },
        )?;
    }
    // 招待リンクから開いた: すぐ応答コードを作る。
    if let Some(code) = take_hash_code(&window) {
        panel.set_hidden(false);
        start_join(app, &code);
    }
    Ok(())
}
