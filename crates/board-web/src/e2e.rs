//! エンドツーエンド暗号化ボード(ブラウザ側)。
//!
//! 鍵はこのブラウザで作り、URL のフラグメント(`#k=…`。ブラウザはサーバーに送らない)と
//! この端末の localStorage にだけ置く。ボードの更新は送る前に暗号化し、届いたものは復号してから取り込む。
//! 暗号そのものは `board-e2e` クレート(XChaCha20-Poly1305)。

use board_core::api::{BoardInfo, E2eInfo};
use board_e2e::{BoardKey, E2eError, Kind};
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlElement, HtmlInputElement, Window};

use crate::http::escape;

/// これより多くの暗号文が溜まったら、編集できる人が 1 つにまとめる。
pub const COMPACT_AT: usize = 64;

const STORAGE_PREFIX: &str = "colabboard.e2e.";

/// 暗号論的な乱数(`crypto.getRandomValues`)。
pub fn fill_random(buf: &mut [u8]) {
    let crypto = web_sys::window()
        .and_then(|w| w.crypto().ok())
        .expect("このブラウザでは暗号用の乱数を使えません");
    // getRandomValues は 1 回 65536 バイトまで。
    for chunk in buf.chunks_mut(65536) {
        crypto
            .get_random_values_with_u8_array(chunk)
            .expect("暗号用の乱数を作れません");
    }
}

/// 新しい鍵。
pub fn new_key() -> BoardKey {
    BoardKey::generate(&mut fill_random)
}

/// 1 枚のボードの暗号化・復号。
pub struct Sealer {
    room: String,
    key: BoardKey,
}

impl Sealer {
    pub fn new(room: &str, key: BoardKey) -> Self {
        Self {
            room: room.to_string(),
            key,
        }
    }

    pub fn key(&self) -> &BoardKey {
        &self.key
    }

    pub fn set_key(&mut self, key: BoardKey) {
        self.key = key;
    }

    pub fn seal_update(&self, update: &[u8]) -> Vec<u8> {
        self.key
            .seal(&self.room, Kind::Update, update, &mut fill_random)
    }

    pub fn seal_snapshot(&self, state: &[u8]) -> Vec<u8> {
        self.key
            .seal(&self.room, Kind::Snapshot, state, &mut fill_random)
    }

    /// ログの 1 件を復号する(1 回の編集か、まとめた全体のどちらか)。
    pub fn open(&self, blob: &[u8]) -> Result<Vec<u8>, E2eError> {
        self.key
            .open(&self.room, Kind::Update, blob)
            .or_else(|_| self.key.open(&self.room, Kind::Snapshot, blob))
    }
}

fn storage(window: &Window) -> Option<web_sys::Storage> {
    window.local_storage().ok().flatten()
}

/// この端末に覚えている鍵。
pub fn stored_key(window: &Window, room: &str) -> Option<BoardKey> {
    let s = storage(window)?
        .get_item(&format!("{STORAGE_PREFIX}{room}"))
        .ok()??;
    BoardKey::from_link(&s).ok()
}

/// 鍵をこの端末に覚える。
pub fn store_key(window: &Window, room: &str, key: &BoardKey) {
    if let Some(s) = storage(window) {
        let _ = s.set_item(&format!("{STORAGE_PREFIX}{room}"), &key.to_link());
    }
}

/// URL の `#k=…` から鍵を取り出し、アドレスバーからは消す(画面共有や履歴に残さない)。
pub fn take_fragment_key(window: &Window) -> Option<BoardKey> {
    let loc = window.location();
    let hash = loc.hash().ok()?;
    let key = board_e2e::key_from_fragment(&hash)?;
    if let Ok(history) = window.history() {
        let url = format!(
            "{}{}",
            loc.pathname().unwrap_or_default(),
            loc.search().unwrap_or_default()
        );
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url));
    }
    Some(key)
}

/// 鍵付きのリンク(`…?room=…[&key=…]#k=…`)。
pub fn key_link(window: &Window, room: &str, link_key: Option<&str>, key: &BoardKey) -> String {
    let loc = window.location();
    format!(
        "{}{}{}#k={}",
        loc.origin().unwrap_or_default(),
        loc.pathname().unwrap_or_default(),
        crate::home::board_url(room, link_key),
        key.to_link()
    )
}

