//! ボードを開いたまま話せる音声通話(WebRTC の P2P メッシュ)。
//!
//! - 参加状態(`protocol::Voice`)は WebSocket で全員に配る。サーバーが覚えていて途中参加の人にも届く。
//! - 接続手順(offer / answer / ICE 候補)は `protocol::Signal` で相手の 1 人にだけ中継してもらう。
//! - 音声は参加者どうしで直接やり取りし、サーバーを通らない。少人数向け([`MAX_PARTICIPANTS`] 人まで)。
//! - 二重に offer しないよう、接続は参加者番号の小さい側から始める。
//! - 発話中かどうかは各自がマイクの音量から判断して `Voice` で知らせる(相手側で解析しない)。
//!
//! メッセージは描画ループを待たずに [`crate::net::Tap`] で受け取るので、タブが裏にあってもつながる。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use board_core::protocol::{Message, Signal, SignalKind, Voice};
use js_sys::{Array, JSON, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AnalyserNode, AudioContext, HtmlAudioElement, MediaStream, MediaStreamConstraints,
    MediaStreamTrack, RtcConfiguration, RtcIceCandidateInit, RtcPeerConnection,
    RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescriptionInit, RtcTrackEvent,
};

use crate::net::{NetEvent, Sender};

/// P2P メッシュで無理なく話せる人数(1 人あたりの上り回線が人数分要る)。
pub const MAX_PARTICIPANTS: usize = 8;
/// 発話とみなすマイクの音量(RMS)。
const SPEAKING_LEVEL: f32 = 0.025;
/// 音量が下がってから「話していない」にするまで(ミリ秒)。
const SPEAKING_HOLD_MS: f64 = 450.0;
/// 音量を調べる間隔(ミリ秒)。
const METER_INTERVAL_MS: i32 = 90;

/// 画面に出す通話の状態。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub in_call: bool,
    pub joining: bool,
    pub muted: bool,
    pub speaking: bool,
    /// 自分以外で通話中の人(参加者番号順)。
    pub others: Vec<Voice>,
    /// 音声がつながっている相手。
    pub connected: Vec<u32>,
    pub error: Option<String>,
}

type IceClosure = Closure<dyn FnMut(RtcPeerConnectionIceEvent)>;
type TrackClosure = Closure<dyn FnMut(RtcTrackEvent)>;
type StateClosure = Closure<dyn FnMut(web_sys::Event)>;

struct PeerLink {
    pc: RtcPeerConnection,
    audio: HtmlAudioElement,
    /// 相手の SDP を受け取る前に届いた ICE 候補。
    pending: Vec<String>,
    remote_set: bool,
    connected: bool,
    _callbacks: (IceClosure, TrackClosure, StateClosure),
}

impl PeerLink {
    fn close(&self) {
        self.pc.set_onicecandidate(None);
        self.pc.set_ontrack(None);
        self.pc.set_onconnectionstatechange(None);
        self.pc.close();
        self.audio.set_src_object(None);
        self.audio.remove();
    }
}

struct Meter {
    ctx: AudioContext,
    analyser: AnalyserNode,
    timer: i32,
    _tick: Closure<dyn FnMut()>,
}

#[derive(Default)]
struct Inner {
    me: u32,
    sender: Sender,
    in_call: bool,
    joining: bool,
    muted: bool,
    speaking: bool,
    quiet_since: Option<f64>,
    local: Option<MediaStream>,
    ice: Option<JsValue>,
    links: HashMap<u32, PeerLink>,
    /// 他の参加者の通話状態(通話中の人だけ)。
    states: HashMap<u32, Voice>,
    meter: Option<Meter>,
    error: Option<String>,
    /// 状態が変わるたびに増やす(画面の更新の目印)。
    version: u64,
}

impl Inner {
    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    fn my_voice(&self) -> Voice {
        Voice {
            peer: 0,
            in_call: self.in_call,
            muted: self.muted,
            speaking: self.in_call && !self.muted && self.speaking,
        }
    }

    fn announce(&self) {
        self.sender.send(&Message::Voice(self.my_voice()));
    }

    fn signal(&self, to: u32, kind: SignalKind, payload: String) {
        self.sender.send(&Message::Signal(Signal {
            from: 0,
            to,
            kind,
            payload,
        }));
    }

