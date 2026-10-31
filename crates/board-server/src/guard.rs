//! 公開するときの守り: ログイン試行・API 呼び出しの回数制限、利用上限、接続元 IP の判定、
//! セキュリティヘッダー。
//!
//! 数え上げはメモリ上だけで行う(再起動すると数え直し)。サーバーを複数台に分けるなら、
//! 前段のリバースプロキシでも制限をかける。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{StatusCode, request::Parts};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::AppState;
use crate::auth::now_ms;

/// 上限と回数制限の設定。`0` は「制限しない」。
#[derive(Clone, Debug)]
pub struct Limits {
    /// 1 つのアカウントへのログイン失敗の上限(15 分あたり)。超えるとしばらくログインできない。
    pub login_failures_per_account: u32,
    /// 1 つの IP からのログイン失敗の上限(15 分あたり)。
    pub login_failures_per_ip: u32,
    /// 1 つの IP からのアカウント作成の試行の上限(1 時間あたり)。
    pub signups_per_ip: u32,
    /// アカウント数の上限。
    pub max_users: usize,
    /// 1 人が持てるボード数の上限。
    pub max_boards_per_user: usize,
    /// 1 つの IP からの API・WebSocket 接続の要求の上限(1 分あたり)。
    pub requests_per_ip_per_minute: u32,
    /// 1 つの IP から同時につなげる WebSocket の数。
    pub connections_per_ip: usize,
    /// 1 つのボードに同時につなげる人数。
    pub peers_per_room: usize,
    /// WebSocket 1 本あたりのメッセージ数の上限(1 秒あたり。短い間はこの 5 倍まで許す)。
    pub messages_per_second: u32,
    /// パスワードのハッシュ計算を同時に何件まで行うか(CPU を使い切らないように)。
    pub password_hash_slots: usize,
    /// ログイン Cookie 付きの WebSocket を受け付ける、ほかのオリジン(`https://host` の形)。
    pub allowed_origins: Vec<String>,
    /// HTML に Content-Security-Policy を付けるか。
    pub csp: bool,
    /// `Strict-Transport-Security` を付けるか(HTTPS で公開するとき)。
    pub hsts: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            login_failures_per_account: 10,
            login_failures_per_ip: 30,
            signups_per_ip: 20,
            max_users: 0,
            max_boards_per_user: 500,
            requests_per_ip_per_minute: 600,
            connections_per_ip: 50,
            peers_per_room: 200,
            messages_per_second: 300,
            password_hash_slots: 4,
            allowed_origins: Vec::new(),
            csp: true,
            hsts: false,
        }
    }
}

impl Limits {
    /// 環境変数から読む(ないものは既定値)。`secure` は `COOKIE_SECURE`。
    pub fn from_env(secure: bool) -> Self {
        let d = Self::default();
        let num = |name: &str, default: u64| -> u64 {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(default)
        };
        let flag = |name: &str, default: bool| match std::env::var(name).as_deref() {
            Ok("0") | Ok("false") => false,
            Ok("1") | Ok("true") => true,
            _ => default,
        };
        Self {
            login_failures_per_account: num(
                "LOGIN_FAILURES_PER_ACCOUNT",
                d.login_failures_per_account.into(),
            ) as u32,
            login_failures_per_ip: num("LOGIN_FAILURES_PER_IP", d.login_failures_per_ip.into())
                as u32,
            signups_per_ip: num("SIGNUPS_PER_IP", d.signups_per_ip.into()) as u32,
            max_users: num("MAX_USERS", d.max_users as u64) as usize,
            max_boards_per_user: num("MAX_BOARDS_PER_USER", d.max_boards_per_user as u64) as usize,
            requests_per_ip_per_minute: num(
                "REQUESTS_PER_MINUTE",
                d.requests_per_ip_per_minute.into(),
            ) as u32,
            connections_per_ip: num("CONNECTIONS_PER_IP", d.connections_per_ip as u64) as usize,
            peers_per_room: num("PEERS_PER_BOARD", d.peers_per_room as u64) as usize,
            messages_per_second: num("MESSAGES_PER_SECOND", d.messages_per_second.into()) as u32,
            password_hash_slots: (num("PASSWORD_HASH_SLOTS", d.password_hash_slots as u64)
                as usize)
                .max(1),
            allowed_origins: std::env::var("ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().trim_end_matches('/').to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect(),
            csp: flag("CSP", true),
            hsts: flag("HSTS", secure),
        }
    }

    /// 「何もしない」設定(テストなどで)。
    pub fn unlimited() -> Self {
        Self {
            login_failures_per_account: 0,
            login_failures_per_ip: 0,
            signups_per_ip: 0,
            max_users: 0,
            max_boards_per_user: 0,
            requests_per_ip_per_minute: 0,
            connections_per_ip: 0,
            peers_per_room: 0,
            messages_per_second: 0,
            ..Self::default()
        }
    }
}

// ---- 回数制限 ----

/// 決まった時間の枠の中で回数を数える。
pub struct Window {
    span_ms: u64,
    max: u32,
    hits: Mutex<HashMap<String, (u64, u32)>>,
}

/// これ以上数えるキーが増えたら、期限切れのものを捨てる。
const PRUNE_AT: usize = 10_000;

impl Window {
    pub fn new(span: Duration, max: u32) -> Self {
        Self {
            span_ms: span.as_millis() as u64,
            max,
            hits: Mutex::default(),
        }
    }

