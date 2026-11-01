//! ブラウザ内 AI: 音声も手書きもサーバーに送らずに、この端末だけで読む。
//!
//! - **音声認識**: Whisper(`crates/board-ai`、candle の wasm)を Web Worker で動かす。
//!   モデル(`config.json`・`tokenizer.json`・`model.safetensors`)は初回に取得して
//!   Cache Storage に置き、次からはネットワークなしで使える。取得元はサーバーが
//!   `GET /api/ai/status` の `local_model_url` で指定できる(社内に置いたモデルを使うため)。
//!   音声はマイクから端末内で切り出し([`Segmenter`])、Worker に渡して文字にする。
//! - **手書き文字認識**: [`board_core::handwriting`]。本体の wasm に入っている。
//!   利用者が教えた字の見本は localStorage に置く。
//! - **クラウド AI を使わない**: 端末ごとの設定。オンにすると、サーバーの Claude を呼ぶ
//!   機能(要約・図の生成・議事録など)は送る前に止め、「AI で整形」は端末内の認識に切り替える。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use board_core::handwriting::{Recognizer, Sample};
use js_sys::{Float32Array, Function, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

pub use crate::speech::{DEFAULT_MODEL_URL, Model, Segmenter};

/// Worker の読み込み口(trunk が `crates/board-ai` から作る)。
const WORKER_URL: &str = "board-ai_loader.js";

const KEY_CLOUD_OFF: &str = "colabboard.localai.cloudOff";
const KEY_MODEL: &str = "colabboard.localai.model";
const KEY_LANG: &str = "colabboard.localai.lang";
const KEY_SAMPLES: &str = "colabboard.handwriting.samples";
/// 覚えておく手書きの見本の上限(古いものから捨てる)。
pub const MAX_SAMPLES: usize = 600;

#[wasm_bindgen(inline_js = r#"
const CACHE = 'colabboard-local-ai-v1';
let worker = null, ready = null, seq = 0;
const pending = new Map();
function failAll(msg) {
  for (const [, p] of pending) p.reject(msg);
  pending.clear();
}
export function lai_start(url) {
  if (ready) return ready;
  ready = new Promise((resolve, reject) => {
    try { worker = new Worker(url); } catch (e) { ready = null; reject(String(e)); return; }
    worker.onmessage = (e) => {
      const d = e.data || {};
      if (d.ready) { resolve(); return; }
      if (d.fatal) {
        failAll(d.error);
        worker.terminate(); worker = null; ready = null;
        return;
      }
      const p = pending.get(d.id);
      if (!p) return;
      pending.delete(d.id);
      if (d.error) p.reject(d.error); else p.resolve(d);
    };
    worker.onerror = (e) => {
      e.preventDefault();
      const msg = 'ブラウザ内 AI の部品を起動できません';
      failAll(msg);
      if (worker) worker.terminate();
      worker = null; ready = null;
      reject(msg);
    };
  });
  return ready;
}
export function lai_call(msg, transfer) {
  return new Promise((resolve, reject) => {
    if (!worker) { reject('ブラウザ内 AI が起動していません'); return; }
    const id = ++seq;
    pending.set(id, { resolve, reject });
    msg.id = id;
    worker.postMessage(msg, transfer || []);
  });
}
export function lai_stop() {
  failAll('止めました');
  if (worker) worker.terminate();
  worker = null; ready = null;
}
export async function lai_fetch(url, onprogress) {
  let cache = null;
  try { cache = await caches.open(CACHE); } catch (e) {}
  if (cache) {
    const hit = await cache.match(url);
    if (hit) {
      const b = new Uint8Array(await hit.arrayBuffer());
      onprogress(b.length, b.length);
      return b;
    }
  }
  let r;
  try { r = await fetch(url, { credentials: 'omit' }); } catch (e) { throw 'モデルを取得できません: ' + url; }
  if (!r.ok) throw 'モデルを取得できません(HTTP ' + r.status + '): ' + url;
  const total = Number(r.headers.get('content-length')) || 0;
  const reader = r.body.getReader();
  const chunks = [];
  let got = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    got += value.length;
    onprogress(got, total);
  }
  const out = new Uint8Array(got);
  let o = 0;
  for (const c of chunks) { out.set(c, o); o += c.length; }
  if (cache) { try { await cache.put(url, new Response(out)); } catch (e) {} }
  return out;
}
export async function lai_cached(urls) {
  try {
    const c = await caches.open(CACHE);
    for (const u of urls) if (!(await c.match(u))) return false;
    return true;
  } catch (e) { return false; }
}
export async function lai_clear() {
  try { await caches.delete(CACHE); } catch (e) {}
}
let mic = null;
export async function lai_mic_start(onchunk) {
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: true, noiseSuppression: true, channelCount: 1 },
  });
  const ctx = new AudioContext();
  const src = ctx.createMediaStreamSource(stream);
  const node = ctx.createScriptProcessor(4096, 1, 1);
  node.onaudioprocess = (e) => onchunk(new Float32Array(e.inputBuffer.getChannelData(0)), ctx.sampleRate);
  src.connect(node);
  node.connect(ctx.destination);
  mic = { stream, ctx, node, src };
}
export function lai_mic_stop() {
  if (!mic) return;
  try { mic.node.disconnect(); mic.src.disconnect(); } catch (e) {}
  mic.stream.getTracks().forEach((t) => t.stop());
  mic.ctx.close().catch(() => {});
  mic = null;
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn lai_start(url: &str) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    fn lai_call(msg: &Object, transfer: &js_sys::Array) -> Result<js_sys::Promise, JsValue>;
    fn lai_stop();
    #[wasm_bindgen(catch)]
    fn lai_fetch(url: &str, onprogress: &Function) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    fn lai_cached(urls: &js_sys::Array) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    fn lai_clear() -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    fn lai_mic_start(onchunk: &Function) -> Result<js_sys::Promise, JsValue>;
    fn lai_mic_stop();
}