    fn drop_link(&mut self, peer: u32) {
        if let Some(l) = self.links.remove(&peer) {
            l.close();
        }
    }

    fn drop_all_links(&mut self) {
        for (_, l) in self.links.drain() {
            l.close();
        }
    }
}

/// 音声通話。複製しても同じ通話を指す。
#[derive(Clone, Default)]
pub struct VoiceCall(Rc<RefCell<Inner>>);

impl VoiceCall {
    pub fn new(sender: Sender) -> Self {
        let inner = Inner {
            sender,
            ..Default::default()
        };
        Self(Rc::new(RefCell::new(inner)))
    }

    /// WebSocket に付ける受け口。通話のメッセージはここで処理し、描画ループには渡さない。
    pub fn tap(&self) -> crate::net::Tap {
        let me = self.clone();
        Rc::new(move |e: &NetEvent| me.on_net(e))
    }

    pub fn version(&self) -> u64 {
        self.0.borrow().version
    }

    pub fn snapshot(&self) -> Snapshot {
        let i = self.0.borrow();
        let mut others: Vec<Voice> = i.states.values().copied().collect();
        others.sort_by_key(|v| v.peer);
        let mut connected: Vec<u32> = i
            .links
            .iter()
            .filter(|(_, l)| l.connected)
            .map(|(&p, _)| p)
            .collect();
        connected.sort_unstable();
        Snapshot {
            in_call: i.in_call,
            joining: i.joining,
            muted: i.muted,
            speaking: i.speaking && !i.muted,
            others,
            connected,
            error: i.error.clone(),
        }
    }

    /// 他の参加者の通話状態。
    pub fn state_of(&self, peer: u32) -> Option<Voice> {
        self.0.borrow().states.get(&peer).copied()
    }

    pub fn in_call(&self) -> bool {
        self.0.borrow().in_call
    }

    fn on_net(&self, e: &NetEvent) -> bool {
        match e {
            NetEvent::Message(Message::Sync { you, .. }) => {
                let mut i = self.0.borrow_mut();
                i.me = *you;
                // つなぎ直したときは参加者番号が変わるので、通話中なら名乗り直す
                // (つながっていた相手からは改めて offer が届く)。
                if i.in_call {
                    i.announce();
                }
                false
            }
            NetEvent::Message(Message::Leave(peer)) => {
                let mut i = self.0.borrow_mut();
                i.drop_link(*peer);
                if i.states.remove(peer).is_some() {
                    i.bump();
                }
                false
            }
            NetEvent::Message(Message::Voice(v)) => {
                self.on_voice(*v);
                true
            }
            NetEvent::Message(Message::Signal(s)) => {
                self.on_signal(s.clone());
                true
            }
            NetEvent::Closed => {
                let mut i = self.0.borrow_mut();
                i.drop_all_links();
                i.states.clear();
                i.bump();
                false
            }
            _ => false,
        }
    }

    fn on_voice(&self, v: Voice) {
        let should_offer = {
            let mut i = self.0.borrow_mut();
            if v.peer == i.me {
                return;
            }
            i.bump();
            if !v.in_call {
                i.states.remove(&v.peer);
                i.drop_link(v.peer);
                return;
            }
            i.states.insert(v.peer, v);
            i.in_call && !i.links.contains_key(&v.peer) && i.me < v.peer
        };
        if should_offer {
            self.start_offer(v.peer);
        }
    }

    // ---- 参加・退出・ミュート ----

    /// 通話に入る(マイクの許可を求める)。
    pub fn join(&self) {
        {
            let mut i = self.0.borrow_mut();
            if i.in_call || i.joining {
                return;
            }
            if i.states.len() >= MAX_PARTICIPANTS {
                i.error = Some(format!("通話は {MAX_PARTICIPANTS} 人までです"));
                i.bump();
                return;
            }
            i.joining = true;
            i.error = None;
            i.bump();
        }
        let me = self.clone();
        spawn_local(async move {
            let result = me.open_devices().await;
            let peers: Vec<u32> = {
                let mut i = me.0.borrow_mut();
                i.joining = false;
                i.bump();
                match result {
                    Ok((stream, ice)) => {
                        i.local = Some(stream);
                        i.ice = Some(ice);
                        i.in_call = true;
                        i.muted = false;
                        i.announce();
                        let my = i.me;
                        i.states
                            .values()
                            .filter(|v| v.in_call && my < v.peer)
                            .map(|v| v.peer)
                            .collect()
                    }
                    Err(e) => {
                        log::warn!("通話に入れません: {e}");
                        i.error = Some(e);
                        return;
                    }
                }
            };
            me.start_meter();
            for p in peers {
                me.start_offer(p);
            }
        });
    }

