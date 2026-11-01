//! サーバーを通さない直接接続(WebRTC のデータチャネル)。
//!
//! 接続手順を中継するサーバーがないので、SDP は ICE 候補を集め終わってから 1 回で渡す
//! (招待コード → 応答コード の 1 往復)。ICE サーバーは使わないので、つながるのは
//! 同じ LAN の端末か同じ端末どうし。中身の形・分割・回し方は [`board_core::p2p`]。
//!
//! ここは接続とデータチャネルの面倒だけを見る。届いたメッセージは `drain` で描画ループに渡す。

use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use board_core::p2p::{self as core, Code, CodeKind, LinkView, MAX_LINKS, PeerMsg, Reassembler};
use js_sys::Reflect;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    MessageEvent, RtcConfiguration, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelInit,
    RtcDataChannelState, RtcDataChannelType, RtcIceGatheringState, RtcPeerConnection, RtcSdpType,
    RtcSessionDescriptionInit,
};

/// ICE 候補を集めるのを待つ上限(ミリ秒)。LAN の候補はすぐ集まる。
const GATHER_TIMEOUT_MS: i32 = 4000;
/// データチャネルの送信待ちがこれを超えたら、減るまで待つ。
const HIGH_WATER: u32 = 1024 * 1024;
const LOW_WATER: u32 = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// 招待コードを作って、応答コードを待っている。
    Waiting,
    /// 応答を受け取って、つながるのを待っている。
    Connecting,
    Open,
    Closed,
}

/// 画面に出す相手の様子。
#[derive(Clone, Debug, PartialEq)]
pub struct LinkInfo {
    pub id: u32,
    pub name: String,
    pub state: LinkState,
    /// 自分が招待した相手か。
    pub invited: bool,
    /// 相手もサーバーにつながっているか。
    pub online: bool,
}

pub enum Event {
    Open(u32),
    Message(u32, PeerMsg),
    Closed(u32),
}

struct Link {
    id: u32,
    token: u32,
    invited: bool,
    name: String,
    pc: RtcPeerConnection,
    dc: Option<RtcDataChannel>,
    state: LinkState,
    online: bool,
    reasm: Reassembler,
    queue: VecDeque<Vec<u8>>,
    callbacks: Vec<Box<dyn Any>>,
}

impl Link {
    fn shutdown(&mut self) {
        if let Some(dc) = &self.dc {
            dc.set_onopen(None);
            dc.set_onmessage(None);
            dc.set_onclose(None);
            dc.set_onbufferedamountlow(None);
            dc.close();
        }
        self.pc.set_ondatachannel(None);
        self.pc.set_onconnectionstatechange(None);
        self.pc.close();
        self.queue.clear();
        self.state = LinkState::Closed;
    }
}

#[derive(Default)]
struct Inner {
    links: Vec<Link>,
    next_id: u32,
    next_msg: u32,
    inbox: VecDeque<Event>,
    version: u64,
}

impl Inner {
    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }
    fn link(&mut self, id: u32) -> Option<&mut Link> {
        self.links.iter_mut().find(|l| l.id == id)
    }
    fn active(&self) -> usize {
        self.links
            .iter()
            .filter(|l| l.state != LinkState::Closed)
            .count()
    }
}

/// 直接接続をまとめて持つ。複製しても同じものを指す。
#[derive(Clone, Default)]
pub struct P2p(Rc<RefCell<Inner>>);

fn js_text(e: &JsValue) -> String {
    Reflect::get(e, &"message".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}

async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(p).await;
}

fn random_u32() -> u32 {
    (js_sys::Math::random() * f64::from(u32::MAX)) as u32
}

/// ICE 候補を集め終わる(か時間切れになる)のを待って、手元の SDP を返す。
async fn gathered_sdp(pc: &RtcPeerConnection) -> Result<String, String> {
    let mut waited = 0;
    while pc.ice_gathering_state() != RtcIceGatheringState::Complete && waited < GATHER_TIMEOUT_MS {
        sleep(100).await;
        waited += 100;
    }
    let sdp = pc
        .local_description()
        .map(|d| d.sdp())
        .ok_or("接続の情報を作れませんでした")?;
    if !sdp.contains("a=candidate") {
        return Err(
            "この端末のネットワークが見つかりません(Wi-Fi か LAN につないでください)".into(),
        );
    }
    Ok(sdp)
}

