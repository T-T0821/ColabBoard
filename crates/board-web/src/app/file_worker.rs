//! 重いファイルの処理を、別の wasm(`crates/board-files`)の Web Worker に並行して任せる。
//!
//! - Worker は仕事が来たときに初めて起こし、CPU のコア数に合わせて最大 4 つまで並べる。
//!   しばらく仕事がなければ止めてメモリを返す。
//! - 各 Worker には仕事を 3 つまで先渡しする。画面のスレッドが描画で忙しく返事をすぐ
//!   受け取れなくても、Worker は止まらずに次のページを描ける。
//! - 仕事は順番待ちの列に並べ、空いた Worker から順に渡す。画像や表は PDF のページより先に渡す
//!   (大きな PDF の取り込み中でも、あとから貼った写真がすぐ出るように)。
//! - PDF は Worker ごとに 1 回だけバイト列を渡して開いたままにし、以後はページ番号だけ頼む。
//!   取り込みをやめたら([`pdf_close`])、まだ渡していないページの仕事は [`CANCELLED`] で終わる。
//! - Worker が落ちたら(壊れたファイルで止まったなど)、その仕事だけ失敗にして Worker を作り直す。

use board_core::tabular::Sheet;
use js_sys::{Object, Reflect, Uint8Array};
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;

/// Worker の読み込み口(trunk が作る)。
const LOADER: &str = "board-files_loader.js";

/// 取り込みをやめたために渡さなかった仕事のエラー。
pub const CANCELLED: &str = "cancelled";

#[wasm_bindgen(inline_js = r#"
const S = { workers: [], queue: [], seq: 0, docs: new Map(), nextDoc: 1, idle: 0, failures: 0 };
const IDLE_MS = 20000;
// 1 つの Worker に先渡ししておく仕事の数。画面のスレッドが描画などで忙しくても、
// Worker が返事を待たずに次の仕事に取りかかれる。
const DEPTH = 3;

function poolSize() {
  const n = (self.navigator && navigator.hardwareConcurrency) || 2;
  return Math.max(1, Math.min(4, n - 1));
}

function spawn(url) {
  const rec = { w: null, ready: false, jobs: new Map(), docs: new Set() };
  try { rec.w = new Worker(url); } catch (e) { failAll(String(e)); return false; }
  rec.w.onmessage = (e) => {
    const d = e.data || {};
    if (d.ready) { rec.ready = true; S.failures = 0; pump(url); return; }
    const job = rec.jobs.get(d.id);
    if (!job) return;
    rec.jobs.delete(d.id);
    if (d.error) job.reject(d.error); else job.resolve(d);
    pump(url);
  };
  rec.w.onerror = (e) => {
    e.preventDefault();
    const started = rec.ready;
    drop(rec);
    for (const job of rec.jobs.values()) job.reject('ファイルの処理中に止まりました');
    rec.jobs.clear();
    if (!started && ++S.failures >= 2) { failAll('ファイルの処理部品を起動できません'); return; }
    pump(url);
  };
  S.workers.push(rec);
  return true;
}

function drop(rec) {
  try { rec.w.terminate(); } catch (_) {}
  S.workers = S.workers.filter((r) => r !== rec);
}

function failAll(msg) {
  const q = S.queue; S.queue = [];
  for (const job of q) job.reject(msg);
  S.failures = 0;
}

function inFlight() {
  return S.workers.reduce((n, r) => n + r.jobs.size, 0);
}

function pump(url) {
  clearTimeout(S.idle);
  const want = Math.min(poolSize(), Math.ceil((inFlight() + S.queue.length) / DEPTH));
  while (S.workers.length < want) { if (!spawn(url)) return; }
  while (S.queue.length) {
    const open = S.workers.filter((r) => r.ready && r.jobs.size < DEPTH);
    if (!open.length) break;
    const job = S.queue[0];
    // 仕事の少ない Worker に渡す(同じ PDF をもう開いている Worker を優先する)。
    const load = (r) => r.jobs.size * 2 + (job.doc && !r.docs.has(job.doc) ? 1 : 0);
    const rec = open.reduce((a, b) => (load(b) < load(a) ? b : a));
    S.queue.shift();
    send(rec, job);
  }
  if (!S.queue.length && !inFlight()) {
    S.idle = setTimeout(() => { for (const r of S.workers.slice()) drop(r); }, IDLE_MS);
  }
}

function send(rec, job) {
  const msg = Object.assign({ id: job.id }, job.msg);
  const transfer = [];
  if (job.doc) {
    msg.doc = job.doc;
    if (!rec.docs.has(job.doc)) {
      const bytes = S.docs.get(job.doc);
      if (!bytes) { job.reject('cancelled'); return; }
      msg.bytes = bytes; // 写して渡す(ほかの Worker にも渡すので手放さない)
      rec.docs.add(job.doc);
    }
  } else if (job.bytes) {
    msg.bytes = job.bytes;
    transfer.push(job.bytes.buffer);
  }
  rec.jobs.set(job.id, job);
  rec.w.postMessage(msg, transfer);
}

export function files_run(url, msg, bytes, doc, urgent) {
  return new Promise((resolve, reject) => {
    const job = { id: ++S.seq, msg, bytes, doc, resolve, reject };
    if (urgent) S.queue.unshift(job); else S.queue.push(job);
    pump(url);
  });
}

