//! ボードをこの端末(IndexedDB)に保存する。
//!
//! 共同編集のボードは yrs の状態(更新の塊)としてルームごとに保存する。
//! サーバーにつながらなくても前回の内容から開け、オフライン中の編集も残る。
//! 再接続すると手元の全体をサーバーへ送るので、CRDT が他の人の変更とマージする。
//!
//! IndexedDB は大きなボード(数十 MB)も入り、localStorage と違って画面を止めない。

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen(inline_js = r#"
const DB = 'colabboard';
const STORE = 'boards';
let dbp = null;
function db() {
  if (!dbp) {
    dbp = new Promise((resolve, reject) => {
      if (!globalThis.indexedDB) { reject(new Error('IndexedDB がありません')); return; }
      const req = indexedDB.open(DB, 1);
      req.onupgradeneeded = () => req.result.createObjectStore(STORE);
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
      req.onblocked = () => reject(new Error('IndexedDB を開けません(別のタブが古い版を使っています)'));
    });
    dbp.catch(() => { dbp = null; });
  }
  return dbp;
}
function tx(mode, f) {
  return db().then((d) => new Promise((resolve, reject) => {
    const t = d.transaction(STORE, mode);
    const req = f(t.objectStore(STORE));
    t.oncomplete = () => resolve(req ? req.result : undefined);
    t.onerror = () => reject(t.error);
    t.onabort = () => reject(t.error || new Error('中断されました'));
  }));
}
export function idb_get(key) {
  return tx('readonly', (s) => s.get(key)).then((v) => (v instanceof Uint8Array ? v : null));
}
export function idb_put(key, bytes) {
  // `bytes` は WASM のメモリを直接指しているので、すぐに複製する。
  const copy = bytes.slice();
  return tx('readwrite', (s) => s.put(copy, key)).then(() => undefined);
}
export function idb_delete(key) {
  return tx('readwrite', (s) => s.delete(key)).then(() => undefined);
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn idb_get(key: &str) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    fn idb_put(key: &str, bytes: &[u8]) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    fn idb_delete(key: &str) -> Result<js_sys::Promise, JsValue>;
}

fn err(e: JsValue) -> String {
    e.as_string()
        .or_else(|| {
            js_sys::Reflect::get(&e, &"message".into())
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{e:?}"))
}

/// ルームの保存キー。
pub fn room_key(room: &str) -> String {
    format!("room:{room}")
}

/// 保存した状態を読む。なければ `None`。
pub async fn load(key: &str) -> Result<Option<Vec<u8>>, String> {
    let v = JsFuture::from(idb_get(key).map_err(err)?)
        .await
        .map_err(err)?;
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    Ok(Some(js_sys::Uint8Array::new(&v).to_vec()))
}

/// 状態を保存する(同じキーは上書き)。
pub async fn save(key: &str, bytes: &[u8]) -> Result<(), String> {
    JsFuture::from(idb_put(key, bytes).map_err(err)?)
        .await
        .map_err(err)?;
    Ok(())
}

#[allow(dead_code)]
pub async fn delete(key: &str) -> Result<(), String> {
    JsFuture::from(idb_delete(key).map_err(err)?)
        .await
        .map_err(err)?;
    Ok(())
}
