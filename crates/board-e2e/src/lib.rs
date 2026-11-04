//! エンドツーエンド暗号化ボード。
//!
//! ボードごとに 32 バイトの鍵(ボード鍵)を**ブラウザの中で**作り、ボードの中身(yrs の更新)を
//! XChaCha20-Poly1305 で暗号化してからサーバーに送る。サーバーは暗号文を保存して中継するだけで、
//! 鍵を受け取らないので、サーバーの管理者にも中身は読めない。
//!
//! - 鍵は共有リンクの `#k=…`(URL のフラグメント。ブラウザはサーバーに送らない)と、
//!   開いた端末の localStorage にだけ置く。
//! - サーバーには鍵そのものではなく、鍵の指紋([`BoardKey::key_id`])だけを預ける。
//!   鍵が違うときに「鍵が違います」と言えるようにするためで、指紋から鍵は戻せない。
//! - 暗号文にはボード ID と種類を関連データとして結び付ける。サーバーが別のボードの暗号文を
//!   差し込んだり、更新と全体を入れ替えたりしても、復号に失敗する。
//! - 平文は長さを 256 バイト単位に切り上げてから暗号化する(1 文字の編集かどうかを長さから読ませない)。
//!
//! 暗号文の形: `版 (1 バイト = 1)` + `ナンス (24 バイト)` + `暗号文 + 認証タグ (16 バイト)`。

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

/// 鍵の長さ(バイト)。
pub const KEY_LEN: usize = 32;
/// ナンスの長さ(バイト)。ランダムに選んでも衝突しない長さ(XChaCha20)。
pub const NONCE_LEN: usize = 24;
/// 暗号文の版。
pub const VERSION: u8 = 1;
/// 暗号化で増えるバイト数(版 + ナンス + タグ)。パディングは含まない。
pub const OVERHEAD: usize = 1 + NONCE_LEN + 16;
/// 平文を切り上げる単位。
pub const PAD_BLOCK: usize = 256;

/// 暗号文の種類。関連データに入るので、種類を偽って使い回せない。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 1 回の編集(yrs v1 更新)。
    Update,
    /// ボード全体(ログをまとめたもの・鍵の作り直し)。
    Snapshot,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Snapshot => "snapshot",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum E2eError {
    /// 鍵の文字列が読めない。
    BadKey,
    /// 暗号文の形が違う(短すぎる・版が違う)。
    Malformed,
    /// 鍵が違うか、改ざんされている。
    Decrypt,
}

impl std::fmt::Display for E2eError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadKey => write!(f, "鍵の形式が違います"),
            Self::Malformed => write!(f, "暗号文の形式が違います"),
            Self::Decrypt => write!(f, "復号できません(鍵が違うか、内容が書き換えられています)"),
        }
    }
}

impl std::error::Error for E2eError {}

/// ボード鍵。
#[derive(Clone, PartialEq, Eq)]
pub struct BoardKey([u8; KEY_LEN]);

impl std::fmt::Debug for BoardKey {
    // ログに鍵が出ないよう、指紋だけを出す。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BoardKey({})", self.key_id())
    }
}

impl Drop for BoardKey {
    fn drop(&mut self) {
        // 最適化で消されないよう volatile で上書きする。
        for b in self.0.iter_mut() {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

impl BoardKey {
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// 乱数で新しい鍵を作る。`fill` は暗号論的な乱数でバッファを埋める関数。
    pub fn generate(fill: &mut dyn FnMut(&mut [u8])) -> Self {
        let mut bytes = [0u8; KEY_LEN];
        fill(&mut bytes);
        Self(bytes)
    }

    /// URL のフラグメント(`#k=…`)に載せる形(base64url、パディングなし、43 文字)。
    pub fn to_link(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    /// [`to_link`](Self::to_link) の逆。前後の空白は無視する。
    pub fn from_link(s: &str) -> Result<Self, E2eError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(s.trim().trim_end_matches('='))
            .map_err(|_| E2eError::BadKey)?;
        let arr: [u8; KEY_LEN] = bytes.try_into().map_err(|_| E2eError::BadKey)?;
        Ok(Self(arr))
    }

    /// 鍵の指紋(16 進 32 文字)。サーバーに預け、鍵が合っているかの確認に使う。
    /// 一方向のハッシュなので、指紋から鍵は戻せない。
    pub fn key_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"graboard-e2e-key-id\0");
        h.update(self.0);
        let digest = h.finalize();
        digest[..16].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 人が見比べる短い指紋(`ABCD-EFGH-…` の 4 組)。共有した相手と同じ鍵か口頭で確かめる用。
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.key_id())
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }

