//! 自分のマイクの声を文字にする(ブラウザの音声認識 `SpeechRecognition`)。
//!
//! - 参加者それぞれが自分の声だけを文字にし、ボードの共有状態(`board_core::meeting`)に足す。
//!   だれが話したかはその人の名前になるので、話者を推定しなくてよい。
//! - 音声通話([`crate::voice`])は P2P でサーバーを通らないので、文字起こしも各自のブラウザで行う。
//! - 話し終えた(確定した)文だけを共有し、話している途中の文は自分の画面にだけ出す。
//! - ブラウザは無音が続くと認識を止めるので、使っている間は自動でかけ直す。
//!
//! Chrome / Edge / Safari で使える(Chrome では音声がブラウザの提供元のサーバーで認識される)。
//! web-sys の該当 API は不安定扱いなので `Reflect` で呼ぶ。

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// かけ直すまでの待ち時間(ミリ秒。すぐだと失敗するブラウザがある)。
const RESTART_DELAY_MS: i32 = 300;

#[derive(Default)]
struct Inner {
    rec: Option<JsValue>,
    /// 使っている(止められるまでかけ直す)。
    on: bool,
    lang: String,
    /// 話している途中の文。
    interim: String,
    /// 確定した文と時刻(まだ共有していないもの)。
    finals: Vec<(String, f64)>,
    error: Option<String>,
    version: u64,
    callbacks: Vec<Closure<dyn FnMut(JsValue)>>,
}

impl Inner {
    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }
}

#[derive(Clone, Default)]
pub struct Transcriber {
    inner: Rc<RefCell<Inner>>,
}

fn constructor() -> Option<Function> {
    let w = web_sys::window()?;
    ["SpeechRecognition", "webkitSpeechRecognition"]
        .iter()
        .filter_map(|n| Reflect::get(&w, &JsValue::from_str(n)).ok())
        .find_map(|v| v.dyn_into::<Function>().ok())
}

