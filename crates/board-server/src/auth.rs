//! アカウントとログインセッション。
//!
//! パスワードは Argon2id でハッシュ化して保存する。ログインするとランダムなセッショントークンを
//! 発行し、`HttpOnly` の Cookie(`cb_session`)で持たせる。

use std::time::{SystemTime, UNIX_EPOCH};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use board_core::api::{Me, valid_login};
use serde::{Deserialize, Serialize};

use crate::store::{SESSIONS, Store, StoreError, USERS};

pub const SESSION_COOKIE: &str = "cb_session";
/// セッションの有効期間。
pub const SESSION_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_PASSWORD_LEN: usize = 256;
pub const MAX_NAME_CHARS: usize = 40;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRecord {
    pub login: String,
    pub name: String,
    pub password_hash: String,
    pub created: u64,
}

impl UserRecord {
    pub fn me(&self) -> Me {
        Me {
            login: self.login.clone(),
            name: self.name.clone(),
            admin: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRecord {
    pub login: String,
    pub expires: u64,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 推測できないランダムな文字列(16 進)。
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("乱数を取得できません");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// URL に載せる短い ID(英小文字と数字)。
pub fn random_id(len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).expect("乱数を取得できません");
    buf.iter()
        .map(|b| CHARS[*b as usize % CHARS.len()] as char)
        .collect()
}

pub fn hash_password(password: &str) -> Result<String, StoreError> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt)?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| e.to_string())?
        .to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .and_then(|h| Argon2::default().verify_password(password.as_bytes(), &h))
        .is_ok()
}

/// 入力の誤り(利用者に見せる)と、内部のエラー。
#[derive(Debug)]
pub enum AuthError {
    Invalid(String),
    Internal(StoreError),
}

impl From<StoreError> for AuthError {
    fn from(e: StoreError) -> Self {
        Self::Internal(e)
    }
}

/// 表示名を整える。空なら `None`。
pub fn clean_name(name: &str) -> Option<String> {
    let name: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// アカウントを作る(重い処理なので非同期ランタイムの外で呼ぶ)。
pub fn signup(
    store: &Store,
    login: &str,
    name: &str,
    password: &str,
) -> Result<UserRecord, AuthError> {
    let login = login.trim().to_ascii_lowercase();
    if !valid_login(&login) {
        return Err(AuthError::Invalid(
            "ログイン名は 3〜32 文字の英小文字・数字・-・_ にしてください".into(),
        ));
    }
    let name = clean_name(name).unwrap_or_else(|| login.clone());
    if password.chars().count() < MIN_PASSWORD_LEN || password.len() > MAX_PASSWORD_LEN {
        return Err(AuthError::Invalid(format!(
            "パスワードは {MIN_PASSWORD_LEN} 文字以上にしてください"
        )));
    }
    let user = UserRecord {
        login: login.clone(),
        name,
        password_hash: hash_password(password)?,
        created: now_ms(),
    };
    if !store.insert_new_json(USERS, &login, &user)? {
        return Err(AuthError::Invalid(
            "そのログイン名はすでに使われています".into(),
        ));
    }
    Ok(user)
}

/// パスワードを確かめる(重い処理なので非同期ランタイムの外で呼ぶ)。
pub fn check_login(store: &Store, login: &str, password: &str) -> Result<UserRecord, AuthError> {
    let login = login.trim().to_ascii_lowercase();
    let user: Option<UserRecord> = if valid_login(&login) {
        store.get_json(USERS, &login)?
    } else {
        None
    };
    match user {
        Some(u) if verify_password(password, &u.password_hash) => Ok(u),
        // 存在しない場合も同じくらい時間をかけ、ログイン名の有無を推測させない。
        None => {
            let _ = hash_password(password);
            Err(AuthError::Invalid(
                "ログイン名かパスワードが違います".into(),
            ))
        }
        Some(_) => Err(AuthError::Invalid(
            "ログイン名かパスワードが違います".into(),
        )),
    }
}

pub fn get_user(store: &Store, login: &str) -> Result<Option<UserRecord>, StoreError> {
    store.get_json(USERS, login)
}

/// セッションを作ってトークンを返す。
pub fn create_session(store: &Store, login: &str) -> Result<String, StoreError> {
    let token = random_hex(32);
    store.put_json(
        SESSIONS,
        &token,
        &SessionRecord {
            login: login.to_string(),
            expires: now_ms() + SESSION_TTL_MS,
        },
    )?;
    Ok(token)
}

pub fn delete_session(store: &Store, token: &str) -> Result<(), StoreError> {
    store.remove(SESSIONS, token)
}

/// トークンからログイン中のユーザーを引く。期限切れなら消す。
pub fn session_user(store: &Store, token: &str) -> Result<Option<UserRecord>, StoreError> {
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let Some(session) = store.get_json::<SessionRecord>(SESSIONS, token)? else {
        return Ok(None);
    };
    if session.expires < now_ms() {
        store.remove(SESSIONS, token)?;
        return Ok(None);
    }
    get_user(store, &session.login)
}

/// `Cookie` ヘッダーから値を取り出す。
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

/// ログイン用の `Set-Cookie`。
pub fn session_cookie(token: &str, secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        SESSION_TTL_MS / 1000,
        if secure { "; Secure" } else { "" }
    )
}

/// ログアウト用の `Set-Cookie`(すぐ期限切れにする)。
pub fn clear_cookie(secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords() {
        let h = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong horse", &h));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn signup_login_sessions() {
        let store = Store::memory();
        let u = signup(&store, " Alice ", "アリス", "password1").unwrap();
        assert_eq!(u.login, "alice");
        assert!(matches!(
            signup(&store, "alice", "", "password2"),
            Err(AuthError::Invalid(_))
        ));
        assert!(matches!(
            signup(&store, "bob", "", "short"),
            Err(AuthError::Invalid(_))
        ));
        assert!(check_login(&store, "ALICE", "password1").is_ok());
        assert!(check_login(&store, "alice", "password2").is_err());
        assert!(check_login(&store, "nobody", "password1").is_err());

        let token = create_session(&store, "alice").unwrap();
        assert_eq!(
            session_user(&store, &token).unwrap().unwrap().name,
            "アリス"
        );
        assert!(session_user(&store, "deadbeef").unwrap().is_none());
        delete_session(&store, &token).unwrap();
        assert!(session_user(&store, &token).unwrap().is_none());
    }

    #[test]
    fn cookies() {
        let h = "a=1; cb_session=abc; b=2";
        assert_eq!(cookie_value(h, SESSION_COOKIE), Some("abc"));
        assert_eq!(cookie_value(h, "c"), None);
        assert!(session_cookie("t", true).contains("Secure"));
        assert!(clean_name("  \u{7}  ").is_none());
    }

    #[test]
    fn ids() {
        assert_eq!(random_hex(32).len(), 64);
        let id = random_id(12);
        assert_eq!(id.len(), 12);
        assert!(crate::valid_room_name(&id));
    }
}