impl P2p {
    pub fn new() -> Self {
        Self::default()
    }

    /// 状態が変わるたびに増える(画面の更新の目印)。
    pub fn version(&self) -> u64 {
        self.0.borrow().version
    }

    pub fn drain(&self) -> Vec<Event> {
        self.0.borrow_mut().inbox.drain(..).collect()
    }

    pub fn links(&self) -> Vec<LinkInfo> {
        self.0
            .borrow()
            .links
            .iter()
            .map(|l| LinkInfo {
                id: l.id,
                name: l.name.clone(),
                state: l.state,
                invited: l.invited,
                online: l.online,
            })
            .collect()
    }

    /// 更新の回し先を決めるための様子([`core::route`])。
    pub fn views(&self) -> Vec<LinkView> {
        self.0
            .borrow()
            .links
            .iter()
            .map(|l| LinkView {
                id: l.id,
                open: l.state == LinkState::Open,
                online: l.online,
            })
            .collect()
    }

    pub fn open_count(&self) -> usize {
        self.0
            .borrow()
            .links
            .iter()
            .filter(|l| l.state == LinkState::Open)
            .count()
    }

    pub fn name_of(&self, id: u32) -> Option<String> {
        self.0
            .borrow()
            .links
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.name.clone())
    }

    pub fn set_name(&self, id: u32, name: &str) {
        let mut i = self.0.borrow_mut();
        if let Some(l) = i.link(id)
            && l.name != name
        {
            l.name = name.to_string();
            i.bump();
        }
    }

    pub fn set_online(&self, id: u32, online: bool) {
        let mut i = self.0.borrow_mut();
        if let Some(l) = i.link(id)
            && l.online != online
        {
            l.online = online;
            i.bump();
        }
    }

    fn new_pc(&self, invited: bool, token: u32, name: &str) -> Result<u32, String> {
        let mut i = self.0.borrow_mut();
        if i.active() >= MAX_LINKS {
            return Err(format!("直接つなげるのは {MAX_LINKS} 台までです"));
        }
        // ICE サーバーなし: 同じ LAN・同じ端末の候補だけを使う(外のサーバーに何も問い合わせない)。
        let config = RtcConfiguration::new();
        let pc = RtcPeerConnection::new_with_configuration(&config).map_err(|e| js_text(&e))?;
        i.next_id += 1;
        let id = i.next_id;
        let weak = Rc::downgrade(&self.0);
        let pc2 = pc.clone();
        let on_state = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            use web_sys::RtcPeerConnectionState as S;
            if matches!(pc2.connection_state(), S::Failed | S::Closed)
                && let Some(inner) = weak.upgrade()
            {
                P2p(inner).mark_closed(id);
            }
        });
        pc.set_onconnectionstatechange(Some(on_state.as_ref().unchecked_ref()));
        i.links.push(Link {
            id,
            token,
            invited,
            name: name.to_string(),
            pc,
            dc: None,
            state: if invited {
                LinkState::Waiting
            } else {
                LinkState::Connecting
            },
            online: false,
            reasm: Reassembler::default(),
            queue: VecDeque::new(),
            callbacks: vec![Box::new(on_state)],
        });
        i.bump();
        Ok(id)
    }

    fn pc(&self, id: u32) -> Option<RtcPeerConnection> {
        self.0
            .borrow()
            .links
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.pc.clone())
    }

    fn attach_channel(&self, id: u32, dc: RtcDataChannel) {
        dc.set_binary_type(RtcDataChannelType::Arraybuffer);
        dc.set_buffered_amount_low_threshold(LOW_WATER);
        let weak = Rc::downgrade(&self.0);
        let on_open = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let mut i = inner.borrow_mut();
            if let Some(l) = i.link(id) {
                l.state = LinkState::Open;
            }
            i.inbox.push_back(Event::Open(id));
            i.bump();
        });
        let weak = Rc::downgrade(&self.0);
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let Some(inner) = weak.upgrade() else { return };
            let Ok(buf) = e.data().dyn_into::<js_sys::ArrayBuffer>() else {
                return;
            };
            let bytes = js_sys::Uint8Array::new(&buf).to_vec();
            let mut i = inner.borrow_mut();
            let Some(l) = i.link(id) else { return };
            match l.reasm.push(&bytes) {
                Ok(Some(msg)) => match PeerMsg::decode(&msg) {
                    Ok(m) => i.inbox.push_back(Event::Message(id, m)),
                    Err(e) => log::warn!("直接接続: {e}"),
                },
                Ok(None) => {}
                Err(e) => log::warn!("直接接続: {e}"),
            }
        });
        let weak = Rc::downgrade(&self.0);
        let on_close = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if let Some(inner) = weak.upgrade() {
                P2p(inner).mark_closed(id);
            }
        });
        let weak = Rc::downgrade(&self.0);
        let on_low = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if let Some(inner) = weak.upgrade() {
                P2p(inner).pump(id);
            }
        });
        dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        dc.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        dc.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        dc.set_onbufferedamountlow(Some(on_low.as_ref().unchecked_ref()));
        let mut i = self.0.borrow_mut();
        if let Some(l) = i.link(id) {
            l.dc = Some(dc);
            l.callbacks.push(Box::new(on_open));
            l.callbacks.push(Box::new(on_message));
            l.callbacks.push(Box::new(on_close));
            l.callbacks.push(Box::new(on_low));
        }
    }

    fn mark_closed(&self, id: u32) {
        let mut i = self.0.borrow_mut();
        let Some(l) = i.link(id) else { return };
        if l.state == LinkState::Closed {
            return;
        }
        let was_open = l.state == LinkState::Open;
        l.state = LinkState::Closed;
        l.queue.clear();
        if was_open {
            i.inbox.push_back(Event::Closed(id));
        }
        i.bump();
    }

    fn fail(&self, id: u32) {
        let mut i = self.0.borrow_mut();
        if let Some(pos) = i.links.iter().position(|l| l.id == id) {
            let mut l = i.links.remove(pos);
            l.shutdown();
            i.bump();
        }
    }

    /// 招待コードを作る。相手の応答コードを [`P2p::accept`] に渡すとつながる。
    pub async fn invite(&self, room: &str, my_name: &str) -> Result<(u32, String), String> {
        let token = random_u32();
        let id = self.new_pc(true, token, "")?;
        let result: Result<String, String> = async {
            let pc = self.pc(id).ok_or("接続を作れませんでした")?;
            let init = RtcDataChannelInit::new();
            init.set_ordered(true);
            let dc = pc.create_data_channel_with_data_channel_dict("graboard", &init);
            self.attach_channel(id, dc);
            let offer = JsFuture::from(pc.create_offer())
                .await
                .map_err(|e| js_text(&e))?;
            let desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
            desc.set_sdp(&sdp_of(&offer));
            JsFuture::from(pc.set_local_description(&desc))
                .await
                .map_err(|e| js_text(&e))?;
            let sdp = gathered_sdp(&pc).await?;
            Ok(Code {
                kind: CodeKind::Offer,
                token,
                room: room.to_string(),
                name: my_name.to_string(),
                sdp,
            }
            .encode())
        }
        .await;
        match result {
            Ok(code) => Ok((id, code)),
            Err(e) => {
                self.fail(id);
                Err(e)
            }
        }
    }

    /// 招待コードに応じて、相手に返す応答コードを作る。
    pub async fn join(&self, offer: &Code, my_name: &str) -> Result<(u32, String), String> {
        if offer.kind != CodeKind::Offer {
            return Err(
                "これは応答コードです。招待した側で「応答コードを貼る」に貼ってください".into(),
            );
        }
        let id = self.new_pc(false, offer.token, &offer.name)?;
        let result: Result<String, String> = async {
            let pc = self.pc(id).ok_or("接続を作れませんでした")?;
            let weak = Rc::downgrade(&self.0);
            let on_channel =
                Closure::<dyn FnMut(RtcDataChannelEvent)>::new(move |e: RtcDataChannelEvent| {
                    if let Some(inner) = weak.upgrade() {
                        P2p(inner).attach_channel(id, e.channel());
                    }
                });
            pc.set_ondatachannel(Some(on_channel.as_ref().unchecked_ref()));
            if let Some(l) = self.0.borrow_mut().link(id) {
                l.callbacks.push(Box::new(on_channel));
            }
            let desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
            desc.set_sdp(&offer.sdp);
            JsFuture::from(pc.set_remote_description(&desc))
                .await
                .map_err(|_| "招待コードを読めませんでした".to_string())?;
            let answer = JsFuture::from(pc.create_answer())
                .await
                .map_err(|e| js_text(&e))?;
            let desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
            desc.set_sdp(&sdp_of(&answer));
            JsFuture::from(pc.set_local_description(&desc))
                .await
                .map_err(|e| js_text(&e))?;
            let sdp = gathered_sdp(&pc).await?;
            Ok(Code {
                kind: CodeKind::Answer,
                token: offer.token,
                room: offer.room.clone(),
                name: my_name.to_string(),
                sdp,
            }
            .encode())
        }
        .await;
        match result {
            Ok(code) => Ok((id, code)),
            Err(e) => {
                self.fail(id);
                Err(e)
            }
        }
    }

    /// 応答コードを受け取ってつなぐ。
    pub async fn accept(&self, answer: &Code) -> Result<u32, String> {
        if answer.kind != CodeKind::Answer {
            return Err("これは招待コードです。応答コードを貼ってください".into());
        }
        let (id, pc) = {
            let i = self.0.borrow();
            let l = i
                .links
                .iter()
                .find(|l| l.invited && l.token == answer.token && l.state == LinkState::Waiting)
                .ok_or(
                    "この応答コードに合う招待が見つかりません(使い済みか、別の招待への応答です)",
                )?;
            (l.id, l.pc.clone())
        };
        let desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
        desc.set_sdp(&answer.sdp);
        JsFuture::from(pc.set_remote_description(&desc))
            .await
            .map_err(|_| "応答コードを読めませんでした".to_string())?;
        let mut i = self.0.borrow_mut();
        if let Some(l) = i.link(id) {
            l.name = answer.name.clone();
            if l.state == LinkState::Waiting {
                l.state = LinkState::Connecting;
            }
        }
        i.bump();
        Ok(id)
    }

    /// 1 人に送る。
    pub fn send(&self, id: u32, msg: &PeerMsg) {
        let bytes = msg.encode();
        {
            let mut i = self.0.borrow_mut();
            i.next_msg = i.next_msg.wrapping_add(1);
            let n = i.next_msg;
            let Some(l) = i.link(id) else { return };
            if l.state != LinkState::Open {
                return;
            }
            l.queue.extend(core::split(&bytes, n));
        }
        self.pump(id);
    }

    /// 送信待ちを、データチャネルがあふれない範囲で流す。
    fn pump(&self, id: u32) {
        let mut i = self.0.borrow_mut();
        let Some(l) = i.link(id) else { return };
        let Some(dc) = l.dc.clone() else { return };
        while dc.ready_state() == RtcDataChannelState::Open && dc.buffered_amount() < HIGH_WATER {
            let Some(frame) = l.queue.pop_front() else {
                break;
            };
            if let Err(e) = dc.send_with_u8_array(&frame) {
                log::warn!("直接接続: 送れません: {e:?}");
                l.queue.clear();
                break;
            }
        }
    }

    /// 切断する(相手には `Bye` を送ってから)。
    pub fn close(&self, id: u32) {
        self.send(id, &PeerMsg::Bye);
        let was_open = {
            let mut i = self.0.borrow_mut();
            let Some(pos) = i.links.iter().position(|l| l.id == id) else {
                return;
            };
            let mut l = i.links.remove(pos);
            let was_open = l.state == LinkState::Open;
            // Bye が届くよう、データチャネルを閉じる前に少し待つ。
            let dc = l.dc.clone();
            l.state = LinkState::Closed;
            let pc = l.pc.clone();
            let callbacks = std::mem::take(&mut l.callbacks);
            wasm_bindgen_futures::spawn_local(async move {
                sleep(300).await;
                if let Some(dc) = dc {
                    dc.close();
                }
                pc.close();
                drop(callbacks);
            });
            i.bump();
            was_open
        };
        if was_open {
            self.0.borrow_mut().inbox.push_back(Event::Closed(id));
        }
    }

    pub fn close_all(&self) {
        let ids: Vec<u32> = self.0.borrow().links.iter().map(|l| l.id).collect();
        for id in ids {
            self.close(id);
        }
    }

    /// 閉じた接続を一覧から片付ける。
    pub fn forget_closed(&self) {
        let mut i = self.0.borrow_mut();
        let before = i.links.len();
        i.links.retain_mut(|l| {
            if l.state == LinkState::Closed {
                l.shutdown();
                false
            } else {
                true
            }
        });
        if i.links.len() != before {
            i.bump();
        }
    }
}

fn sdp_of(desc: &JsValue) -> String {
    Reflect::get(desc, &"sdp".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}
