//! OpenID Connect によるシングルサインオン(SSO)。
//!
//! 認可コードフロー + PKCE(S256)。ID トークンの署名(RS256 / ES256)は IdP の JWKS で確かめ、
//! `iss`・`aud`・`azp`・`exp`・`nonce` を検証する。暗号処理はすべて Pure Rust(RustCrypto)。
//!
//! 1. `GET /api/sso/oidc/start?next=` で `state`・`nonce`・PKCE を作り、IdP へリダイレクトする。
//!    `state` は Cookie(`cb_oidc`)にも入れて、別のブラウザーから戻ってきた応答を断る。
//! 2. `GET /api/sso/oidc/callback?code=&state=` でコードをトークンに替え、ID トークンを確かめる。
//! 3. IdP の `(iss, sub)` に結びついたアカウントでログインする。初めてなら作る(JIT プロビジョニング)。
//!    同じログイン名のパスワードのアカウントがあっても勝手には結びつけない(乗っ取りを防ぐ)。
//! 4. メールのドメインがチームの「SSO 自動参加ドメイン」に合えば、そのチームのメンバーにする。
//!
//! 進行中のログインはメモリに持つ(10 分で失効)ので、サーバーを複数台にするときは
//! ロードバランサーで同じサーバーに振り分ける必要がある。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::header::{LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use board_core::api::{SsoStatus, TeamRole, valid_login};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::audit::{self, Entry};
use crate::auth::{self, UserRecord, now_ms, random_hex};
use crate::store::{SSO_IDENTITIES, Store, StoreError, USERS};
use crate::teams;

const STATE_COOKIE: &str = "cb_oidc";
const PENDING_TTL: Duration = Duration::from_secs(600);
/// 時計のずれとして許す幅。
const CLOCK_SKEW_SECS: u64 = 120;
/// JWKS を取り直す最短の間隔(知らない `kid` が来たとき)。
const JWKS_REFRESH: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct OidcConfig {
    /// IdP の issuer(`https://accounts.example.com` など)。
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    /// IdP に登録したコールバック URL(`https://graboard.example.com/api/sso/oidc/callback`)。
    pub redirect_url: String,
    /// ログイン画面のボタンに出す名前。
    pub name: String,
    pub scopes: String,
    /// 空でなければ、このドメインのメール(確認済み)の人だけがログインできる。
    pub allowed_domains: Vec<String>,
}

impl OidcConfig {
    /// `OIDC_ISSUER`・`OIDC_CLIENT_ID`・`OIDC_CLIENT_SECRET`・`OIDC_REDIRECT_URL` がそろえば有効。
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Some(Self {
            issuer: var("OIDC_ISSUER")?.trim().to_string(),
            client_id: var("OIDC_CLIENT_ID")?,
            client_secret: var("OIDC_CLIENT_SECRET")?,
            redirect_url: var("OIDC_REDIRECT_URL")?,
            name: var("OIDC_NAME").unwrap_or_else(|| "SSO".into()),
            scopes: var("OIDC_SCOPES").unwrap_or_else(|| "openid email profile".into()),
            allowed_domains: split_domains(&var("OIDC_ALLOWED_DOMAINS").unwrap_or_default()),
        })
    }
}

/// `example.com, Example.org` → `["example.com", "example.org"]`。
pub fn split_domains(s: &str) -> Vec<String> {
    s.split([',', ' ', '\n'])
        .map(|d| d.trim().trim_start_matches('@').to_ascii_lowercase())
        .filter(|d| !d.is_empty() && d.contains('.') && !d.contains('/'))
        .collect()
}

#[derive(Clone, Debug, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

struct Pending {
    nonce: String,
    verifier: String,
    next: Option<String>,
    created: Instant,
}

pub struct Oidc {
    pub config: OidcConfig,
    http: reqwest::Client,
    discovery: tokio::sync::Mutex<Option<Discovery>>,
    jwks: tokio::sync::Mutex<Option<(Instant, Vec<Jwk>)>>,
    pending: Mutex<HashMap<String, Pending>>,
}