export function files_pdf_open(bytes) {
  const doc = S.nextDoc++;
  S.docs.set(doc, bytes);
  return doc;
}

export function files_pdf_close(doc) {
  S.docs.delete(doc);
  const keep = [];
  for (const job of S.queue) { if (job.doc === doc) job.reject('cancelled'); else keep.push(job); }
  S.queue = keep;
  for (const r of S.workers) {
    if (r.docs.delete(doc)) r.w.postMessage({ op: 'pdf-close', doc });
  }
}
"#)]
extern "C" {
    fn files_run(
        url: &str,
        msg: &JsValue,
        bytes: &JsValue,
        doc: u32,
        urgent: bool,
    ) -> js_sys::Promise;
    fn files_pdf_open(bytes: &Uint8Array) -> u32;
    fn files_pdf_close(doc: u32);
}

/// Worker が返した画像(縮めたもの、または PDF のページ)。
pub struct Picture {
    pub data: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

fn get(obj: &JsValue, key: &str) -> JsValue {
    Reflect::get(obj, &key.into()).unwrap_or(JsValue::UNDEFINED)
}

fn message(op: &str, fields: &[(&str, JsValue)]) -> JsValue {
    let msg = Object::new();
    let _ = Reflect::set(&msg, &"op".into(), &op.into());
    for (k, v) in fields {
        let _ = Reflect::set(&msg, &(*k).into(), v);
    }
    msg.into()
}

async fn run(
    msg: JsValue,
    bytes: Option<&[u8]>,
    doc: u32,
    urgent: bool,
) -> Result<JsValue, String> {
    // wasm のメモリを指したままにしないよう、JS 側に写してから渡す。
    let bytes = bytes.map_or(JsValue::UNDEFINED, |b| Uint8Array::from(b).into());
    wasm_bindgen_futures::JsFuture::from(files_run(LOADER, &msg, &bytes, doc, urgent))
        .await
        .map_err(|e| {
            e.as_string()
                .unwrap_or_else(|| "ファイルを処理できません".into())
        })
}

fn picture(reply: &JsValue) -> Picture {
    Picture {
        data: Uint8Array::new(&get(reply, "data")).to_vec(),
        w: get(reply, "w").as_f64().unwrap_or(0.0) as u32,
        h: get(reply, "h").as_f64().unwrap_or(0.0) as u32,
    }
}

/// 画像をデコードし、大きすぎれば縮める。
pub async fn image(bytes: &[u8]) -> Result<Picture, String> {
    run(message("image", &[]), Some(bytes), 0, true)
        .await
        .map(|r| picture(&r))
}

/// CSV(`csv`)か表計算のファイルを読む。シートごとに、先頭 `keep` 行と空でない行の総数。
pub async fn table(bytes: &[u8], csv: bool, keep: usize) -> Result<Vec<(Sheet, usize)>, String> {
    let msg = message(
        "table",
        &[("csv", csv.into()), ("keep", (keep as u32).into())],
    );
    let reply = run(msg, Some(bytes), 0, true).await?;
    let sheets = js_sys::Array::from(&get(&reply, "sheets"))
        .iter()
        .map(|s| {
            let rows = js_sys::Array::from(&get(&s, "rows"))
                .iter()
                .map(|r| {
                    js_sys::Array::from(&r)
                        .iter()
                        .map(|c| c.as_string().unwrap_or_default())
                        .collect()
                })
                .collect();
            let sheet = Sheet {
                name: get(&s, "name").as_string().unwrap_or_default(),
                rows,
            };
            (sheet, get(&s, "total").as_f64().unwrap_or(0.0) as usize)
        })
        .collect();
    Ok(sheets)
}

/// PDF を Worker に渡せるように預ける。返す番号で [`pdf_info`]・[`pdf_page`] を頼み、
/// 終わったら [`pdf_close`] する。
pub fn pdf_open(bytes: &[u8]) -> u32 {
    files_pdf_open(&Uint8Array::from(bytes))
}

/// 取り込むページの大きさ(ポイント)と、PDF の全ページ数。
pub async fn pdf_info(doc: u32) -> Result<(Vec<(f32, f32)>, usize), String> {
    let reply = run(message("pdf-info", &[]), None, doc, true).await?;
    let flat = js_sys::Float32Array::new(&get(&reply, "sizes")).to_vec();
    let sizes = flat
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[w, h]| (w, h))
        .collect();
    Ok((sizes, get(&reply, "total").as_f64().unwrap_or(0.0) as usize))
}

/// `page` 番目(0 から)のページを、長辺 `pixels` ピクセルの JPEG にする。
pub async fn pdf_page(doc: u32, page: usize, pixels: u32) -> Result<Picture, String> {
    let msg = message(
        "pdf-page",
        &[("page", (page as u32).into()), ("pixels", pixels.into())],
    );
    run(msg, None, doc, false).await.map(|r| picture(&r))
}

/// PDF の取り込みを終える(まだ渡していないページの仕事はやめ、Worker から PDF を捨てる)。
pub fn pdf_close(doc: u32) {
    files_pdf_close(doc);
}