/// 手元にある鍵のうち、サーバーに預けた指紋と合うもの。合えばこの端末に覚え直す。
pub fn resolve_key(window: &Window, room: &str, info: &E2eInfo) -> Result<BoardKey, KeyProblem> {
    let from_link = take_fragment_key(window);
    let had_link = from_link.is_some();
    let candidates = from_link.into_iter().chain(stored_key(window, room));
    for k in candidates {
        if k.key_id() == info.key_id {
            store_key(window, room, &k);
            return Ok(k);
        }
    }
    Err(if had_link || stored_key(window, room).is_some() {
        KeyProblem::Wrong
    } else {
        KeyProblem::Missing
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyProblem {
    /// 鍵を持っていない。
    Missing,
    /// 持っている鍵が違う(作り直された・別のボードの鍵)。
    Wrong,
}

/// 鍵がないときの画面。鍵付きリンクか鍵を貼り付けてもらい、合えば開き直す。
pub fn show_key_prompt(document: &Document, info: &BoardInfo, problem: KeyProblem, rotated: bool) {
    let Some(root) = document.get_element_by_id("home") else {
        return;
    };
    if let Some(body) = document.body() {
        let _ = body.class_list().add_1("home-mode");
    }
    let lead = match (problem, rotated) {
        (_, true) => {
            "オーナーがこのボードの鍵を作り直しました。新しい鍵付きリンクをオーナーから受け取ってください。"
        }
        (KeyProblem::Wrong, _) => {
            "手元の鍵ではこのボードを開けません。鍵が作り直されたか、別のボードの鍵かもしれません。"
        }
        (KeyProblem::Missing, _) => {
            "このボードの中身は暗号化されていて、サーバーの管理者にも読めません。開くには、共有された鍵付きリンク(# 以降に鍵が入っています)が必要です。"
        }
    };
    root.set_inner_html(&format!(
        r#"<div class="home-card auth e2e-key">
  <h1><span aria-hidden="true">🔒</span> {title}</h1>
  <p class="muted">{lead}</p>
  <form data-form="e2e-key" class="stack">
    <label>鍵付きリンク、または鍵<input name="key" autocomplete="off" spellcheck="false" placeholder="https://…#k=… または 43 文字の鍵" autofocus /></label>
    <p class="form-error" data-slot="error" role="alert" hidden></p>
    <p class="row"><button type="submit" class="primary">このボードを開く</button><a class="button" href="./">ボード一覧へ</a></p>
  </form>
  <p class="muted small">鍵の指紋: <code translate="no">{fp}</code>(共有した人の画面の表示と同じなら、正しい鍵です)</p>
</div>"#,
        title = escape(&info.title),
        fp = escape(&short_fingerprint(&info.key_id_or_empty())),
    ));
    if let Ok(root) = root.clone().dyn_into::<HtmlElement>() {
        root.set_hidden(false);
    }
    let room = info.id.clone();
    let want = info
        .e2e
        .as_ref()
        .map(|e| e.key_id.clone())
        .unwrap_or_default();
    let doc = document.clone();
    let cb = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::Event)>::new(
        move |e: web_sys::Event| {
            e.prevent_default();
            let input = doc
                .query_selector("[data-form=e2e-key] [name=key]")
                .ok()
                .flatten()
                .and_then(|el| el.dyn_into::<HtmlInputElement>().ok());
            let Some(input) = input else { return };
            let raw = input.value();
            let key = parse_key_input(&raw);
            let err = doc
                .query_selector("[data-form=e2e-key] [data-slot=error]")
                .ok()
                .flatten();
            match key {
                Some(k) if k.key_id() == want => {
                    let Some(window) = web_sys::window() else {
                        return;
                    };
                    store_key(&window, &room, &k);
                    let _ = window.location().reload();
                }
                other => {
                    if let Some(err) = err {
                        err.set_text_content(Some(if other.is_some() {
                            "鍵が違います。このボードの鍵付きリンクか確かめてください。"
                        } else {
                            "鍵の形式が違います。リンクをそのまま貼り付けてください。"
                        }));
                        let _ = err.remove_attribute("hidden");
                    }
                }
            }
        },
    );
    if let Ok(Some(form)) = document.query_selector("[data-form=e2e-key]") {
        let _ = form.add_event_listener_with_callback("submit", cb.as_ref().unchecked_ref());
    }
    cb.forget();
}

pub use board_e2e::parse_key_input;

/// 指紋(16 進)を人が見比べやすい 4 文字 × 4 組にする。
pub fn short_fingerprint(key_id: &str) -> String {
    board_e2e::fingerprint_of(key_id)
}

trait KeyIdOrEmpty {
    fn key_id_or_empty(&self) -> String;
}

impl KeyIdOrEmpty for BoardInfo {
    fn key_id_or_empty(&self) -> String {
        self.e2e
            .as_ref()
            .map(|e| e.key_id.clone())
            .unwrap_or_default()
    }
}

/// 通報の理由の選択肢(HTML)。
pub fn reason_options() -> String {
    board_core::api::REPORT_REASONS
        .iter()
        .map(|(k, v)| format!(r#"<option value="{k}">{}</option>"#, escape(v)))
        .collect()
}

/// base64(鍵の作り直しで暗号文を送るとき)。
pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