/// ID トークンから使う値。
#[derive(Clone, Debug, PartialEq)]
pub struct Claims {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
    pub preferred_username: Option<String>,
}

impl Claims {
    /// 確認済みのメールのドメイン。
    pub fn verified_domain(&self) -> Option<String> {
        if !self.email_verified {
            return None;
        }
        let email = self.email.as_deref()?;
        let (_, domain) = email.rsplit_once('@')?;
        Some(domain.to_ascii_lowercase())
    }
}

impl Oidc {
    pub fn new(config: OidcConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("HTTP クライアント"),
            discovery: tokio::sync::Mutex::new(None),
            jwks: tokio::sync::Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
        }
    }

    async fn discovery(&self) -> Result<Discovery, String> {
        let mut cached = self.discovery.lock().await;
        if let Some(d) = cached.as_ref() {
            return Ok(d.clone());
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.config.issuer.trim_end_matches('/')
        );
        let d: Discovery = self
            .http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("IdP の設定を読めません: {e}"))?
            .json()
            .await
            .map_err(|e| format!("IdP の設定が読めない形です: {e}"))?;
        if d.issuer.trim_end_matches('/') != self.config.issuer.trim_end_matches('/') {
            return Err(format!(
                "IdP の issuer が設定と違います({} と {})",
                d.issuer, self.config.issuer
            ));
        }
        *cached = Some(d.clone());
        Ok(d)
    }

    /// JWKS(`refresh` なら、前に取ってから時間がたっていれば取り直す)。
    async fn keys(&self, jwks_uri: &str, refresh: bool) -> Result<Vec<Jwk>, String> {
        let mut cached = self.jwks.lock().await;
        if let Some((at, keys)) = cached.as_ref()
            && (!refresh || at.elapsed() < JWKS_REFRESH)
        {
            return Ok(keys.clone());
        }
        let set: JwkSet = self
            .http
            .get(jwks_uri)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("IdP の公開鍵を読めません: {e}"))?
            .json()
            .await
            .map_err(|e| format!("IdP の公開鍵が読めない形です: {e}"))?;
        *cached = Some((Instant::now(), set.keys.clone()));
        Ok(set.keys)
    }

    fn take_pending(&self, state: &str) -> Option<Pending> {
        let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, p| p.created.elapsed() < PENDING_TTL);
        map.remove(state)
    }

    fn put_pending(&self, state: String, p: Pending) {
        let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, p| p.created.elapsed() < PENDING_TTL);
        // 大量に始めても膨らみすぎないように。
        if map.len() > 10_000 {
            map.clear();
        }
        map.insert(state, p);
    }

    /// IdP へのリダイレクト先を作る。
    pub async fn start(&self, next: Option<String>) -> Result<(String, String), String> {
        let d = self.discovery().await?;
        let state = random_hex(16);
        let nonce = random_hex(16);
        let verifier = random_hex(32);
        let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
        let sep = if d.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!(
            "{}{sep}response_type=code&client_id={}&redirect_uri={}&scope={}&state={state}&nonce={nonce}&code_challenge={challenge}&code_challenge_method=S256",
            d.authorization_endpoint,
            urlencode(&self.config.client_id),
            urlencode(&self.config.redirect_url),
            urlencode(&self.config.scopes),
        );
        self.put_pending(
            state.clone(),
            Pending {
                nonce,
                verifier,
                next,
                created: Instant::now(),
            },
        );
        Ok((url, state))
    }

    /// コードをトークンに替えて ID トークンを確かめる。戻り先(`next`)も返す。
    async fn finish(&self, code: &str, state: &str) -> Result<(Claims, Option<String>), String> {
        let pending = self
            .take_pending(state)
            .ok_or("ログインの手続きが見つからないか、期限切れです。もう一度お試しください")?;
        let d = self.discovery().await?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.config.redirect_url.as_str()),
            ("code_verifier", pending.verifier.as_str()),
        ];
        let basic = d
            .token_endpoint_auth_methods_supported
            .as_ref()
            .is_none_or(|m| m.iter().any(|x| x == "client_secret_basic"));
        let mut req = self.http.post(&d.token_endpoint);
        if basic {
            req = req.basic_auth(
                urlencode(&self.config.client_id),
                Some(urlencode(&self.config.client_secret)),
            );
        } else {
            form.push(("client_id", self.config.client_id.as_str()));
            form.push(("client_secret", self.config.client_secret.as_str()));
        }
        let body = form
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let res = req
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("IdP につながりません: {e}"))?;
        let status = res.status();
        let json: Value = res
            .json()
            .await
            .map_err(|e| format!("IdP の応答が読めません: {e}"))?;
        if !status.is_success() {
            let why = json
                .get("error_description")
                .or_else(|| json.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("不明なエラー");
            return Err(format!("IdP がトークンの発行を断りました: {why}"));
        }
        let id_token = json
            .get("id_token")
            .and_then(Value::as_str)
            .ok_or("IdP の応答に ID トークンがありません")?;
        let now = now_ms() / 1000;
        let check = |keys: &[Jwk]| {
            verify_id_token(
                id_token,
                keys,
                &d.issuer,
                &self.config.client_id,
                &pending.nonce,
                now,
            )
        };
        let keys = self.keys(&d.jwks_uri, false).await?;
        let claims = match check(&keys) {
            // 鍵が入れ替わったのかもしれないので取り直す。
            Err(TokenError::UnknownKey) => check(&self.keys(&d.jwks_uri, true).await?),
            other => other,
        }
        .map_err(|e| e.to_string())?;
        Ok((claims, pending.next))
    }
}