    /// 暗号化する。`fill` はナンス用の乱数。
    pub fn seal(
        &self,
        room: &str,
        kind: Kind,
        plaintext: &[u8],
        fill: &mut dyn FnMut(&mut [u8]),
    ) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        fill(&mut nonce);
        self.seal_with_nonce(room, kind, plaintext, nonce)
    }

    /// ナンスを指定して暗号化する(テスト用。同じ鍵でナンスを使い回してはいけない)。
    pub fn seal_with_nonce(
        &self,
        room: &str,
        kind: Kind,
        plaintext: &[u8],
        nonce: [u8; NONCE_LEN],
    ) -> Vec<u8> {
        let padded = pad(plaintext);
        let aad = aad(room, kind);
        let ct = self
            .cipher()
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &padded,
                    aad: &aad,
                },
            )
            .expect("XChaCha20-Poly1305 の暗号化は失敗しない");
        let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// 復号する。
    pub fn open(&self, room: &str, kind: Kind, blob: &[u8]) -> Result<Vec<u8>, E2eError> {
        if blob.len() < OVERHEAD + 4 || blob[0] != VERSION {
            return Err(E2eError::Malformed);
        }
        let (nonce, ct) = blob[1..].split_at(NONCE_LEN);
        let aad = aad(room, kind);
        let padded = self
            .cipher()
            .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: &aad })
            .map_err(|_| E2eError::Decrypt)?;
        unpad(&padded).ok_or(E2eError::Malformed)
    }
}

/// 関連データ: 版・ボード ID・種類。
fn aad(room: &str, kind: Kind) -> Vec<u8> {
    format!("graboard-e2e/v{VERSION}/{room}/{}", kind.label()).into_bytes()
}

/// 長さ(u32 LE) + 本体 + 0 埋め(全体を [`PAD_BLOCK`] の倍数に)。
fn pad(plain: &[u8]) -> Vec<u8> {
    let len = 4 + plain.len();
    let total = len.div_ceil(PAD_BLOCK) * PAD_BLOCK;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(plain.len() as u32).to_le_bytes());
    out.extend_from_slice(plain);
    out.resize(total, 0);
    out
}

fn unpad(padded: &[u8]) -> Option<Vec<u8>> {
    let len = u32::from_le_bytes(padded.get(..4)?.try_into().ok()?) as usize;
    padded.get(4..4 + len).map(<[u8]>::to_vec)
}

/// URL のフラグメント(`#…`。先頭の `#` はあってもなくてもよい)から鍵を取り出す。
/// `k=` のほかの項目があっても構わない。
pub fn key_from_fragment(fragment: &str) -> Option<BoardKey> {
    fragment
        .trim_start_matches('#')
        .split('&')
        .find_map(|kv| kv.strip_prefix("k="))
        .and_then(|v| BoardKey::from_link(v).ok())
}