fn get(v: &JsValue, key: &str) -> JsValue {
    Reflect::get(v, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn call0(target: &JsValue, method: &str) -> Result<(), JsValue> {
    let f: Function = get(target, method).dyn_into()?;
    f.call0(target).map(|_| ())
}

/// 認識の失敗の理由を画面向けの文にする。`None` は気にしなくてよいもの(無音など)。
pub fn error_message(code: &str) -> Option<&'static str> {
    match code {
        "not-allowed" | "service-not-allowed" => {
            Some("マイクの使用が許可されていません。ブラウザの設定で許可してください")
        }
        "audio-capture" => Some("マイクが見つかりません"),
        "network" => Some("音声認識のサービスにつながりません"),
        "language-not-supported" => Some("この言語の音声認識に対応していません"),
        _ => None,
    }
}

impl Transcriber {
    /// このブラウザで使えるか。
    pub fn supported() -> bool {
        constructor().is_some()
    }

    pub fn is_on(&self) -> bool {
        self.inner.borrow().on
    }

    /// 状態が変わるたびに増える(画面の更新の目印)。
    pub fn version(&self) -> u64 {
        self.inner.borrow().version
    }

    pub fn interim(&self) -> String {
        self.inner.borrow().interim.clone()
    }

    pub fn error(&self) -> Option<String> {
        self.inner.borrow().error.clone()
    }

    /// 確定した文を取り出す(取り出したものは消える)。
    pub fn take_finals(&self) -> Vec<(String, f64)> {
        std::mem::take(&mut self.inner.borrow_mut().finals)
    }

    /// 文字起こしを始める。`lang` は `ja-JP` など。
    pub fn start(&self, lang: &str) -> Result<(), String> {
        if self.is_on() {
            return Ok(());
        }
        let ctor = constructor().ok_or(
            "このブラウザは音声の文字起こしに対応していません(Chrome・Edge・Safari で使えます)",
        )?;
        let rec = Reflect::construct(&ctor, &js_sys::Array::new())
            .map_err(|_| "音声認識を始められません".to_string())?;
        let set = |k: &str, v: JsValue| {
            let _ = Reflect::set(&rec, &JsValue::from_str(k), &v);
        };
        set("lang", JsValue::from_str(lang));
        set("continuous", JsValue::TRUE);
        set("interimResults", JsValue::TRUE);
        set("maxAlternatives", JsValue::from_f64(1.0));

        let weak = Rc::downgrade(&self.inner);
        let on_result = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
            let Some(inner) = weak.upgrade() else { return };
            let mut inner = inner.borrow_mut();
            let results = get(&e, "results");
            let from = get(&e, "resultIndex").as_f64().unwrap_or(0.0) as u32;
            let len = get(&results, "length").as_f64().unwrap_or(0.0) as u32;
            let mut interim = String::new();
            for i in from..len {
                let r = Reflect::get_u32(&results, i).unwrap_or(JsValue::UNDEFINED);
                let alt = Reflect::get_u32(&r, 0).unwrap_or(JsValue::UNDEFINED);
                let text = get(&alt, "transcript").as_string().unwrap_or_default();
                if get(&r, "isFinal").as_bool().unwrap_or(false) {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        inner.finals.push((text, js_sys::Date::now()));
                    }
                } else {
                    interim.push_str(&text);
                }
            }
            inner.interim = interim.trim().to_string();
            inner.bump();
        });
        let weak = Rc::downgrade(&self.inner);
        let on_error = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
            let Some(inner) = weak.upgrade() else { return };
            let code = get(&e, "error").as_string().unwrap_or_default();
            log::warn!("音声認識: {code}");
            if let Some(msg) = error_message(&code) {
                let mut inner = inner.borrow_mut();
                inner.error = Some(msg.to_string());
                // 許可がない・マイクがないときはかけ直しても無駄なので止める。
                if code != "network" {
                    inner.on = false;
                }
                inner.bump();
            }
        });
        let weak = Rc::downgrade(&self.inner);
        let on_end = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = Transcriber { inner };
            {
                let mut i = this.inner.borrow_mut();
                i.interim.clear();
                i.bump();
                if !i.on {
                    i.rec = None;
                    return;
                }
            }
            // 無音などで止まったので、少し待ってかけ直す。
            let again = Closure::once_into_js(move || this.restart());
            if let Some(w) = web_sys::window() {
                let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    again.unchecked_ref(),
                    RESTART_DELAY_MS,
                );
            }
        });
        set("onresult", on_result.as_ref().clone());
        set("onerror", on_error.as_ref().clone());
        set("onend", on_end.as_ref().clone());
        call0(&rec, "start").map_err(|_| "音声認識を始められません".to_string())?;
        let mut inner = self.inner.borrow_mut();
        inner.rec = Some(rec);
        inner.on = true;
        inner.lang = lang.to_string();
        inner.error = None;
        inner.callbacks = vec![on_result, on_error, on_end];
        inner.bump();
        Ok(())
    }

    fn restart(&self) {
        let rec = {
            let i = self.inner.borrow();
            if !i.on {
                return;
            }
            i.rec.clone()
        };
        if let Some(rec) = rec
            && call0(&rec, "start").is_err()
        {
            let mut i = self.inner.borrow_mut();
            i.on = false;
            i.error = Some("文字起こしが止まりました。もう一度始めてください".into());
            i.bump();
        }
    }

    /// 止める(話している途中の文は、確定すれば届く)。
    pub fn stop(&self) {
        let rec = {
            let mut i = self.inner.borrow_mut();
            i.on = false;
            i.interim.clear();
            i.bump();
            i.rec.clone()
        };
        if let Some(rec) = rec {
            let _ = call0(&rec, "stop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_worth_showing() {
        assert!(error_message("not-allowed").is_some());
        assert!(error_message("no-speech").is_none());
        assert!(error_message("aborted").is_none());
    }
}