#[derive(Debug, PartialEq)]
pub enum TokenError {
    Malformed,
    UnsupportedAlg(String),
    UnknownKey,
    BadSignature,
    Claim(&'static str),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => write!(f, "ID トークンの形が正しくありません"),
            Self::UnsupportedAlg(a) => write!(f, "対応していない署名方式です({a})"),
            Self::UnknownKey => write!(f, "ID トークンの署名鍵が見つかりません"),
            Self::BadSignature => write!(f, "ID トークンの署名が正しくありません"),
            Self::Claim(c) => write!(f, "ID トークンの {c} が正しくありません"),
        }
    }
}

/// ID トークン(JWS コンパクト形式)を確かめて中身を返す。
pub fn verify_id_token(
    token: &str,
    keys: &[Jwk],
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now_secs: u64,
) -> Result<Claims, TokenError> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };
    let header: Value = decode_json(h)?;
    let alg = header
        .get("alg")
        .and_then(Value::as_str)
        .ok_or(TokenError::Malformed)?;
    let kty = match alg {
        "RS256" => "RSA",
        "ES256" => "EC",
        other => return Err(TokenError::UnsupportedAlg(other.to_string())),
    };
    let kid = header.get("kid").and_then(Value::as_str);
    let sig = B64.decode(s).map_err(|_| TokenError::Malformed)?;
    let signed = &token.as_bytes()[..h.len() + 1 + p.len()];
    let candidates: Vec<&Jwk> = keys
        .iter()
        .filter(|k| k.kty == kty)
        .filter(|k| k.use_.as_deref().is_none_or(|u| u == "sig"))
        .filter(|k| k.alg.as_deref().is_none_or(|a| a == alg))
        .filter(|k| kid.is_none() || k.kid.as_deref() == kid)
        .collect();
    if candidates.is_empty() {
        return Err(TokenError::UnknownKey);
    }
    if !candidates.iter().any(|k| verify_sig(k, alg, signed, &sig)) {
        return Err(TokenError::BadSignature);
    }

    let c: Value = decode_json(p)?;
    let s = |k: &str| c.get(k).and_then(Value::as_str).map(str::to_string);
    let iss = s("iss").ok_or(TokenError::Claim("iss"))?;
    if iss.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(TokenError::Claim("iss"));
    }
    let aud_ok = match c.get("aud") {
        Some(Value::String(a)) => a == client_id,
        Some(Value::Array(list)) => {
            list.iter().any(|a| a.as_str() == Some(client_id))
                && (list.len() == 1 || s("azp").as_deref() == Some(client_id))
        }
        _ => false,
    };
    if !aud_ok {
        return Err(TokenError::Claim("aud"));
    }
    if let Some(azp) = s("azp")
        && azp != client_id
    {
        return Err(TokenError::Claim("azp"));
    }
    let exp = c
        .get("exp")
        .and_then(Value::as_u64)
        .ok_or(TokenError::Claim("exp"))?;
    if exp + CLOCK_SKEW_SECS < now_secs {
        return Err(TokenError::Claim("exp"));
    }
    if let Some(iat) = c.get("iat").and_then(Value::as_u64)
        && iat > now_secs + CLOCK_SKEW_SECS
    {
        return Err(TokenError::Claim("iat"));
    }
    if s("nonce").as_deref() != Some(nonce) {
        return Err(TokenError::Claim("nonce"));
    }
    let subject = s("sub")
        .filter(|v| !v.is_empty())
        .ok_or(TokenError::Claim("sub"))?;
    let email_verified = match c.get("email_verified") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(v)) => v == "true",
        _ => false,
    };
    Ok(Claims {
        issuer: iss,
        subject,
        email: s("email"),
        email_verified,
        name: s("name"),
        preferred_username: s("preferred_username"),
    })
}