fn js_msg(e: JsValue) -> String {
    e.as_string()
        .or_else(|| {
            Reflect::get(&e, &"message".into())
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| "ブラウザ内 AI でエラーが起きました".into())
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn get_pref(key: &str) -> Option<String> {
    storage()?.get_item(key).ok().flatten()
}

fn set_pref(key: &str, value: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(key, value);
    }
}

/// クラウド AI(サーバーの Claude)を使わない設定か。
pub fn cloud_off() -> bool {
    get_pref(KEY_CLOUD_OFF).as_deref() == Some("1")
}

pub fn set_cloud_off(on: bool) {
    set_pref(KEY_CLOUD_OFF, if on { "1" } else { "0" });
}

/// クラウド AI を止めているときのお知らせ。
pub const CLOUD_OFF_MESSAGE: &str =
    "クラウド AI を使わない設定です。「ブラウザ内 AI」で切り替えられます";

pub fn model() -> Model {
    get_pref(KEY_MODEL)
        .and_then(|m| Model::from_id(&m))
        .unwrap_or(Model::Base)
}

pub fn set_model(m: Model) {
    set_pref(KEY_MODEL, m.id());
}

/// 文字起こしの言語。空なら自動。既定は画面の言語。
pub fn language() -> String {
    get_pref(KEY_LANG).unwrap_or_else(|| crate::i18n::lang().code().to_string())
}

pub fn set_language(l: &str) {
    set_pref(KEY_LANG, l);
}

// ---- 手書きの見本 ----

thread_local! {
    static RECOGNIZER: RefCell<Option<Rc<Recognizer>>> = const { RefCell::new(None) };
}

/// 教えた手書きの見本。
pub fn samples() -> Vec<Sample> {
    get_pref(KEY_SAMPLES)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_samples(list: &[Sample]) {
    if let Ok(json) = serde_json::to_string(list) {
        set_pref(KEY_SAMPLES, &json);
    }
    RECOGNIZER.with(|r| *r.borrow_mut() = None);
}

/// 見本を足す(多すぎたら古いものから捨てる)。
pub fn add_samples(new: Vec<Sample>) {
    let mut list = samples();
    list.extend(new);
    if list.len() > MAX_SAMPLES {
        list.drain(..list.len() - MAX_SAMPLES);
    }
    save_samples(&list);
}

pub fn clear_samples() {
    save_samples(&[]);
}

/// 組み込みの見本 + 教えた見本の認識器(見本が変わるまで使い回す)。
pub fn recognizer() -> Rc<Recognizer> {
    RECOGNIZER.with(|r| {
        r.borrow_mut()
            .get_or_insert_with(|| Rc::new(Recognizer::with_samples(&samples())))
            .clone()
    })
}

// ---- 音声認識 ----

/// 音声認識の状態。
#[derive(Clone, Debug, PartialEq)]
pub enum Speech {
    /// まだ読み込んでいない。
    Idle,
    /// モデルを取得中(取得済みのバイト数, 全体)。
    Downloading(f64, f64),
    /// Worker でモデルを読み込み中。
    Loading,
    Ready(Model),
    Failed(String),
}

thread_local! {
    static SPEECH: RefCell<Speech> = const { RefCell::new(Speech::Idle) };
    static MODEL_URL: RefCell<String> = RefCell::new(DEFAULT_MODEL_URL.to_string());
    static LISTENERS: RefCell<Vec<Rc<dyn Fn()>>> = const { RefCell::new(Vec::new()) };
}

pub fn speech() -> Speech {
    SPEECH.with(|s| s.borrow().clone())
}

fn set_speech(s: Speech) {
    SPEECH.with(|x| *x.borrow_mut() = s);
    let ls: Vec<Rc<dyn Fn()>> = LISTENERS.with(|l| l.borrow().clone());
    for l in ls {
        l();
    }
}

/// 状態が変わったら呼ぶ関数を登録する(画面の更新用)。
pub fn on_change(f: Rc<dyn Fn()>) {
    LISTENERS.with(|l| l.borrow_mut().push(f));
}

/// サーバーが指定したモデルの取得元(`{model}` を含む URL)。
pub fn set_model_url(url: &str) {
    if !url.trim().is_empty() {
        MODEL_URL.with(|u| *u.borrow_mut() = url.trim().to_string());
    }
}

/// モデルのファイルの URL(config, tokenizer, weights)。
pub fn model_files(m: Model) -> [String; 3] {
    MODEL_URL.with(|u| crate::speech::model_files(&u.borrow(), m))
}

/// モデルがこの端末に保存済みか。
pub async fn is_cached(m: Model) -> bool {
    let urls = js_sys::Array::new();
    for u in model_files(m) {
        urls.push(&u.into());
    }
    match lai_cached(&urls) {
        Ok(p) => JsFuture::from(p)
            .await
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// 保存したモデルを消し、Worker も止める。
pub async fn forget_models() {
    lai_stop();
    if let Ok(p) = lai_clear() {
        let _ = JsFuture::from(p).await;
    }
    set_speech(Speech::Idle);
}

async fn fetch_bytes(url: &str, progress: &Function) -> Result<Uint8Array, String> {
    let p = lai_fetch(url, progress).map_err(js_msg)?;
    let v = JsFuture::from(p).await.map_err(js_msg)?;
    Ok(Uint8Array::new(&v))
}

thread_local! {
    static LOADING: Cell<bool> = const { Cell::new(false) };
}

/// モデルを取得して Worker に読み込む(取得済みならすぐ)。
pub async fn load(m: Model) -> Result<(), String> {
    if speech() == Speech::Ready(m) {
        return Ok(());
    }
    if LOADING.with(|l| l.replace(true)) {
        return Err("モデルを読み込み中です".into());
    }
    let result = load_inner(m).await;
    LOADING.with(|l| l.set(false));
    match &result {
        Ok(()) => set_speech(Speech::Ready(m)),
        Err(e) => set_speech(Speech::Failed(e.clone())),
    }
    result
}

async fn load_inner(m: Model) -> Result<(), String> {
    JsFuture::from(lai_start(WORKER_URL).map_err(js_msg)?)
        .await
        .map_err(js_msg)?;
    let [config_url, tok_url, weights_url] = model_files(m);
    // 進み具合は 3 つのファイルの合計で出す(重みがほとんど)。
    let done = Rc::new(Cell::new(0f64));
    let make_progress = |offset: Rc<Cell<f64>>| {
        Closure::<dyn FnMut(f64, f64)>::new(move |got: f64, total: f64| {
            let base = offset.get();
            set_speech(Speech::Downloading(base + got, base + total.max(got)));
        })
    };
    set_speech(Speech::Downloading(0.0, 0.0));
    let p1 = make_progress(done.clone());
    let config = fetch_bytes(&config_url, p1.as_ref().unchecked_ref()).await?;
    done.set(done.get() + config.length() as f64);
    let p2 = make_progress(done.clone());
    let tokenizer = fetch_bytes(&tok_url, p2.as_ref().unchecked_ref()).await?;
    done.set(done.get() + tokenizer.length() as f64);
    let p3 = make_progress(done.clone());
    let weights = fetch_bytes(&weights_url, p3.as_ref().unchecked_ref()).await?;
    set_speech(Speech::Loading);
    let msg = Object::new();
    let set = |k: &str, v: &JsValue| {
        let _ = Reflect::set(&msg, &k.into(), v);
    };
    set("type", &"load".into());
    set("config", &config);
    set("tokenizer", &tokenizer);
    set("weights", &weights);
    // 重み(数百 MB)は写さずに Worker へ渡す。
    let transfer = js_sys::Array::of1(&weights.buffer());
    JsFuture::from(lai_call(&msg, &transfer).map_err(js_msg)?)
        .await
        .map_err(js_msg)?;
    Ok(())
}

/// 文字起こしの結果。
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub language: String,
    /// かかった時間(ミリ秒)。
    pub ms: f64,
}

/// 音声(モノラル、`rate` Hz)を文字にする。モデルを読み込んでいなければエラー。
pub async fn transcribe(pcm: &[f32], rate: f32, language: &str) -> Result<Transcript, String> {
    if !matches!(speech(), Speech::Ready(_)) {
        return Err("音声認識のモデルを読み込んでください".into());
    }
    let msg = Object::new();
    let samples = Float32Array::from(pcm);
    let set = |k: &str, v: &JsValue| {
        let _ = Reflect::set(&msg, &k.into(), v);
    };
    set("type", &"transcribe".into());
    set("pcm", &samples);
    set("rate", &JsValue::from_f64(rate as f64));
    set(
        "language",
        &if language.is_empty() {
            JsValue::NULL
        } else {
            language.into()
        },
    );
    let transfer = js_sys::Array::of1(&samples.buffer());
    let v = JsFuture::from(lai_call(&msg, &transfer).map_err(js_msg)?)
        .await
        .map_err(js_msg)?;
    let get = |k: &str| Reflect::get(&v, &k.into()).unwrap_or(JsValue::UNDEFINED);
    Ok(Transcript {
        text: get("text").as_string().unwrap_or_default(),
        language: get("language").as_string().unwrap_or_default(),
        ms: get("ms").as_f64().unwrap_or(0.0),
    })
}

/// マイクの音を受け取り始める。`on_chunk(サンプル, サンプリング周波数)`。
pub async fn start_mic(on_chunk: impl FnMut(Vec<f32>, f32) + 'static) -> Result<(), String> {
    let mut on_chunk = on_chunk;
    let cb = Closure::<dyn FnMut(Float32Array, f64)>::new(move |a: Float32Array, rate: f64| {
        on_chunk(a.to_vec(), rate as f32);
    });
    let p = lai_mic_start(cb.as_ref().unchecked_ref()).map_err(js_msg)?;
    JsFuture::from(p)
        .await
        .map_err(|e| format!("マイクを使えません: {}", js_msg(e)))?;
    // マイクを止めるまで呼ばれ続ける。
    cb.forget();
    Ok(())
}

pub fn stop_mic() {
    lai_mic_stop();
}
