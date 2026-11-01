//! ブラウザ内で動く音声認識(Whisper)。音声をサーバーに送らずに文字起こしする。
//!
//! candle(Rust の機械学習ライブラリ)は wasm を数 MB 大きくするので、本体とは別の wasm にして
//! Web Worker で動かす。本体は「ブラウザ内 AI」で音声認識を使い始めたときに初めてこの Worker を起こす。
//! 推論も本体のスレッドの外で走るので、文字起こし中も画面は止まらない。
//!
//! Worker とのやりとり(すべて `postMessage` のオブジェクト):
//! - 起動すると `{ ready: true }` を送る
//! - `{ type: "load", config, tokenizer, weights }`(どれも `Uint8Array`)→ `{ loaded: true }`
//! - `{ type: "transcribe", id, pcm: Float32Array(16 kHz), language: "ja" | null }`
//!   → `{ id, text, language, noSpeech, ms }`
//! - 失敗したら `{ id?, error: "…" }`

pub mod mel;
pub mod tokenizer;
pub mod whisper;

pub use whisper::{Transcript, Whisper};

#[cfg(target_arch = "wasm32")]
mod worker {
    use std::cell::RefCell;

    use js_sys::{Float32Array, Object, Reflect, Uint8Array};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

    use crate::Whisper;

    thread_local! {
        static MODEL: RefCell<Option<Whisper>> = const { RefCell::new(None) };
    }

    fn get(o: &JsValue, key: &str) -> JsValue {
        Reflect::get(o, &key.into()).unwrap_or(JsValue::UNDEFINED)
    }

    fn set(o: &Object, key: &str, v: impl Into<JsValue>) {
        let _ = Reflect::set(o, &key.into(), &v.into());
    }

    fn bytes(o: &JsValue, key: &str) -> Vec<u8> {
        Uint8Array::new(&get(o, key)).to_vec()
    }

    fn now() -> f64 {
        js_sys::Date::now()
    }

    fn handle(data: &JsValue) -> Object {
        let reply = Object::new();
        let id = get(data, "id");
        if !id.is_undefined() {
            set(&reply, "id", id);
        }
        let kind = get(data, "type").as_string().unwrap_or_default();
        let result: Result<(), String> = match kind.as_str() {
            "load" => {
                // 先に古いモデルを捨てる(大きいモデルに替えるときに 2 つ分のメモリを使わない)。
                MODEL.with(|m| m.borrow_mut().take());
                Whisper::load(
                    &bytes(data, "config"),
                    &bytes(data, "tokenizer"),
                    bytes(data, "weights"),
                )
                .map(|model| {
                    MODEL.with(|m| *m.borrow_mut() = Some(model));
                    set(&reply, "loaded", true);
                })
            }
            "transcribe" => {
                let pcm = Float32Array::new(&get(data, "pcm")).to_vec();
                let language = get(data, "language").as_string().filter(|l| !l.is_empty());
                let start = now();
                MODEL.with(|m| match m.borrow_mut().as_mut() {
                    Some(model) => model.transcribe(&pcm, language.as_deref()).map(|t| {
                        set(&reply, "text", t.text);
                        set(&reply, "language", t.language);
                        set(&reply, "noSpeech", t.no_speech);
                        set(&reply, "ms", now() - start);
                    }),
                    None => Err("モデルを読み込んでいません".into()),
                })
            }
            "unload" => {
                MODEL.with(|m| m.borrow_mut().take());
                Ok(())
            }
            other => Err(format!("知らない指示です: {other}")),
        };
        if let Err(e) = result {
            set(&reply, "error", e);
        }
        reply
    }

    #[wasm_bindgen(start)]
    pub fn start() {
        console_error_panic_hook_lite();
        let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
        let reply_to = scope.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let reply = handle(&e.data());
            let _ = reply_to.post_message(&reply);
        });
        scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        on_message.forget();
        let ready = Object::new();
        set(&ready, "ready", true);
        let _ = scope.post_message(&ready);
    }

    /// パニックしたら内容を本体に知らせる(Worker は黙って止まるので)。
    fn console_error_panic_hook_lite() {
        std::panic::set_hook(Box::new(|info| {
            let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
            let o = Object::new();
            set(&o, "error", format!("ブラウザ内 AI が止まりました: {info}"));
            set(&o, "fatal", true);
            let _ = scope.post_message(&o);
        }));
    }
}