fn decode_json(part: &str) -> Result<Value, TokenError> {
    let bytes = B64.decode(part).map_err(|_| TokenError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| TokenError::Malformed)
}

fn verify_sig(key: &Jwk, alg: &str, signed: &[u8], sig: &[u8]) -> bool {
    let b = |v: &Option<String>| v.as_deref().and_then(|s| B64.decode(s).ok());
    match alg {
        "RS256" => {
            use rsa::pkcs1v15::{Signature, VerifyingKey};
            use rsa::signature::Verifier;
            let (Some(n), Some(e)) = (b(&key.n), b(&key.e)) else {
                return false;
            };
            let Ok(pk) = rsa::RsaPublicKey::new(
                rsa::BigUint::from_bytes_be(&n),
                rsa::BigUint::from_bytes_be(&e),
            ) else {
                return false;
            };
            // 短すぎる鍵は受け付けない。
            if n.len() < 256 {
                return false;
            }
            let Ok(sig) = Signature::try_from(sig) else {
                return false;
            };
            VerifyingKey::<Sha256>::new(pk).verify(signed, &sig).is_ok()
        }
        "ES256" => {
            use p256::ecdsa::signature::Verifier;
            use p256::ecdsa::{Signature, VerifyingKey};
            if key.crv.as_deref() != Some("P-256") {
                return false;
            }
            let (Some(x), Some(y)) = (b(&key.x), b(&key.y)) else {
                return false;
            };
            if x.len() != 32 || y.len() != 32 {
                return false;
            }
            let point = p256::EncodedPoint::from_affine_coordinates(
                p256::FieldBytes::from_slice(&x),
                p256::FieldBytes::from_slice(&y),
                false,
            );
            let Ok(vk) = VerifyingKey::from_encoded_point(&point) else {
                return false;
            };
            let Ok(sig) = Signature::from_slice(sig) else {
                return false;
            };
            vk.verify(signed, &sig).is_ok()
        }
        _ => false,
    }
}

/// `application/x-www-form-urlencoded` / URL のクエリ用に符号化する。
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---- アカウントとの結びつけ ----

fn identity_key(issuer: &str, subject: &str) -> String {
    format!("{}\n{subject}", issuer.trim_end_matches('/'))
}