    async fn open_devices(&self) -> Result<(MediaStream, JsValue), String> {
        let window = web_sys::window().ok_or("window がありません")?;
        let devices = window.navigator().media_devices().map_err(|_| {
            "このブラウザではマイクを使えません(HTTPS か localhost で開いてください)".to_string()
        })?;
        let audio = js_sys::Object::new();
        for k in ["echoCancellation", "noiseSuppression", "autoGainControl"] {
            let _ = Reflect::set(&audio, &k.into(), &JsValue::TRUE);
        }
        let constraints = MediaStreamConstraints::new();
        constraints.set_audio(&audio);
        let promise = devices
            .get_user_media_with_constraints(&constraints)
            .map_err(|_| "マイクを使えません".to_string())?;
        let stream: MediaStream = JsFuture::from(promise)
            .await
            .map_err(|e| mic_error(&e))?
            .dyn_into()
            .map_err(|_| "マイクを使えません".to_string())?;
        // ICE サーバー(STUN / TURN)はサーバーの設定から。取れなければ公開 STUN を使う。
        let ice = match crate::http::get::<serde_json::Value>("/rtc").await {
            Ok(v) => v,
            Err(_) => {
                serde_json::json!({ "iceServers": [{ "urls": "stun:stun.l.google.com:19302" }] })
            }
        };
        let ice =
            JSON::parse(&ice.to_string()).map_err(|_| "通話の設定を読めません".to_string())?;
        Ok((stream, ice))
    }

    /// 通話をやめる。
    pub fn leave(&self) {
        let mut i = self.0.borrow_mut();
        if !i.in_call {
            return;
        }
        i.in_call = false;
        i.speaking = false;
        i.drop_all_links();
        if let Some(stream) = i.local.take() {
            for t in tracks(&stream) {
                t.stop();
            }
        }
        if let Some(m) = i.meter.take() {
            if let Some(w) = web_sys::window() {
                w.clear_interval_with_handle(m.timer);
            }
            let _ = m.ctx.close();
        }
        i.announce();
        i.bump();
    }

    pub fn toggle_mute(&self) {
        let mut i = self.0.borrow_mut();
        if !i.in_call {
            return;
        }
        i.muted = !i.muted;
        let muted = i.muted;
        if let Some(stream) = &i.local {
            for t in tracks(stream) {
                t.set_enabled(!muted);
            }
        }
        i.announce();
        i.bump();
    }

    pub fn clear_error(&self) {
        let mut i = self.0.borrow_mut();
        if i.error.take().is_some() {
            i.bump();
        }
    }

    // ---- 発話の検出 ----

    fn start_meter(&self) {
        let Some(stream) = self.0.borrow().local.clone() else {
            return;
        };
        let Ok(ctx) = AudioContext::new() else {
            return;
        };
        let Ok(source) = ctx.create_media_stream_source(&stream) else {
            return;
        };
        let Ok(analyser) = ctx.create_analyser() else {
            return;
        };
        analyser.set_fft_size(1024);
        if source.connect_with_audio_node(&analyser).is_err() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let tick = Closure::<dyn FnMut()>::new(move || {
            if let Some(inner) = weak.upgrade() {
                VoiceCall(inner).meter_tick();
            }
        });
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(timer) = window.set_interval_with_callback_and_timeout_and_arguments_0(
            tick.as_ref().unchecked_ref(),
            METER_INTERVAL_MS,
        ) else {
            return;
        };
        self.0.borrow_mut().meter = Some(Meter {
            ctx,
            analyser,
            timer,
            _tick: tick,
        });
    }

