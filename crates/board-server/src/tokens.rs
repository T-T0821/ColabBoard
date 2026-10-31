//! 外部から API を使うためのトークン。
//!
//! トークンは `gbt_<ID>_<秘密>` の形。サーバーには秘密の BLAKE2b ハッシュだけを保存し、
//! 平文は作ったときに一度だけ返す。`Authorization: Bearer <token>` で送ると、
//! 持ち主のユーザーとして、ただし [`TokenScope`] の範囲の権限で扱う。

use board_core::access::{NewToken, TokenInfo, TokenScope};
use serde::{Deserialize, Serialize};

use crate::auth::{AuthError, now_ms, random_hex, random_id};
use crate::store::{API_TOKENS, Store, StoreError, USER_TOKENS};

pub const TOKEN_PREFIX: &str = "gbt_";
/// 1 人が持てるトークンの数。
pub const MAX_TOKENS_PER_USER: usize = 50;
pub const MAX_TOKEN_NAME_CHARS: usize = 60;
/// トークンを限定できるボードの数。
pub const MAX_TOKEN_BOARDS: usize = 100;
/// 「最後に使った時刻」を書き直す最短間隔(毎回書くと遅くなるので)。
const LAST_USED_RESOLUTION_MS: u64 = 60_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenRecord {
    pub id: String,
    pub login: String,
    pub name: String,
    pub scope: TokenScope,
    #[serde(default)]
    pub boards: Option<Vec<String>>,
    pub created: u64,
    #[serde(default)]
    pub last_used: Option<u64>,
    #[serde(default)]
    pub expires: Option<u64>,
    /// 秘密の部分のハッシュ(16 進)。
    pub hash: String,
}

impl TokenRecord {
    pub fn info(&self) -> TokenInfo {
        TokenInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            scope: self.scope,
            boards: self.boards.clone(),
            created: self.created,
            last_used: self.last_used,
            expires: self.expires,
        }
    }

    /// このボードに使えるか。
    pub fn allows_board(&self, id: &str) -> bool {
        self.boards
            .as_ref()
            .is_none_or(|b| b.iter().any(|x| x == id))
    }
}