/// IdP の人に結びついたアカウントを返す。なければ作る。
pub fn account_for(store: &Store, claims: &Claims) -> Result<(UserRecord, bool), StoreError> {
    let key = identity_key(&claims.issuer, &claims.subject);
    if let Some(login) = store.get_json::<String>(SSO_IDENTITIES, &key)?
        && let Some(user) = auth::get_user(store, &login)?
    {
        return Ok((user, false));
    }
    let base = login_candidate(claims);
    let name = claims
        .name
        .as_deref()
        .and_then(auth::clean_name)
        .unwrap_or_else(|| base.clone());
    for n in 1..1000u32 {
        let login = if n == 1 {
            base.clone()
        } else {
            let suffix = format!("-{n}");
            format!("{}{suffix}", &base[..base.len().min(32 - suffix.len())])
        };
        let user = UserRecord {
            login: login.clone(),
            name: name.clone(),
            // パスワードではログインできない(空のハッシュは必ず失敗する)。
            password_hash: String::new(),
            created: now_ms(),
        };
        if store.insert_new_json(USERS, &login, &user)? {
            store.put_json(SSO_IDENTITIES, &key, &login)?;
            return Ok((user, true));
        }
    }
    Err("ログイン名を決められませんでした".into())
}

/// `preferred_username` かメールの @ の前から、ログイン名の候補を作る。
pub fn login_candidate(claims: &Claims) -> String {
    let raw = claims
        .preferred_username
        .as_deref()
        .or(claims.email.as_deref())
        .map(|s| s.split('@').next().unwrap_or(s))
        .unwrap_or("user");
    let mut login: String = raw
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    login = login.trim_matches('-').chars().take(32).collect();
    while login.contains("--") {
        login = login.replace("--", "-");
    }
    if login.len() < 3 {
        login = format!("user-{login}").trim_end_matches('-').to_string();
    }
    if !valid_login(&login) {
        login = "user".into();
    }
    login
}

/// メールのドメインが自動参加ドメインに合うチームへ入れる。入ったチームの ID を返す。
pub fn auto_join(store: &Store, login: &str, domain: &str) -> Result<Vec<String>, StoreError> {
    let mut joined = Vec::new();
    for mut team in teams::all(store)? {
        if team.sso_domains.iter().any(|d| d == domain) && !team.members.contains_key(login) {
            team.members.insert(login.to_string(), TeamRole::Member);
            teams::save_member_change(store, &team, login)?;
            joined.push(team.id.clone());
        }
    }
    Ok(joined)
}

// ---- HTTP ----

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sso", get(status))
        .route("/sso/oidc/start", get(start))
        .route("/sso/oidc/callback", get(callback))
}

async fn status(State(state): State<Arc<AppState>>) -> Json<SsoStatus> {
    Json(SsoStatus {
        password_login: state.config.password_login,
        oidc: state.config.oidc.as_ref().map(|o| o.config.name.clone()),
    })
}

#[derive(Deserialize)]
pub struct StartQuery {
    next: Option<String>,
}

/// ログイン後の戻り先として受け付けるのは `?room=…` だけ。
pub fn safe_next(next: Option<String>) -> Option<String> {
    next.filter(|n| {
        n.starts_with("?room=")
            && n.len() <= 200
            && n.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'?' | b'=' | b'&' | b'-' | b'_'))
    })
}

fn redirect(to: &str, cookie: Option<String>) -> Response {
    let mut res = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        res.headers_mut().insert(LOCATION, v);
    }
    if let Some(c) = cookie.and_then(|c| HeaderValue::from_str(&c).ok()) {
        res.headers_mut().append(SET_COOKIE, c);
    }
    res
}

fn error_redirect(msg: &str) -> Response {
    tracing::warn!("SSO に失敗: {msg}");
    redirect(
        &format!("/?sso_error={}", urlencode(msg)),
        Some(clear_state_cookie()),
    )
}

fn state_cookie(state: &str, secure: bool) -> String {
    format!(
        "{STATE_COOKIE}={state}; Path=/api/sso; HttpOnly; SameSite=Lax; Max-Age={}{}",
        PENDING_TTL.as_secs(),
        if secure { "; Secure" } else { "" }
    )
}

fn clear_state_cookie() -> String {
    format!("{STATE_COOKIE}=; Path=/api/sso; HttpOnly; SameSite=Lax; Max-Age=0")
}