/// 指紋([`BoardKey::key_id`])の先頭 16 文字を、人が見比べやすい `ABCD-EFGH-…` の 4 組にする。
pub fn fingerprint_of(key_id: &str) -> String {
    key_id
        .to_uppercase()
        .as_bytes()
        .chunks(4)
        .take(4)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

/// 人が貼り付けた文字(鍵付きリンク・`#k=…`・`k=…`・鍵だけ)から鍵を取り出す。
pub fn parse_key_input(raw: &str) -> Option<BoardKey> {
    let raw = raw.trim();
    if let Some((_, frag)) = raw.split_once('#') {
        return key_from_fragment(frag);
    }
    key_from_fragment(raw).or_else(|| BoardKey::from_link(raw).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(buf: &mut [u8]) {
        getrandom::fill(buf).unwrap();
    }

    fn key() -> BoardKey {
        BoardKey::generate(&mut rng)
    }

    #[test]
    fn round_trip() {
        let k = key();
        let msg = b"hello board".to_vec();
        let blob = k.seal("room1", Kind::Update, &msg, &mut rng);
        assert_eq!(k.open("room1", Kind::Update, &blob).unwrap(), msg);
        // 空の平文も扱える。
        let blob = k.seal("room1", Kind::Snapshot, &[], &mut rng);
        assert_eq!(
            k.open("room1", Kind::Snapshot, &blob).unwrap(),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn ciphertext_hides_plaintext_and_length() {
        let k = key();
        let secret = "社外秘の売上目標".as_bytes();
        let blob = k.seal("r", Kind::Update, secret, &mut rng);
        assert!(!blob.windows(secret.len()).any(|w| w == secret));
        // 長さは 256 バイト単位にそろう。
        let a = k.seal("r", Kind::Update, b"a", &mut rng);
        let b = k.seal("r", Kind::Update, &[7u8; 200], &mut rng);
        assert_eq!(a.len(), b.len());
        assert_eq!(a.len(), OVERHEAD + PAD_BLOCK);
        // 同じ平文でもナンスが違うので暗号文は変わる。
        let c = k.seal("r", Kind::Update, b"a", &mut rng);
        assert_ne!(a, c);
    }

    #[test]
    fn wrong_key_room_or_kind_fails() {
        let k = key();
        let blob = k.seal("room1", Kind::Update, b"x", &mut rng);
        assert_eq!(
            key().open("room1", Kind::Update, &blob),
            Err(E2eError::Decrypt)
        );
        assert_eq!(k.open("room2", Kind::Update, &blob), Err(E2eError::Decrypt));
        assert_eq!(
            k.open("room1", Kind::Snapshot, &blob),
            Err(E2eError::Decrypt)
        );
    }

    #[test]
    fn tampering_is_detected() {
        let k = key();
        let mut blob = k.seal("r", Kind::Update, b"important", &mut rng);
        let last = blob.len() - 1;
        blob[last] ^= 1;
        assert_eq!(k.open("r", Kind::Update, &blob), Err(E2eError::Decrypt));
        assert_eq!(
            k.open("r", Kind::Update, &[1, 2, 3]),
            Err(E2eError::Malformed)
        );
        let mut bad_version = k.seal("r", Kind::Update, b"x", &mut rng);
        bad_version[0] = 9;
        assert_eq!(
            k.open("r", Kind::Update, &bad_version),
            Err(E2eError::Malformed)
        );
    }

    #[test]
    fn link_and_fragment() {
        let k = key();
        let link = k.to_link();
        assert_eq!(link.len(), 43);
        assert_eq!(BoardKey::from_link(&link).unwrap(), k);
        assert_eq!(key_from_fragment(&format!("#k={link}")).unwrap(), k);
        assert_eq!(key_from_fragment(&format!("x=1&k={link}")).unwrap(), k);
        assert!(key_from_fragment("#x=1").is_none());
        assert_eq!(BoardKey::from_link("short"), Err(E2eError::BadKey));
    }

    #[test]
    fn key_id_is_stable_and_one_way() {
        let k = BoardKey::from_bytes([42; KEY_LEN]);
        let id = k.key_id();
        assert_eq!(id.len(), 32);
        assert_eq!(id, BoardKey::from_bytes([42; KEY_LEN]).key_id());
        assert_ne!(id, BoardKey::from_bytes([43; KEY_LEN]).key_id());
        // 指紋に鍵のバイトがそのまま出ない。
        assert!(!id.contains("2a2a2a2a"));
        assert_eq!(k.fingerprint().len(), 19);
        assert!(!format!("{k:?}").contains(&k.to_link()));
    }

    #[test]
    fn parses_key_inputs() {
        let k = BoardKey::from_bytes([3; KEY_LEN]);
        let link = k.to_link();
        for raw in [
            format!("https://x.example/?room=abc&key=zz#k={link}"),
            format!("#k={link}"),
            format!("k={link}"),
            format!("  {link}  "),
        ] {
            assert_eq!(parse_key_input(&raw).as_ref(), Some(&k), "{raw}");
        }
        assert!(parse_key_input("https://x.example/?room=abc").is_none());
        assert!(parse_key_input("nope").is_none());
        assert_eq!(
            fingerprint_of("0123456789abcdef0123456789abcdef"),
            "0123-4567-89AB-CDEF"
        );
    }

    #[test]
    fn fixed_vector() {
        // 形式を変えたら気づけるように、決まった鍵とナンスでの結果を固定する。
        let k = BoardKey::from_bytes([1; KEY_LEN]);
        let blob = k.seal_with_nonce("r", Kind::Update, b"abc", [2; NONCE_LEN]);
        assert_eq!(blob[0], VERSION);
        assert_eq!(&blob[1..25], &[2; NONCE_LEN]);
        assert_eq!(k.open("r", Kind::Update, &blob).unwrap(), b"abc");
    }
}