fn hash_secret(secret: &str) -> String {
    use blake2::{Blake2b512, Digest};
    let mut h = Blake2b512::new();
    h.update(b"graboard-api-token\0");
    h.update(secret.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// トークンを作る。返り値は(平文のトークン, 記録)。
pub fn create(
    store: &Store,
    login: &str,
    req: &NewToken,
) -> Result<(String, TokenRecord), AuthError> {
    let name: String = req
        .name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TOKEN_NAME_CHARS)
        .collect();
    if name.is_empty() {
        return Err(AuthError::Invalid("トークンの名前を入れてください".into()));
    }
    if list(store, login)?.len() >= MAX_TOKENS_PER_USER {
        return Err(AuthError::Invalid(format!(
            "トークンは {MAX_TOKENS_PER_USER} 個までです。使っていないものを消してください"
        )));
    }
    let boards = match &req.boards {
        None => None,
        Some(b) if b.is_empty() || b.len() > MAX_TOKEN_BOARDS => {
            return Err(AuthError::Invalid(format!(
                "ボードは 1〜{MAX_TOKEN_BOARDS} 個で指定してください"
            )));
        }
        Some(b) => {
            if let Some(bad) = b.iter().find(|id| !crate::valid_room_name(id)) {
                return Err(AuthError::Invalid(format!(
                    "ボード ID「{bad}」が正しくありません"
                )));
            }
            let mut b = b.clone();
            b.sort();
            b.dedup();
            Some(b)
        }
    };
    let now = now_ms();
    let expires = req
        .expires_in_days
        .filter(|d| *d > 0)
        .map(|d| now + d as u64 * 24 * 60 * 60 * 1000);
    loop {
        let id = random_id(12);
        let secret = random_hex(24);
        let rec = TokenRecord {
            id: id.clone(),
            login: login.to_string(),
            name: name.clone(),
            scope: req.scope,
            boards: boards.clone(),
            created: now,
            last_used: None,
            expires,
            hash: hash_secret(&secret),
        };
        if store.insert_new_json(API_TOKENS, &id, &rec)? {
            store.put_pair(USER_TOKENS, login, &id, &())?;
            return Ok((format!("{TOKEN_PREFIX}{id}_{secret}"), rec));
        }
    }
}

/// 自分のトークン(古い順)。
pub fn list(store: &Store, login: &str) -> Result<Vec<TokenRecord>, StoreError> {
    let mut out = Vec::new();
    for (id, ()) in store.scan_pair::<()>(USER_TOKENS, login)? {
        if let Some(rec) = store.get_json::<TokenRecord>(API_TOKENS, &id)? {
            out.push(rec);
        }
    }
    out.sort_by_key(|r| r.created);
    Ok(out)
}

/// トークンを無効にする。自分のものでなければ false。
pub fn revoke(store: &Store, login: &str, id: &str) -> Result<bool, StoreError> {
    match store.get_json::<TokenRecord>(API_TOKENS, id)? {
        Some(rec) if rec.login == login => {
            store.remove(API_TOKENS, id)?;
            store.remove_pair(USER_TOKENS, login, id)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// 平文のトークンを確かめる。正しくなければ `None`。
pub fn verify(store: &Store, token: &str) -> Result<Option<TokenRecord>, StoreError> {
    let Some(rest) = token.strip_prefix(TOKEN_PREFIX) else {
        return Ok(None);
    };
    let Some((id, secret)) = rest.split_once('_') else {
        return Ok(None);
    };
    if !crate::valid_room_name(id) || secret.len() != 48 {
        return Ok(None);
    }
    let Some(mut rec) = store.get_json::<TokenRecord>(API_TOKENS, id)? else {
        return Ok(None);
    };
    if !constant_time_eq(rec.hash.as_bytes(), hash_secret(secret).as_bytes()) {
        return Ok(None);
    }
    let now = now_ms();
    if rec.expires.is_some_and(|e| e < now) {
        return Ok(None);
    }
    if rec
        .last_used
        .is_none_or(|t| now.saturating_sub(t) >= LAST_USED_RESOLUTION_MS)
    {
        rec.last_used = Some(now);
        store.put_json(API_TOKENS, id, &rec)?;
    }
    Ok(Some(rec))
}

/// 持ち主のユーザーを消したときなど、全部消す。
pub fn revoke_all(store: &Store, login: &str) -> Result<(), StoreError> {
    for rec in list(store, login)? {
        revoke(store, login, &rec.id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(scope: TokenScope) -> NewToken {
        NewToken {
            name: "テスト".into(),
            scope,
            boards: None,
            expires_in_days: None,
        }
    }

    #[test]
    fn create_verify_revoke() {
        let store = Store::memory();
        let (token, rec) = create(&store, "alice", &req(TokenScope::Write)).unwrap();
        assert!(token.starts_with("gbt_"));
        assert_eq!(verify(&store, &token).unwrap().unwrap().login, "alice");
        assert!(verify(&store, &token).unwrap().unwrap().last_used.is_some());
        // 秘密を 1 文字変えると通らない。
        let mut bad = token.clone();
        let last = bad.pop().unwrap();
        bad.push(if last == '0' { '1' } else { '0' });
        assert!(verify(&store, &bad).unwrap().is_none());
        assert!(verify(&store, "gbt_x_y").unwrap().is_none());
        assert!(verify(&store, "nope").unwrap().is_none());

        assert_eq!(list(&store, "alice").unwrap().len(), 1);
        assert!(!revoke(&store, "bob", &rec.id).unwrap());
        assert!(revoke(&store, "alice", &rec.id).unwrap());
        assert!(verify(&store, &token).unwrap().is_none());
        assert!(list(&store, "alice").unwrap().is_empty());
    }

    #[test]
    fn board_limits_and_expiry() {
        let store = Store::memory();
        let mut r = req(TokenScope::Read);
        r.boards = Some(vec!["b1".into(), "b1".into()]);
        let (_, rec) = create(&store, "alice", &r).unwrap();
        assert_eq!(rec.boards.as_deref(), Some(&["b1".to_string()][..]));
        assert!(rec.allows_board("b1"));
        assert!(!rec.allows_board("b2"));
        r.boards = Some(vec!["../x".into()]);
        assert!(create(&store, "alice", &r).is_err());
        r.boards = None;
        r.name = "  ".into();
        assert!(create(&store, "alice", &r).is_err());

        // 期限切れ。
        let (token, mut rec) = create(&store, "alice", &req(TokenScope::Read)).unwrap();
        rec.expires = Some(1);
        store.put_json(API_TOKENS, &rec.id, &rec).unwrap();
        assert!(verify(&store, &token).unwrap().is_none());
    }
}
