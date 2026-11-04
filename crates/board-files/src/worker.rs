//! Worker の入口。本体(`board-web` の `file_worker`)からの仕事を 1 つずつ処理して返す。
//!
//! 起動すると `{ ready: true }` を送る。仕事は `{ id, op, ... }` で届き、`{ id, ... }` か
//! `{ id, error }` を返す(`pdf-close` だけは返さない)。
//!
//! - `pdf-info` `{ doc, bytes? }` → `{ sizes: Float32Array [w, h, ...], total }`
//! - `pdf-page` `{ doc, bytes?, page, pixels }` → `{ data: Uint8Array(JPEG), w, h }`
//! - `pdf-close` `{ doc }` → 開いた PDF を捨てる
//! - `image` `{ bytes }` → `{ data, w, h }`(縮めた画像)
//! - `table` `{ bytes, csv, keep }` → `{ sheets: [{ name, rows: [[...]], total }] }`
//!
//! PDF は `doc` ごとに開いたまま持っておき、同じ PDF の次のページはバイト列なしで頼める
//! (本体は Worker ごとに、その PDF を初めて頼むときだけバイト列を付ける)。

use std::cell::RefCell;
use std::collections::HashMap;

use js_sys::{Array, Float32Array, Object, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

thread_local! {
    static DOCS: RefCell<HashMap<u32, crate::pdf::Pdf>> = RefCell::new(HashMap::new());
}

fn get(obj: &JsValue, key: &str) -> JsValue {
    Reflect::get(obj, &key.into()).unwrap_or(JsValue::UNDEFINED)
}

fn set(obj: &Object, key: &str, value: impl Into<JsValue>) {
    let _ = Reflect::set(obj, &key.into(), &value.into());
}

fn bytes_of(msg: &JsValue) -> Option<Vec<u8>> {
    let b = get(msg, "bytes");
    b.is_object().then(|| Uint8Array::new(&b).to_vec())
}

fn num(msg: &JsValue, key: &str) -> f64 {
    get(msg, key).as_f64().unwrap_or(0.0)
}

/// `doc` の PDF を(まだなら届いたバイト列で開いて)使う。
fn with_doc<R>(
    msg: &JsValue,
    f: impl FnOnce(&crate::pdf::Pdf) -> Result<R, String>,
) -> Result<R, String> {
    let doc = num(msg, "doc") as u32;
    DOCS.with(|d| {
        let mut d = d.borrow_mut();
        let pdf = match d.entry(doc) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let bytes = bytes_of(msg).ok_or("PDF がまだ届いていません")?;
                e.insert(crate::pdf::open(bytes)?)
            }
        };
        f(pdf)
    })
}

/// 画像を返す(バイト列は写さずに渡す)。
fn image_reply(reply: &Object, transfer: &Array, (data, w, h): (Vec<u8>, u32, u32)) {
    let data = Uint8Array::from(data.as_slice());
    transfer.push(&data.buffer());
    set(reply, "data", data);
    set(reply, "w", w);
    set(reply, "h", h);
}

/// 仕事を 1 つ処理して返事を作る。返事のいらない仕事は `None`。
fn handle(msg: &JsValue, reply: &Object, transfer: &Array) -> Option<Result<(), String>> {
    let op = get(msg, "op").as_string().unwrap_or_default();
    let r = match op.as_str() {
        "pdf-info" => with_doc(msg, crate::pdf::page_sizes).map(|(sizes, total)| {
            let flat: Vec<f32> = sizes.iter().flat_map(|&(w, h)| [w, h]).collect();
            set(reply, "sizes", Float32Array::from(flat.as_slice()));
            set(reply, "total", total as u32);
        }),
        "pdf-page" => {
            let page = num(msg, "page") as usize;
            let pixels = num(msg, "pixels") as u32;
            with_doc(msg, |pdf| crate::pdf::render_page(pdf, page, pixels))
                .map(|img| image_reply(reply, transfer, img))
        }
        "pdf-close" => {
            let doc = num(msg, "doc") as u32;
            DOCS.with(|d| d.borrow_mut().remove(&doc));
            return None;
        }
        "image" => bytes_of(msg)
            .ok_or_else(|| "画像がありません".to_string())
            .and_then(|b| crate::picture::prepare_image(&b))
            .map(|img| image_reply(reply, transfer, img)),
        "table" => {
            let bytes = bytes_of(msg).unwrap_or_default();
            let keep = num(msg, "keep") as usize;
            let sheets = if get(msg, "csv").is_truthy() {
                Ok(vec![crate::table::read_csv(&bytes, keep)])
            } else {
                crate::table::read_spreadsheet(bytes, keep)
            };
            sheets.map(|sheets| {
                let list = Array::new();
                for (sheet, total) in sheets {
                    let s = Object::new();
                    set(&s, "name", sheet.name);
                    let rows = Array::new();
                    for r in sheet.rows {
                        rows.push(&r.into_iter().map(JsValue::from).collect::<Array>());
                    }
                    set(&s, "rows", rows);
                    set(&s, "total", total as u32);
                    list.push(&s);
                }
                set(reply, "sheets", list);
            })
        }
        other => Err(format!("知らない仕事です: {other}")),
    };
    Some(r)
}

#[wasm_bindgen(start)]
pub fn start() {
    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let reply_to = scope.clone();
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        let msg = e.data();
        let reply = Object::new();
        set(&reply, "id", get(&msg, "id"));
        let transfer = Array::new();
        match handle(&msg, &reply, &transfer) {
            None => return,
            Some(Ok(())) => {}
            Some(Err(err)) => set(&reply, "error", err),
        }
        let _ = reply_to.post_message_with_transfer(&reply, &transfer);
    });
    scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();
    // 受け取る準備ができたことを知らせる(wasm の初期化は非同期なので、それまでに来た
    // メッセージは取りこぼす)。本体はこれを待ってから仕事を送る。
    let ready = Object::new();
    set(&ready, "ready", true);
    let _ = scope.post_message(&ready);
}