    /// 上限に達していれば、あと何秒待てばよいか。
    pub fn blocked(&self, key: &str) -> Option<u64> {
        if self.max == 0 {
            return None;
        }
        let now = now_ms();
        let hits = self.hits.lock().unwrap();
        match hits.get(key) {
            Some(&(start, n)) if now < start + self.span_ms && n >= self.max => {
                Some((start + self.span_ms - now).div_ceil(1000).max(1))
            }
            _ => None,
        }
    }

    /// 1 回数える。
    pub fn hit(&self, key: &str) {
        if self.max == 0 {
            return;
        }
        let now = now_ms();
        let mut hits = self.hits.lock().unwrap();
        if hits.len() >= PRUNE_AT {
            let span = self.span_ms;
            hits.retain(|_, (start, _)| now < *start + span);
        }
        let e = hits.entry(key.to_string()).or_insert((now, 0));
        if now >= e.0 + self.span_ms {
            *e = (now, 0);
        }
        e.1 = e.1.saturating_add(1);
    }

    /// 数えて、上限を超えていれば待つ秒数を返す。
    pub fn take(&self, key: &str) -> Option<u64> {
        let wait = self.blocked(key);
        if wait.is_none() {
            self.hit(key);
        }
        wait
    }

    pub fn reset(&self, key: &str) {
        self.hits.lock().unwrap().remove(key);
    }
}

/// WebSocket 1 本のメッセージ数を数える(トークンバケット)。
pub struct Bucket {
    rate: f64,
    capacity: f64,
    tokens: f64,
    last: std::time::Instant,
}

impl Bucket {
    /// `per_second` が 0 なら制限しない。
    pub fn new(per_second: u32) -> Option<Self> {
        (per_second > 0).then(|| {
            let rate = f64::from(per_second);
            Self {
                rate,
                capacity: rate * 5.0,
                tokens: rate * 5.0,
                last: std::time::Instant::now(),
            }
        })
    }