    fn meter_tick(&self) {
        let mut i = self.0.borrow_mut();
        let Some(m) = &i.meter else { return };
        let mut buf = vec![0f32; m.analyser.fft_size() as usize];
        m.analyser.get_float_time_domain_data(&mut buf);
        let level = rms(&buf);
        let now = js_sys::Date::now();
        let loud = !i.muted && level > SPEAKING_LEVEL;
        let speaking = if loud {
            i.quiet_since = None;
            true
        } else {
            let since = *i.quiet_since.get_or_insert(now);
            i.speaking && now - since < SPEAKING_HOLD_MS
        };
        if speaking != i.speaking {
            i.speaking = speaking;
            i.announce();
            i.bump();
        }
    }

    // ---- WebRTC ----

    fn new_link(&self, peer: u32) -> Option<()> {
        let mut i = self.0.borrow_mut();
        if i.links.contains_key(&peer) {
            return Some(());
        }
        let config: RtcConfiguration = i.ice.clone()?.unchecked_into();
        let pc = RtcPeerConnection::new_with_configuration(&config).ok()?;
        let stream = i.local.clone()?;
        for t in tracks(&stream) {
            pc.add_track(&t, &stream, &Array::new());
        }
        let audio = HtmlAudioElement::new().ok()?;
        audio.set_autoplay(true);
        let _ = audio.set_attribute("data-peer", &peer.to_string());
        if let Some(body) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.body())
        {
            let _ = body.append_child(&audio);
        }

        let weak = Rc::downgrade(&self.0);
        let on_ice = Closure::<dyn FnMut(RtcPeerConnectionIceEvent)>::new(
            move |e: RtcPeerConnectionIceEvent| {
                let (Some(c), Some(inner)) = (e.candidate(), weak.upgrade()) else {
                    return;
                };
                let json = JSON::stringify(&c.to_json())
                    .ok()
                    .and_then(|s| s.as_string());
                if let Some(json) = json {
                    inner.borrow().signal(peer, SignalKind::Candidate, json);
                }
            },
        );
        let audio2 = audio.clone();
        let on_track = Closure::<dyn FnMut(RtcTrackEvent)>::new(move |e: RtcTrackEvent| {
            if let Ok(stream) = e.streams().get(0).dyn_into::<MediaStream>() {
                audio2.set_src_object(Some(&stream));
                if let Ok(p) = audio2.play() {
                    // 自動再生が止められても、次の操作で鳴るので気にしない。
                    spawn_local(async move {
                        let _ = JsFuture::from(p).await;
                    });
                }
            }
        });
        let weak = Rc::downgrade(&self.0);
        let pc2 = pc.clone();
        let on_state = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let state = pc2.connection_state();
            let mut i = inner.borrow_mut();
            use web_sys::RtcPeerConnectionState as S;
            match state {
                S::Connected => {
                    if let Some(l) = i.links.get_mut(&peer) {
                        l.connected = true;
                    }
                    i.bump();
                }
                S::Failed | S::Closed => {
                    // この処理の中で自分(コールバック)を捨てられないので、後で片付ける。
                    drop(i);
                    let inner = inner.clone();
                    spawn_local(async move {
                        let mut i = inner.borrow_mut();
                        i.drop_link(peer);
                        i.bump();
                    });
                }
                S::Disconnected => {
                    if let Some(l) = i.links.get_mut(&peer) {
                        l.connected = false;
                    }
                    i.bump();
                }
                _ => {}
            }
        });
        pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));
        pc.set_ontrack(Some(on_track.as_ref().unchecked_ref()));
        pc.set_onconnectionstatechange(Some(on_state.as_ref().unchecked_ref()));
        i.links.insert(
            peer,
            PeerLink {
                pc,
                audio,
                pending: Vec::new(),
                remote_set: false,
                connected: false,
                _callbacks: (on_ice, on_track, on_state),
            },
        );
        i.bump();
        Some(())
    }

    fn pc(&self, peer: u32) -> Option<RtcPeerConnection> {
        self.0.borrow().links.get(&peer).map(|l| l.pc.clone())
    }

    fn start_offer(&self, peer: u32) {
        if self.new_link(peer).is_none() {
            return;
        }
        let Some(pc) = self.pc(peer) else { return };
        let me = self.clone();
        spawn_local(async move {
            let result: Result<(), JsValue> = async {
                let offer = JsFuture::from(pc.create_offer()).await?;
                let sdp = sdp_of(&offer);
                let desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
                desc.set_sdp(&sdp);
                JsFuture::from(pc.set_local_description(&desc)).await?;
                me.0.borrow().signal(peer, SignalKind::Offer, sdp);
                Ok(())
            }
            .await;
            if let Err(e) = result {
                log::warn!("通話の接続を始められません: {e:?}");
                me.0.borrow_mut().drop_link(peer);
            }
        });
    }

    fn on_signal(&self, s: Signal) {
        if !self.in_call() {
            return;
        }
        let peer = s.from;
        match s.kind {
            SignalKind::Offer => {
                // 相手が通話に入り直したときは古い接続を捨てる。
                self.0.borrow_mut().drop_link(peer);
                if self.new_link(peer).is_none() {
                    return;
                }
                let Some(pc) = self.pc(peer) else { return };
                let me = self.clone();
                spawn_local(async move {
                    let result: Result<(), JsValue> = async {
                        let desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
                        desc.set_sdp(&s.payload);
                        JsFuture::from(pc.set_remote_description(&desc)).await?;
                        me.flush_candidates(peer, &pc).await;
                        let answer = JsFuture::from(pc.create_answer()).await?;
                        let sdp = sdp_of(&answer);
                        let desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                        desc.set_sdp(&sdp);
                        JsFuture::from(pc.set_local_description(&desc)).await?;
                        me.0.borrow().signal(peer, SignalKind::Answer, sdp);
                        Ok(())
                    }
                    .await;
                    if let Err(e) = result {
                        log::warn!("通話の応答に失敗: {e:?}");
                        me.0.borrow_mut().drop_link(peer);
                    }
                });
            }
            SignalKind::Answer => {
                let Some(pc) = self.pc(peer) else { return };
                let me = self.clone();
                spawn_local(async move {
                    let desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                    desc.set_sdp(&s.payload);
                    match JsFuture::from(pc.set_remote_description(&desc)).await {
                        Ok(_) => me.flush_candidates(peer, &pc).await,
                        Err(e) => log::warn!("通話の応答を受け取れません: {e:?}"),
                    }
                });
            }
            SignalKind::Candidate => {
                let ready = {
                    let mut i = self.0.borrow_mut();
                    let Some(l) = i.links.get_mut(&peer) else {
                        return;
                    };
                    if !l.remote_set {
                        l.pending.push(s.payload.clone());
                        return;
                    }
                    l.pc.clone()
                };
                spawn_local(async move {
                    add_candidate(&ready, &s.payload).await;
                });
            }
        }
    }

    /// 相手の SDP を受け取ったので、ためていた ICE 候補を渡す。
    async fn flush_candidates(&self, peer: u32, pc: &RtcPeerConnection) {
        let pending = {
            let mut i = self.0.borrow_mut();
            let Some(l) = i.links.get_mut(&peer) else {
                return;
            };
            l.remote_set = true;
            std::mem::take(&mut l.pending)
        };
        for c in pending {
            add_candidate(pc, &c).await;
        }
    }
}