async fn start(State(state): State<Arc<AppState>>, Query(q): Query<StartQuery>) -> Response {
    let Some(oidc) = state.config.oidc.clone() else {
        return (StatusCode::NOT_FOUND, "SSO は設定されていません").into_response();
    };
    match oidc.start(safe_next(q.next)).await {
        Ok((url, st)) => redirect(&url, Some(state_cookie(&st, state.config.secure_cookies))),
        Err(e) => error_redirect(&e),
    }
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn callback(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let Some(oidc) = state.config.oidc.clone() else {
        return (StatusCode::NOT_FOUND, "SSO は設定されていません").into_response();
    };
    if let Some(e) = q.error {
        return error_redirect(&format!(
            "IdP がログインを断りました: {}",
            q.error_description.unwrap_or(e)
        ));
    }
    let (Some(code), Some(st)) = (q.code, q.state) else {
        return error_redirect("IdP からの応答が足りません");
    };
    // 同じブラウザーで始めたログインか。
    let cookie_state = headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|h| auth::cookie_value(h, STATE_COOKIE));
    if cookie_state != Some(st.as_str()) {
        // 手続きは使えなくしておく。
        let _ = oidc.take_pending(&st);
        return error_redirect("ログインを始めたブラウザーと違います。もう一度お試しください");
    }
    let (claims, next) = match oidc.finish(&code, &st).await {
        Ok(v) => v,
        Err(e) => return error_redirect(&e),
    };
    let domain = claims.verified_domain();
    let allowed = &oidc.config.allowed_domains;
    if !allowed.is_empty() && !domain.as_ref().is_some_and(|d| allowed.contains(d)) {
        audit::record(
            &state,
            &headers,
            Entry::new("auth.login_failed")
                .target(claims.email.clone().unwrap_or(claims.subject.clone()))
                .detail("SSO: 許可されていないドメイン"),
        );
        return error_redirect("このアカウントのドメインではログインできません");
    }
    let store = state.store().clone();
    let c = claims.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<_, StoreError> {
        let (user, created) = account_for(&store, &c)?;
        let joined = match c.verified_domain() {
            Some(d) => auto_join(&store, &user.login, &d)?,
            None => Vec::new(),
        };
        let token = auth::create_session(&store, &user.login)?;
        Ok((user, created, joined, token))
    })
    .await;
    let (user, created, joined, token) = match result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            tracing::error!("SSO のアカウント処理に失敗: {e}");
            return error_redirect("サーバーでエラーが起きました");
        }
        Err(e) => {
            tracing::error!("SSO のアカウント処理に失敗: {e}");
            return error_redirect("サーバーでエラーが起きました");
        }
    };
    if created {
        tracing::info!(login = %user.login, "SSO でアカウント作成");
        audit::record(
            &state,
            &headers,
            Entry::new("auth.signup").actor(&user.login).detail("SSO"),
        );
    }
    for team in joined {
        audit::record(
            &state,
            &headers,
            Entry::new("team.member")
                .actor(&user.login)
                .team(Some(team))
                .target(&user.login)
                .detail("member (SSO 自動参加)"),
        );
    }
    audit::record(
        &state,
        &headers,
        Entry::new("auth.sso")
            .actor(&user.login)
            .detail(claims.email.clone().unwrap_or_default()),
    );
    let mut res = redirect(
        &format!("/{}", next.unwrap_or_default()),
        Some(auth::session_cookie(&token, state.config.secure_cookies)),
    );
    if let Ok(v) = HeaderValue::from_str(&clear_state_cookie()) {
        res.headers_mut().append(SET_COOKIE, v);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::Signer;

    fn es_key() -> (SigningKey, Jwk) {
        let sk = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let point = sk.verifying_key().to_encoded_point(false);
        let jwk = Jwk {
            kty: "EC".into(),
            kid: Some("k1".into()),
            alg: Some("ES256".into()),
            use_: Some("sig".into()),
            n: None,
            e: None,
            crv: Some("P-256".into()),
            x: Some(B64.encode(point.x().unwrap())),
            y: Some(B64.encode(point.y().unwrap())),
        };
        (sk, jwk)
    }

    fn sign_es(sk: &SigningKey, header: &Value, claims: &Value) -> String {
        let h = B64.encode(header.to_string());
        let p = B64.encode(claims.to_string());
        let sig: p256::ecdsa::Signature = sk.sign(format!("{h}.{p}").as_bytes());
        format!("{h}.{p}.{}", B64.encode(sig.to_bytes()))
    }

    fn claims(now: u64) -> Value {
        serde_json::json!({
            "iss": "https://idp.example.com",
            "sub": "u-123",
            "aud": "graboard",
            "exp": now + 300,
            "iat": now,
            "nonce": "n0nce",
            "email": "Taro.Yamada@Example.co.jp",
            "email_verified": true,
            "name": "山田 太郎",
        })
    }

    #[test]
    fn verifies_es256() {
        let (sk, jwk) = es_key();
        let now = 1_800_000_000;
        let header = serde_json::json!({"alg": "ES256", "kid": "k1"});
        let ok = sign_es(&sk, &header, &claims(now));
        let c = verify_id_token(
            &ok,
            std::slice::from_ref(&jwk),
            "https://idp.example.com/",
            "graboard",
            "n0nce",
            now,
        )
        .unwrap();
        assert_eq!(c.subject, "u-123");
        assert_eq!(c.verified_domain().as_deref(), Some("example.co.jp"));

        let check = |token: &str| {
            verify_id_token(
                token,
                std::slice::from_ref(&jwk),
                "https://idp.example.com",
                "graboard",
                "n0nce",
                now,
            )
        };
        // nonce・aud・期限・iss の違い。
        let mut c2 = claims(now);
        c2["nonce"] = "other".into();
        assert_eq!(
            check(&sign_es(&sk, &header, &c2)),
            Err(TokenError::Claim("nonce"))
        );
        let mut c2 = claims(now);
        c2["aud"] = serde_json::json!(["graboard", "other"]);
        assert_eq!(
            check(&sign_es(&sk, &header, &c2)),
            Err(TokenError::Claim("aud"))
        );
        c2["azp"] = "graboard".into();
        assert!(check(&sign_es(&sk, &header, &c2)).is_ok());
        let mut c2 = claims(now);
        c2["exp"] = (now - 1000).into();
        assert_eq!(
            check(&sign_es(&sk, &header, &c2)),
            Err(TokenError::Claim("exp"))
        );
        let mut c2 = claims(now);
        c2["iss"] = "https://evil.example.com".into();
        assert_eq!(
            check(&sign_es(&sk, &header, &c2)),
            Err(TokenError::Claim("iss"))
        );
        // 中身を書き換えると署名が合わない。
        let mut parts: Vec<String> = ok.split('.').map(str::to_string).collect();
        let mut c2 = claims(now);
        c2["sub"] = "admin".into();
        parts[1] = B64.encode(c2.to_string());
        assert_eq!(check(&parts.join(".")), Err(TokenError::BadSignature));
        // alg=none や HS256 は受け付けない。
        let none = format!(
            "{}.{}.",
            B64.encode(r#"{"alg":"none"}"#),
            B64.encode(claims(now).to_string())
        );
        assert_eq!(check(&none), Err(TokenError::UnsupportedAlg("none".into())));
        let hs = sign_es(&sk, &serde_json::json!({"alg": "HS256"}), &claims(now));
        assert_eq!(check(&hs), Err(TokenError::UnsupportedAlg("HS256".into())));
        // 知らない kid。
        let other = sign_es(
            &sk,
            &serde_json::json!({"alg": "ES256", "kid": "k2"}),
            &claims(now),
        );
        assert_eq!(check(&other), Err(TokenError::UnknownKey));
    }

    #[test]
    fn verifies_rs256() {
        use rsa::pkcs1v15::SigningKey as RsaSigningKey;
        use rsa::pkcs8::DecodePrivateKey;
        use rsa::signature::{SignatureEncoding, Signer as _};
        use rsa::traits::PublicKeyParts;
        let sk = rsa::RsaPrivateKey::from_pkcs8_pem(TEST_RSA_KEY).unwrap();
        let pk = sk.to_public_key();
        let jwk = Jwk {
            kty: "RSA".into(),
            kid: None,
            alg: None,
            use_: None,
            n: Some(B64.encode(pk.n().to_bytes_be())),
            e: Some(B64.encode(pk.e().to_bytes_be())),
            crv: None,
            x: None,
            y: None,
        };
        let now = 1_800_000_000;
        let h = B64.encode(r#"{"alg":"RS256","kid":"whatever"}"#);
        let p = B64.encode(claims(now).to_string());
        let signer = RsaSigningKey::<Sha256>::new(sk);
        let sig = signer.sign(format!("{h}.{p}").as_bytes());
        let token = format!("{h}.{p}.{}", B64.encode(sig.to_bytes()));
        // kid の付いていない鍵は kid の付いたトークンには使わない。
        assert_eq!(
            verify_id_token(
                &token,
                std::slice::from_ref(&jwk),
                "https://idp.example.com",
                "graboard",
                "n0nce",
                now
            ),
            Err(TokenError::UnknownKey)
        );
        let jwk = Jwk {
            kid: Some("whatever".into()),
            ..jwk
        };
        let c = verify_id_token(
            &token,
            &[jwk],
            "https://idp.example.com",
            "graboard",
            "n0nce",
            now,
        )
        .unwrap();
        assert_eq!(c.name.as_deref(), Some("山田 太郎"));
    }

    #[test]
    fn provisioning() {
        let store = Store::memory();
        let mk = |sub: &str, user: Option<&str>, email: Option<&str>| Claims {
            issuer: "https://idp.example.com".into(),
            subject: sub.into(),
            email: email.map(str::to_string),
            email_verified: true,
            name: None,
            preferred_username: user.map(str::to_string),
        };
        assert_eq!(
            login_candidate(&mk("1", Some("Taro.Yamada"), None)),
            "taro-yamada"
        );
        assert_eq!(login_candidate(&mk("1", None, Some("ab@x.jp"))), "user-ab");
        assert_eq!(login_candidate(&mk("1", Some("山田"), None)), "user");

        // パスワードのアカウント「alice」があっても乗っ取らない。
        auth::signup(&store, "alice", "", "password123").unwrap();
        let (u, created) = account_for(&store, &mk("s1", Some("alice"), None)).unwrap();
        assert!(created);
        assert_eq!(u.login, "alice-2");
        assert!(auth::check_login(&store, "alice-2", "").is_err());
        // 2 回目は同じアカウント。
        let (u2, created) = account_for(&store, &mk("s1", Some("renamed"), None)).unwrap();
        assert!(!created);
        assert_eq!(u2.login, "alice-2");

        // 自動参加。
        let mut t = teams::create(&store, "alice", "開発").unwrap();
        t.sso_domains = vec!["example.com".into()];
        teams::put_team(&store, &t).unwrap();
        assert_eq!(
            auto_join(&store, "alice-2", "example.com").unwrap(),
            vec![t.id.clone()]
        );
        assert!(
            auto_join(&store, "alice-2", "example.com")
                .unwrap()
                .is_empty()
        );
        assert!(auto_join(&store, "bob", "evil.com").unwrap().is_empty());
        assert_eq!(
            split_domains("Example.com, @foo.jp bar"),
            vec!["example.com", "foo.jp"]
        );
        assert_eq!(
            safe_next(Some("?room=abc&key=12".into())).as_deref(),
            Some("?room=abc&key=12")
        );
        assert_eq!(safe_next(Some("//evil.com".into())), None);
        assert_eq!(safe_next(Some("?room=a/../x".into())), None);
    }

    const TEST_RSA_KEY: &str = include_str!("../tests/data/test_rsa.pem");
}
