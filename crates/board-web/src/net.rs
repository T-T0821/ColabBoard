//! サーバーとの WebSocket 接続。受け取ったものは `poll` で取り出す(描画ループから呼ぶ)。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use board_core::protocol::Message;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{BinaryType, MessageEvent, WebSocket};

pub enum NetEvent {
    Open,
    Message(Message),
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Online,
    /// 一度つながった後に切れて、つなぎ直そうとしている。
    Reconnecting,
    /// サーバーがない(最初の接続に失敗した)ので、このタブだけで動かしている。
    Offline,
    /// ダミー表示などで接続を止めた。
    Disabled,
}

type Callbacks = (
    Closure<dyn FnMut(web_sys::Event)>,
    Closure<dyn FnMut(MessageEvent)>,
    Closure<dyn FnMut(web_sys::Event)>,
);

/// 描画ループを待たずに届いたその場で受け取る処理(音声通話の接続手順など)。
/// `true` を返したメッセージは `poll` には渡さない。
pub type Tap = Rc<dyn Fn(&NetEvent) -> bool>;

/// 描画ループの外から送るための口(音声通話用)。今つながっているソケットに送る。
#[derive(Clone, Default)]
pub struct Sender(Rc<RefCell<Option<WebSocket>>>);

impl Sender {
    pub fn send(&self, m: &Message) {
        if let Some(ws) = self.0.borrow().as_ref()
            && ws.ready_state() == WebSocket::OPEN
            && let Err(e) = ws.send_with_u8_array(&m.encode())
        {
            log::warn!("送信できません: {e:?}");
        }
    }
}

pub struct Net {
    url: String,
    ws: Option<WebSocket>,
    sender: Sender,
    tap: Rc<RefCell<Option<Tap>>>,
    callbacks: Option<Callbacks>,
    inbox: Rc<RefCell<VecDeque<NetEvent>>>,
    pub status: Status,
    /// 一度でも初回同期まで済んだか。
    pub synced_once: bool,
    retry_at: Option<f64>,
    retry_delay: f64,
}

impl Net {
    pub fn new(url: String) -> Self {
        Self {
            url,
            ws: None,
            sender: Sender::default(),
            tap: Rc::default(),
            callbacks: None,
            inbox: Rc::default(),
            status: Status::Connecting,
            synced_once: false,
            retry_at: None,
            retry_delay: 1000.0,
        }
    }

    pub fn connect(&mut self) {
        self.drop_socket();
        let ws = match WebSocket::new(&self.url) {
            Ok(ws) => ws,
            Err(e) => {
                log::warn!("WebSocket を作れません: {e:?}");
                self.inbox.borrow_mut().push_back(NetEvent::Closed);
                return;
            }
        };
        ws.set_binary_type(BinaryType::Arraybuffer);

        let inbox = self.inbox.clone();
        let tap = self.tap.clone();
        let on_open = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            inbox.borrow_mut().push_back(NetEvent::Open);
        });
        let inbox = self.inbox.clone();
        let tap2 = tap.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let Ok(buf) = e.data().dyn_into::<js_sys::ArrayBuffer>() else {
                return;
            };
            let bytes = js_sys::Uint8Array::new(&buf).to_vec();
            match Message::decode(&bytes) {
                Ok(m) => {
                    let e = NetEvent::Message(m);
                    let tapped = tap2.borrow().clone();
                    if !tapped.is_some_and(|t| t(&e)) {
                        inbox.borrow_mut().push_back(e);
                    }
                }
                Err(e) => log::warn!("読めないメッセージ: {e}"),
            }
        });
        let inbox = self.inbox.clone();
        let on_close = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let tapped = tap.borrow().clone();
            if let Some(t) = tapped {
                t(&NetEvent::Closed);
            }
            inbox.borrow_mut().push_back(NetEvent::Closed);
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        self.callbacks = Some((on_open, on_message, on_close));
        *self.sender.0.borrow_mut() = Some(ws.clone());
        self.ws = Some(ws);
    }

    /// 描画ループの外から送るための口。
    pub fn sender(&self) -> Sender {
        self.sender.clone()
    }

    /// 届いたその場で受け取る処理を付ける。
    pub fn set_tap(&mut self, tap: Tap) {
        *self.tap.borrow_mut() = Some(tap);
    }

    fn drop_socket(&mut self) {
        *self.sender.0.borrow_mut() = None;
        if let Some(ws) = self.ws.take() {
            ws.set_onopen(None);
            ws.set_onmessage(None);
            ws.set_onclose(None);
            let _ = ws.close();
        }
        self.callbacks = None;
    }

    /// 接続をやめる(以後つなぎ直さない)。
    pub fn disable(&mut self) {
        self.drop_socket();
        self.inbox.borrow_mut().clear();
        self.status = Status::Disabled;
        self.retry_at = None;
    }

    pub fn is_open(&self) -> bool {
        self.ws
            .as_ref()
            .is_some_and(|ws| ws.ready_state() == WebSocket::OPEN)
    }

    pub fn send(&self, m: &Message) {
        if let Some(ws) = &self.ws
            && ws.ready_state() == WebSocket::OPEN
            && let Err(e) = ws.send_with_u8_array(&m.encode())
        {
            log::warn!("送信できません: {e:?}");
        }
    }

    /// 届いたものを取り出す。切断時のつなぎ直しもここで行う。
    pub fn poll(&mut self, now: f64) -> Vec<NetEvent> {
        let events: Vec<NetEvent> = self.inbox.borrow_mut().drain(..).collect();
        for e in &events {
            match e {
                NetEvent::Open => {}
                NetEvent::Message(Message::Sync { .. } | Message::SealedSync { .. }) => {
                    self.status = Status::Online;
                    self.synced_once = true;
                    self.retry_delay = 1000.0;
                }
                NetEvent::Message(_) => {}
                NetEvent::Closed => {
                    self.drop_socket();
                    if self.status == Status::Disabled {
                        continue;
                    }
                    if self.synced_once {
                        self.status = Status::Reconnecting;
                    } else {
                        // サーバーが立ち上がるのを待ちつつ、手元で編集を続けられるようにする。
                        self.status = Status::Offline;
                    }
                    self.retry_at = Some(now + self.retry_delay);
                    self.retry_delay = (self.retry_delay * 2.0).min(15_000.0);
                }
            }
        }
        if let Some(at) = self.retry_at
            && now >= at
        {
            self.retry_at = None;
            self.connect();
        }
        events
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.drop_socket();
    }
}