async fn add_candidate(pc: &RtcPeerConnection, json: &str) {
    let Ok(init) = JSON::parse(json) else { return };
    let init: RtcIceCandidateInit = init.unchecked_into();
    if let Err(e) =
        JsFuture::from(pc.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&init))).await
    {
        log::debug!("ICE 候補を追加できません: {e:?}");
    }
}

fn sdp_of(desc: &JsValue) -> String {
    Reflect::get(desc, &"sdp".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

fn tracks(stream: &MediaStream) -> Vec<MediaStreamTrack> {
    stream
        .get_audio_tracks()
        .iter()
        .filter_map(|t| t.dyn_into().ok())
        .collect()
}

fn mic_error(e: &JsValue) -> String {
    let name = Reflect::get(e, &"name".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    match name.as_str() {
        "NotAllowedError" | "SecurityError" => {
            "マイクの使用が許可されていません(ブラウザのアドレスバーから許可できます)".into()
        }
        "NotFoundError" | "OverconstrainedError" => "マイクが見つかりません".into(),
        "NotReadableError" => "マイクを他のアプリが使っています".into(),
        _ => "マイクを使えません".into(),
    }
}

/// 音量(二乗平均平方根)。
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_levels() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0.0; 16]), 0.0);
        assert!((rms(&[0.5, -0.5, 0.5, -0.5]) - 0.5).abs() < 1e-6);
        assert!(rms(&[0.01; 64]) < SPEAKING_LEVEL);
    }
}