    /// 1 通使う。使い切っていれば false。
    pub fn take(&mut self) -> bool {
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// 実行中の数え上げ。
pub struct Guard {
    pub limits: Limits,
    login_by_account: Window,
    login_by_ip: Window,
    signup_by_ip: Window,
    requests_by_ip: Window,
    connections: Mutex<HashMap<IpAddr, usize>>,
    pub hash_slots: Semaphore,
}

impl Guard {
    pub fn new(limits: Limits) -> Self {
        let quarter = Duration::from_secs(15 * 60);
        Self {
            login_by_account: Window::new(quarter, limits.login_failures_per_account),
            login_by_ip: Window::new(quarter, limits.login_failures_per_ip),
            signup_by_ip: Window::new(Duration::from_secs(3600), limits.signups_per_ip),
            requests_by_ip: Window::new(Duration::from_secs(60), limits.requests_per_ip_per_minute),
            connections: Mutex::default(),
            hash_slots: Semaphore::new(limits.password_hash_slots.max(1)),
            limits,
        }
    }

    /// ログインを試してよいか。だめなら待つ秒数。
    pub fn login_blocked(&self, ip: Option<IpAddr>, login: &str) -> Option<u64> {
        let account = self.login_by_account.blocked(&account_key(login));
        let by_ip = ip.and_then(|ip| self.login_by_ip.blocked(&ip.to_string()));
        account.max(by_ip)
    }

    pub fn login_failed(&self, ip: Option<IpAddr>, login: &str) {
        self.login_by_account.hit(&account_key(login));
        if let Some(ip) = ip {
            self.login_by_ip.hit(&ip.to_string());
        }
    }

    pub fn login_succeeded(&self, login: &str) {
        self.login_by_account.reset(&account_key(login));
    }

    /// アカウント作成を 1 回数える。上限を超えていれば待つ秒数。
    pub fn signup_attempt(&self, ip: Option<IpAddr>) -> Option<u64> {
        self.signup_by_ip.take(&ip_key(ip))
    }

    /// WebSocket を 1 本つなぐ。上限を超えていれば `None`。外すと数が戻る。
    pub fn connect(self: &Arc<Self>, ip: Option<IpAddr>) -> Option<Connection> {
        let ip = ip?;
        let max = self.limits.connections_per_ip;
        let mut conns = self.connections.lock().unwrap();
        let n = conns.entry(ip).or_insert(0);
        if max > 0 && *n >= max {
            return None;
        }
        *n += 1;
        Some(Connection {
            guard: self.clone(),
            ip,
        })
    }
}

/// つないでいる WebSocket 1 本(外すと数が戻る)。
pub struct Connection {
    guard: Arc<Guard>,
    ip: IpAddr,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let mut conns = self.guard.connections.lock().unwrap();
        if let Some(n) = conns.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                conns.remove(&self.ip);
            }
        }
    }
}

fn account_key(login: &str) -> String {
    login.trim().to_ascii_lowercase()
}

fn ip_key(ip: Option<IpAddr>) -> String {
    ip.map_or_else(|| "unknown".into(), |ip| ip.to_string())
}

/// 「しばらく待って」の応答。
pub fn too_many(wait_secs: u64, msg: &str) -> Response {
    let mins = wait_secs.div_ceil(60);
    let text = if mins <= 1 {
        format!("{msg}。1 分ほど待ってからやり直してください")
    } else {
        format!("{msg}。{mins} 分ほど待ってからやり直してください")
    };
    let mut res = (
        StatusCode::TOO_MANY_REQUESTS,
        axum::Json(board_core::api::ApiError { error: text }),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&wait_secs.to_string()) {
        res.headers_mut().insert(header::RETRY_AFTER, v);
    }
    res
}

// ---- 接続元 ----

/// リクエストの接続元 IP。`TRUST_PROXY=1` ならプロキシが付けたヘッダーを使う。
pub fn client_ip(
    parts_ext: &axum::http::Extensions,
    headers: &HeaderMap,
    trust_proxy: bool,
) -> Option<IpAddr> {
    if trust_proxy {
        // 直前のプロキシが付け足した値(いちばん右)を使う。左側は利用者が偽れる。
        let forwarded = headers
            .get_all("x-forwarded-for")
            .iter()
            .next_back()
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|s| s.trim().parse::<IpAddr>().ok());
        let real = || {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse().ok())
        };
        if let Some(ip) = forwarded.or_else(real) {
            return Some(ip);
        }
    }
    parts_ext
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(a)| a.ip())
}

/// ハンドラーで接続元 IP を受け取る。分からなければ `None`(テストなど)。
pub struct ClientIp(pub Option<IpAddr>);

impl axum::extract::FromRequestParts<Arc<AppState>> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(client_ip(
            &parts.extensions,
            &parts.headers,
            state.config.trust_proxy,
        )))
    }
}

/// WebSocket の `Origin` が同じサイト(または許可したオリジン)か。ないときは true
/// (ブラウザ以外のクライアント)。ほかのサイトのページからログイン Cookie 付きで
/// つながれるのを防ぐ。
pub fn same_origin(headers: &HeaderMap, allowed: &[String]) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin = origin.trim_end_matches('/').to_ascii_lowercase();
    if allowed.contains(&origin) {
        return true;
    }
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .map(|h| h.to_ascii_lowercase());
    let origin_host = origin.split_once("://").map(|(_, h)| h);
    matches!((origin_host, host), (Some(o), Some(h)) if o == h)
}

// ---- ミドルウェア ----

/// `/api` と `/ws` の呼び出し回数を IP ごとに数える。
pub async fn rate_limit(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api/") || path.starts_with("/ws/") {
        let ip = client_ip(req.extensions(), req.headers(), state.config.trust_proxy);
        if let Some(wait) = state.guard.requests_by_ip.take(&ip_key(ip)) {
            tracing::warn!(?ip, "呼び出しが多すぎるので断りました");
            return too_many(wait, "アクセスが多すぎます");
        }
    }
    next.run(req).await
}

const CSP_BASE: &str = "default-src 'self'; \
    style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob: https:; \
    media-src 'self' data: blob:; \
    font-src 'self' data:; \
    connect-src 'self' https: wss: ws: data: blob:; \
    worker-src 'self' blob:; \
    object-src 'none'; base-uri 'self'; form-action 'self'";

/// HTML の中のインラインスクリプトのハッシュを並べた CSP(インラインは、このハッシュのものだけ動く)。
/// `frame_ancestors` は iframe で埋め込んでよい相手。
pub fn csp_for_html(html: &str, frame_ancestors: &str) -> String {
    let mut hashes = String::new();
    let mut rest = html;
    while let Some(i) = rest.find("<script") {
        rest = &rest[i + 7..];
        let Some(end_tag) = rest.find('>') else { break };
        let attrs = &rest[..end_tag];
        rest = &rest[end_tag + 1..];
        let Some(close) = rest.find("</script>") else {
            break;
        };
        let body = &rest[..close];
        rest = &rest[close..];
        if attrs.contains("src=") {
            continue;
        }
        let digest = Sha256::digest(body.as_bytes());
        hashes.push_str(" 'sha256-");
        hashes.push_str(&base64::engine::general_purpose::STANDARD.encode(digest));
        hashes.push('\'');
    }
    format!(
        "{CSP_BASE}; frame-ancestors {frame_ancestors}; script-src 'self' 'wasm-unsafe-eval'{hashes}"
    )
}

/// 圧縮して配る HTML(`index.html.br` など)を、CSP を計算するために展開する。
fn decode(bytes: &[u8], encoding: Option<&str>) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    match encoding.map(str::trim) {
        None | Some("identity") => return Some(bytes.to_vec()),
        Some("br") => brotli::Decompressor::new(bytes, 4096)
            .read_to_end(&mut out)
            .ok()?,
        Some("gzip") => flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .ok()?,
        _ => return None,
    };
    Some(out)
}

/// `?embed=1`(ほかのサイトへの埋め込み表示)か。
pub(crate) fn is_embed_request(query: Option<&str>) -> bool {
    query.is_some_and(|q| q.split('&').any(|p| p == "embed=1"))
}

/// どの応答にもセキュリティヘッダーを付ける。HTML には CSP も付ける。
///
/// iframe への埋め込みは、`?embed=1` のページなら `EMBED_ALLOWED_ORIGINS` のサイトから、
/// それ以外は同じサイトからだけ許す(クリックジャッキング対策)。
pub async fn security_headers(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let embed = is_embed_request(req.uri().query());
    let res = next.run(req).await;
    let frame_ancestors = if embed {
        state.config.embed_origins.as_str()
    } else {
        "'self'"
    };
    let limits = &state.config.limits;
    let is_html = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/html"));
    let (mut parts, body) = res.into_parts();
    let body = if is_html && limits.csp {
        match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
            Ok(bytes) => {
                let encoding = parts
                    .headers
                    .get(header::CONTENT_ENCODING)
                    .and_then(|v| v.to_str().ok());
                let csp = match decode(&bytes, encoding) {
                    Some(html) => csp_for_html(&String::from_utf8_lossy(&html), frame_ancestors),
                    None => {
                        tracing::warn!(
                            ?encoding,
                            "HTML を読めないので、インラインスクリプトを許す CSP にします"
                        );
                        format!(
                            "{CSP_BASE}; frame-ancestors {frame_ancestors}; script-src 'self' 'wasm-unsafe-eval' 'unsafe-inline'"
                        )
                    }
                };
                if let Ok(v) = HeaderValue::from_str(&csp) {
                    parts.headers.insert(header::CONTENT_SECURITY_POLICY, v);
                }
                Body::from(bytes)
            }
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    } else {
        if is_html
            && let Ok(v) = HeaderValue::from_str(&format!("frame-ancestors {frame_ancestors}"))
        {
            parts.headers.insert(header::CONTENT_SECURITY_POLICY, v);
        }
        body
    };
    let h = &mut parts.headers;
    let set = |h: &mut HeaderMap, name: HeaderName, value: &'static str| {
        h.entry(name).or_insert(HeaderValue::from_static(value));
    };
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    if !embed {
        set(h, header::X_FRAME_OPTIONS, "SAMEORIGIN");
    }
    // 共有リンクの鍵が URL にあるので、ほかのサイトへは送らない。
    set(h, header::REFERRER_POLICY, "same-origin");
    set(
        h,
        HeaderName::from_static("cross-origin-opener-policy"),
        "same-origin",
    );
    if limits.hsts {
        set(h, header::STRICT_TRANSPORT_SECURITY, "max-age=31536000");
    }
    Response::from_parts(parts, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_blocks_and_resets() {
        let w = Window::new(Duration::from_secs(60), 3);
        for _ in 0..3 {
            assert!(w.blocked("a").is_none());
            w.hit("a");
        }
        let wait = w.blocked("a").unwrap();
        assert!((1..=60).contains(&wait));
        assert!(w.blocked("b").is_none());
        w.reset("a");
        assert!(w.blocked("a").is_none());
        // 0 は制限なし。
        let off = Window::new(Duration::from_secs(60), 0);
        for _ in 0..100 {
            assert!(off.take("x").is_none());
        }
    }

    #[test]
    fn bucket_allows_bursts_then_limits() {
        let mut b = Bucket::new(10).unwrap();
        let ok = (0..100).filter(|_| b.take()).count();
        assert!((50..=51).contains(&ok), "{ok}");
        assert!(Bucket::new(0).is_none());
    }

    #[test]
    fn connections_are_counted_per_ip() {
        let g = Arc::new(Guard::new(Limits {
            connections_per_ip: 2,
            ..Limits::default()
        }));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let a = g.connect(Some(ip)).unwrap();
        let _b = g.connect(Some(ip)).unwrap();
        assert!(g.connect(Some(ip)).is_none());
        assert!(g.connect(Some("10.0.0.2".parse().unwrap())).is_some());
        drop(a);
        assert!(g.connect(Some(ip)).is_some());
    }

    #[test]
    fn proxy_headers_only_when_trusted() {
        let mut ext = axum::http::Extensions::new();
        ext.insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))));
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.2.3.4, 5.6.7.8".parse().unwrap());
        assert_eq!(
            client_ip(&ext, &h, false),
            Some("127.0.0.1".parse().unwrap())
        );
        assert_eq!(client_ip(&ext, &h, true), Some("5.6.7.8".parse().unwrap()));
    }

    #[test]
    fn origins() {
        let mut h = HeaderMap::new();
        assert!(same_origin(&h, &[]));
        h.insert(header::HOST, "board.example.com".parse().unwrap());
        h.insert(header::ORIGIN, "https://board.example.com".parse().unwrap());
        assert!(same_origin(&h, &[]));
        h.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!same_origin(&h, &[]));
        assert!(same_origin(&h, &["https://evil.example".into()]));
    }

    #[test]
    fn compressed_html_is_decoded_for_csp() {
        use std::io::Write;
        let html = b"<script>boot()</script>";
        let mut br = Vec::new();
        brotli::CompressorWriter::new(&mut br, 4096, 9, 22)
            .write_all(html)
            .unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(html).unwrap();
        let gz = gz.finish().unwrap();
        assert_eq!(decode(&br, Some("br")).unwrap(), html);
        assert_eq!(decode(&gz, Some("gzip")).unwrap(), html);
        assert_eq!(decode(html, None).unwrap(), html);
        assert!(decode(html, Some("zstd")).is_none());
    }

    #[test]
    fn csp_hashes_inline_scripts_only() {
        let html = "<script type=\"module\">init()</script><script src=\"/a.js\"></script>";
        let csp = csp_for_html(html, "'self'");
        let expected = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(b"init()"));
        assert!(csp.contains(&format!("'sha256-{expected}'")));
        assert_eq!(csp.matches("sha256-").count(), 1);
        assert!(csp.contains("frame-ancestors 'self'"));
    }
}
