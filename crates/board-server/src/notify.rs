//! メールとスマホ(Web Push)への通知。
//!
//! コメントのメンション・返信と、スレッドの期限([`comments::check_due`])で作った通知を、
//! 各自の設定に合わせてメールとプッシュ通知でも届ける。画面のベルの通知はこれまでどおり。
//!
//! - **メール**: `SMTP_URL`(例 `smtps://user:pass@smtp.example.com`)と `MAIL_FROM` を設定すると使える。
//!   送り先のアドレスは各自がボード一覧の「通知の設定」で登録し、確認のメールのリンクを開くまで使わない
//!   (他人のアドレスへ送りつけられないように)。
//! - **プッシュ通知**: Web Push(VAPID)。鍵は `VAPID_PRIVATE_KEY` か、なければサーバーが作って保存する。
//!   通知の中身はプッシュのサービス(Google・Apple・Mozilla)を通さない。中身のない合図だけを送り、
//!   Service Worker が `/api/notifications` から新しい通知を取って表示する。
//!   送り先は既知のプッシュサービスだけ(`PUSH_ALLOWED_HOSTS` で変えられる)。
//! - **期限**: [`spawn_due_checker`] が 1 分ごとに期限を見て、`DUE_REMIND_HOURS`(既定 24)時間前と、
//!   過ぎたときに知らせる。
//!
//! 送信は裏で行い、失敗してもコメントなどの操作は失敗させない。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use board_core::api::{
    Notification, NotificationKind, NotifyKinds, NotifyStatus, PushKeys, PushSubscribe,
    PushUnsubscribe, SetNotify,
};
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::AppState;
use crate::api::{self, ApiResult, Error};
use crate::auth::{self, now_ms};
use crate::comments::{self, Sent};
use crate::store::{NOTIFY_SETTINGS, NOTIFY_VERIFY, PUSH_SUBS, SERVER_KEYS, Store, StoreError};

/// 既定で送ってよいプッシュサービス(先頭が `.` ならその下のホストすべて)。
pub const DEFAULT_PUSH_HOSTS: &[&str] = &[
    "fcm.googleapis.com",
    ".push.services.mozilla.com",
    "web.push.apple.com",
    ".push.apple.com",
    ".notify.windows.com",
];
/// 1 人が登録できるプッシュ通知の端末の数(超えたら古いものから消す)。
const MAX_PUSH_DEVICES: usize = 10;
/// 確認のメールのリンクの有効期間。
const VERIFY_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// 確認のメールを送り直せる間隔。
const VERIFY_INTERVAL_MS: u64 = 60 * 1000;
const MAX_EMAIL_LEN: usize = 254;
const MAX_ENDPOINT_LEN: usize = 1024;
/// 期限を見る間隔。
const DUE_CHECK_INTERVAL: Duration = Duration::from_secs(60);

// ---- 各自の設定 ----

/// 各自の通知の設定(`notify_settings`)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotifyRecord {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub email_verified: bool,
    #[serde(default = "default_email_on")]
    pub email_on: NotifyKinds,
    #[serde(default = "default_push_on")]
    pub push_on: NotifyKinds,
    /// 最後に確認のメールを送った時刻(送りすぎないように)。
    #[serde(default)]
    pub verify_sent: u64,
}

fn default_email_on() -> NotifyKinds {
    // 返信はメールだと多くなりがちなので、既定では送らない。
    NotifyKinds {
        mention: true,
        reply: false,
        due: true,
    }
}

fn default_push_on() -> NotifyKinds {
    NotifyKinds::ALL
}

impl Default for NotifyRecord {
    fn default() -> Self {
        Self {
            email: String::new(),
            email_verified: false,
            email_on: default_email_on(),
            push_on: default_push_on(),
            verify_sent: 0,
        }
    }
}

/// 確認のメールのリンク(`notify_verify`)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifyRecord {
    pub login: String,
    pub email: String,
    pub expires: u64,
}

/// プッシュ通知の送り先(`push_subs`)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushRecord {
    pub endpoint: String,
    #[serde(default)]
    pub keys: Option<PushKeys>,
    pub created: u64,
}

pub fn settings(store: &Store, login: &str) -> Result<NotifyRecord, StoreError> {
    Ok(store
        .get_json::<NotifyRecord>(NOTIFY_SETTINGS, login)?
        .unwrap_or_default())
}

fn endpoint_key(endpoint: &str) -> String {
    let h = sha2::Sha256::digest(endpoint.as_bytes());
    h.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

pub fn push_devices(store: &Store, login: &str) -> Result<Vec<(String, PushRecord)>, StoreError> {
    store.scan_pair::<PushRecord>(PUSH_SUBS, login)
}

/// メールアドレスとして受け付けるか(ざっくり。最後は確認のメールで確かめる)。
pub fn valid_email(email: &str) -> bool {
    email.len() <= MAX_EMAIL_LEN
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.parse::<lettre::Address>().is_ok()
}

// ---- サーバー全体の設定 ----

pub struct NotifyConfig {
    /// `smtp://` か `smtps://` の URL(lettre の書き方)。`None` ならメールは送らない。
    pub smtp_url: Option<String>,
    /// 送り主(`グラボード <noreply@example.com>` など)。
    pub mail_from: Option<String>,
    /// プッシュ通知を使うか。
    pub push: bool,
    /// VAPID の秘密鍵(base64url の 32 バイト)。`None` ならサーバーが作って保存する。
    pub vapid_private_key: Option<String>,
    /// VAPID の `sub`(`mailto:` か `https:`)。プッシュサービスが問い合わせ先に使う。
    pub vapid_subject: Option<String>,
    /// プッシュ通知を送ってよいホスト。`*` なら https ならどこでも。
    pub push_hosts: Vec<String>,
    /// 期限の何ミリ秒前に「近づいた」と知らせるか。
    pub due_lead_ms: u64,
    /// メールに載せるリンクの土台(末尾 `/` 付き)。
    pub public_url: Option<String>,
    /// メールに書く時刻の UTC からのずれ(分)。既定は日本時間(+540)。
    pub utc_offset_minutes: i64,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            smtp_url: None,
            mail_from: None,
            push: false,
            vapid_private_key: None,
            vapid_subject: None,
            push_hosts: DEFAULT_PUSH_HOSTS.iter().map(|h| h.to_string()).collect(),
            due_lead_ms: 24 * 60 * 60 * 1000,
            public_url: None,
            utc_offset_minutes: 9 * 60,
        }
    }
}

impl NotifyConfig {
    /// 環境変数から読む:
    /// `SMTP_URL` `MAIL_FROM` `PUSH_NOTIFICATIONS`(既定 1) `VAPID_PRIVATE_KEY` `VAPID_SUBJECT`
    /// `PUSH_ALLOWED_HOSTS` `DUE_REMIND_HOURS`(既定 24) `PUBLIC_URL` `NOTIFY_UTC_OFFSET`(既定 `+09:00`)。
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let d = Self::default();
        Self {
            smtp_url: var("SMTP_URL"),
            mail_from: var("MAIL_FROM"),
            push: !matches!(var("PUSH_NOTIFICATIONS").as_deref(), Some("0" | "false")),
            vapid_private_key: var("VAPID_PRIVATE_KEY"),
            vapid_subject: var("VAPID_SUBJECT"),
            push_hosts: var("PUSH_ALLOWED_HOSTS")
                .map(|v| {
                    v.split(',')
                        .map(|h| h.trim().to_ascii_lowercase())
                        .filter(|h| !h.is_empty())
                        .collect()
                })
                .unwrap_or(d.push_hosts),
            due_lead_ms: var("DUE_REMIND_HOURS")
                .and_then(|h| h.parse::<f64>().ok())
                .filter(|h| h.is_finite() && *h >= 0.0)
                .map_or(d.due_lead_ms, |h| (h * 3_600_000.0) as u64),
            public_url: var("PUBLIC_URL"),
            utc_offset_minutes: var("NOTIFY_UTC_OFFSET")
                .and_then(|v| parse_offset(&v))
                .unwrap_or(d.utc_offset_minutes),
        }
    }
}

/// `+09:00` `-05:30` `9` などを分に。
fn parse_offset(v: &str) -> Option<i64> {
    let v = v.trim();
    let (sign, rest) = match v.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, v.strip_prefix('+').unwrap_or(v)),
    };
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None => (rest.parse::<i64>().ok()?, 0),
    };
    (h <= 14 && m < 60).then_some(sign * (h * 60 + m))
}

// ---- 送るもの ----

/// 送ったもの(テストで確かめるために記録できる)。
#[derive(Clone, Debug, PartialEq)]
pub enum Outgoing {
    Email {
        to: String,
        subject: String,
        body: String,
    },
    Push {
        endpoint: String,
    },
}

struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

struct Push {
    key: SigningKey,
    /// 公開鍵(非圧縮 65 バイトの base64url)。
    public: String,
    subject: String,
    hosts: Vec<String>,
}

/// メールとプッシュ通知を送るもの。
pub struct Notifier {
    mailer: Option<Mailer>,
    push: Option<Push>,
    public_url: Option<String>,
    pub due_lead_ms: u64,
    utc_offset_minutes: i64,
    http: reqwest::Client,
    /// `Some` なら送らずにここへ記録する(テスト用)。
    record: Option<Arc<Mutex<Vec<Outgoing>>>>,
    /// 記録するときもメールを送れることにする(テスト用)。
    record_email: bool,
}

impl Default for Notifier {
    /// メールもプッシュ通知も使わない。
    fn default() -> Self {
        Self::build(NotifyConfig::default(), None, None)
    }
}

impl Notifier {
    /// 設定から作る。VAPID の鍵が設定になければ `store` に作って保存する。
    pub fn new(config: NotifyConfig, store: &Store) -> Result<Self, StoreError> {
        let mailer = match (&config.smtp_url, &config.mail_from) {
            (Some(url), Some(from)) => {
                let from: Mailbox = from
                    .parse()
                    .map_err(|e| format!("MAIL_FROM を読めません: {e}"))?;
                let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
                    .map_err(|e| format!("SMTP_URL を読めません: {e}"))?
                    .timeout(Some(Duration::from_secs(20)))
                    .build();
                Some(Mailer { transport, from })
            }
            (Some(_), None) => {
                tracing::warn!("SMTP_URL がありますが MAIL_FROM がないので、メールは送りません");
                None
            }
            _ => None,
        };
        let key = if config.push {
            Some(match &config.vapid_private_key {
                Some(k) => signing_key_from_b64(k)
                    .ok_or("VAPID_PRIVATE_KEY は base64url の 32 バイトで指定してください")?,
                None => stored_vapid_key(store)?,
            })
        } else {
            None
        };
        Ok(Self::build(config, mailer, key))
    }

    fn build(config: NotifyConfig, mailer: Option<Mailer>, key: Option<SigningKey>) -> Self {
        let public_url = config.public_url.as_ref().map(|u| {
            let u = u.trim().to_string();
            if u.ends_with('/') { u } else { format!("{u}/") }
        });
        let push = key.map(|key| {
            let public = B64.encode(key.verifying_key().to_encoded_point(false).as_bytes());
            let subject = config
                .vapid_subject
                .clone()
                .or_else(|| mailer.as_ref().map(|m| format!("mailto:{}", m.from.email)))
                .or_else(|| public_url.clone().filter(|u| u.starts_with("https://")))
                .unwrap_or_else(|| "mailto:admin@localhost".into());
            Push {
                key,
                public,
                subject,
                hosts: config.push_hosts.clone(),
            }
        });
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP クライアントを作れません");
        Self {
            mailer,
            push,
            public_url,
            due_lead_ms: config.due_lead_ms,
            utc_offset_minutes: config.utc_offset_minutes,
            http,
            record: None,
            record_email: false,
        }
    }

    /// 送らずに記録する(テスト用)。`email` ならメールを送れることにする。プッシュ通知は既知のホストへ。
    pub fn recording(store: &Store, email: bool) -> (Self, Arc<Mutex<Vec<Outgoing>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut n = Self::new(
            NotifyConfig {
                push: true,
                public_url: Some("https://board.example.com".into()),
                ..NotifyConfig::default()
            },
            store,
        )
        .expect("記録用の通知");
        n.record = Some(log.clone());
        n.record_email = email;
        (n, log)
    }

    pub fn email_available(&self) -> bool {
        self.mailer.is_some() || self.record_email
    }

    /// プッシュ通知の公開鍵。
    pub fn push_key(&self) -> Option<&str> {
        self.push.as_ref().map(|p| p.public.as_str())
    }

    fn push_allowed(&self, endpoint: &str) -> bool {
        let Some(push) = &self.push else {
            return false;
        };
        if endpoint.len() > MAX_ENDPOINT_LEN {
            return false;
        }
        let Ok(url) = reqwest::Url::parse(endpoint) else {
            return false;
        };
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return false;
        };
        push.hosts.iter().any(|h| {
            h == "*"
                || *h == host
                || (h.starts_with('.') && host.ends_with(h.as_str()) && host.len() > h.len())
        })
    }

    /// 作った通知を、各自の設定に合わせてメールとプッシュ通知でも届ける(裏で送る)。
    pub fn deliver(self: &Arc<Self>, store: &Arc<Store>, sent: Vec<Sent>) {
        if sent.is_empty() || (!self.email_available() && self.push.is_none()) {
            return;
        }
        for (login, n) in sent {
            let rec = match settings(store, &login) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("通知の設定を読めません({login}): {e}");
                    continue;
                }
            };
            if rec.email_verified && !rec.email.is_empty() && rec.email_on.allows(n.kind) {
                let (subject, body) = self.email_text(&n);
                self.send_email(rec.email.clone(), subject, body);
            }
            if self.push.is_some() && rec.push_on.allows(n.kind) {
                self.push_all(store, &login);
            }
        }
    }

    /// そのユーザーの端末すべてにプッシュ通知の合図を送る。
    fn push_all(self: &Arc<Self>, store: &Arc<Store>, login: &str) {
        let devices = match push_devices(store, login) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("プッシュ通知の送り先を読めません({login}): {e}");
                return;
            }
        };
        for (key, d) in devices {
            if !self.push_allowed(&d.endpoint) {
                continue;
            }
            let me = self.clone();
            let store = store.clone();
            let login = login.to_string();
            tokio::spawn(async move {
                match me.send_push(&d.endpoint).await {
                    Ok(PushResult::Gone) => {
                        // ブラウザ側で購読が切れた。
                        let _ = store.remove_pair(PUSH_SUBS, &login, &key);
                    }
                    Ok(PushResult::Sent) => {}
                    Err(e) => tracing::warn!("プッシュ通知を送れません: {e}"),
                }
            });
        }
    }

    async fn send_push(&self, endpoint: &str) -> Result<PushResult, String> {
        if let Some(log) = &self.record {
            log.lock().unwrap().push(Outgoing::Push {
                endpoint: endpoint.to_string(),
            });
            return Ok(PushResult::Sent);
        }
        let push = self.push.as_ref().ok_or("プッシュ通知は無効です")?;
        let url = reqwest::Url::parse(endpoint).map_err(|e| e.to_string())?;
        let aud = url.origin().ascii_serialization();
        let jwt = vapid_jwt(
            &push.key,
            &aud,
            &push.subject,
            now_ms() / 1000 + 12 * 60 * 60,
        );
        let res = self
            .http
            .post(url)
            .header("TTL", "86400")
            .header("Urgency", "normal")
            .header("Authorization", format!("vapid t={jwt}, k={}", push.public))
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        match res.status().as_u16() {
            200..=299 => Ok(PushResult::Sent),
            404 | 410 => Ok(PushResult::Gone),
            s => Err(format!("プッシュサービスが {s} を返しました")),
        }
    }

    fn send_email(&self, to: String, subject: String, body: String) {
        if let Some(log) = &self.record {
            log.lock()
                .unwrap()
                .push(Outgoing::Email { to, subject, body });
            return;
        }
        let Some(m) = &self.mailer else { return };
        let to_box: Mailbox = match to.parse() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("メールアドレスを読めません: {e}");
                return;
            }
        };
        let msg = match lettre::Message::builder()
            .from(m.from.clone())
            .to(to_box)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body)
        {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!("メールを作れません: {e}");
                return;
            }
        };
        let transport = m.transport.clone();
        tokio::spawn(async move {
            if let Err(e) = transport.send(msg).await {
                tracing::warn!("メールを送れません: {e}");
            }
        });
    }

    fn link(&self, query: &str) -> Option<String> {
        self.public_url.as_ref().map(|u| format!("{u}{query}"))
    }

    /// 通知のメールの件名と本文。
    pub fn email_text(&self, n: &Notification) -> (String, String) {
        let headline = n.headline();
        let subject = format!("[グラボード] {headline}({})", n.board_title);
        let mut body = format!("{headline}\n\nボード: {}\n", n.board_title);
        if let Some(due) = n.due {
            body.push_str(&format!(
                "期限: {}\n",
                format_time(due, self.utc_offset_minutes)
            ));
        }
        match n.kind {
            NotificationKind::Due | NotificationKind::Overdue => {
                body.push_str(&format!("コメント({}): ", n.from_name));
            }
            _ => {}
        }
        body.push_str(&format!("「{}」\n", n.excerpt));
        let mut query = format!("?room={}", encode(&n.board));
        if let Some(k) = &n.key {
            query.push_str(&format!("&key={}", encode(k)));
        }
        query.push_str(&format!("&comment={}", n.thread));
        if let Some(link) = self.link(&query) {
            body.push_str(&format!("\n開く: {link}\n"));
        }
        body.push_str(
            "\n--\nグラボード からのお知らせです。メールで受け取る通知は、ボード一覧の「通知の設定」で変えられます。\n",
        );
        (subject, body)
    }

    /// 確認のメールを送る。
    fn send_verify(&self, to: &str, name: &str, token: &str) {
        let link = self
            .link(&format!("api/notify/verify?token={token}"))
            .unwrap_or_else(|| format!("/api/notify/verify?token={token}"));
        let body = format!(
            "{name} さん\n\nグラボード の通知をこのメールアドレスで受け取るには、24 時間以内に次のリンクを開いてください。\n\n{link}\n\n心当たりがなければ、このメールは捨ててください。\n"
        );
        self.send_email(
            to.to_string(),
            "[グラボード] メールアドレスの確認".into(),
            body,
        );
    }
}

enum PushResult {
    Sent,
    Gone,
}

fn signing_key_from_b64(s: &str) -> Option<SigningKey> {
    let bytes = B64.decode(s.trim().trim_end_matches('=')).ok()?;
    SigningKey::from_slice(&bytes).ok()
}

/// 保存してある VAPID の鍵。なければ作って保存する。
fn stored_vapid_key(store: &Store) -> Result<SigningKey, StoreError> {
    if let Some(k) = store.get_json::<String>(SERVER_KEYS, "vapid")?
        && let Some(key) = signing_key_from_b64(&k)
    {
        return Ok(key);
    }
    let key = loop {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf)?;
        if let Ok(k) = SigningKey::from_slice(&buf) {
            break k;
        }
    };
    let encoded = B64.encode(key.to_bytes());
    if !store.insert_new_json(SERVER_KEYS, "vapid", &encoded)? {
        // 同時に作られていたら、そちらを使う。
        return stored_vapid_key(store);
    }
    Ok(key)
}

/// VAPID の JWT(ES256)。
fn vapid_jwt(key: &SigningKey, aud: &str, sub: &str, exp: u64) -> String {
    let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = B64.encode(
        serde_json::json!({ "aud": aud, "exp": exp, "sub": sub })
            .to_string()
            .as_bytes(),
    );
    let input = format!("{header}.{claims}");
    let sig: Signature = key.sign(input.as_bytes());
    format!("{input}.{}", B64.encode(sig.to_bytes()))
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// UNIX ミリ秒を `2026/11/03 18:00` に(`offset` 分ずらす)。
pub fn format_time(ms: u64, offset_minutes: i64) -> String {
    let secs = ms as i64 / 1000 + offset_minutes * 60;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // 日付(Howard Hinnant の civil_from_days)。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y}/{m:02}/{d:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

// ---- 期限の見回り ----

/// 1 分ごとに期限を見て、近づいた・過ぎたスレッドを知らせる。
pub fn spawn_due_checker(
    store: Arc<Store>,
    notifier: Arc<Notifier>,
    anonymous_boards: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(DUE_CHECK_INTERVAL);
        loop {
            tick.tick().await;
            let s = store.clone();
            let lead = notifier.due_lead_ms;
            let res = tokio::task::spawn_blocking(move || {
                comments::check_due(&s, now_ms(), lead, anonymous_boards)
            })
            .await;
            match res {
                Ok(Ok(sent)) => notifier.deliver(&store, sent),
                Ok(Err(e)) => tracing::warn!("期限を確かめられません: {e}"),
                Err(e) => tracing::warn!("期限の確認が止まりました: {e}"),
            }
        }
    })
}

// ---- API ----

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/notify/settings", get(get_settings).put(set_settings))
        .route("/notify/verify", get(verify))
        .route("/notify/push", post(subscribe).delete(unsubscribe))
        .route("/notify/test", post(send_test))
}

fn status(state: &AppState, login: &str) -> ApiResult<NotifyStatus> {
    let rec = settings(state.store(), login)?;
    let n = &state.config.notifier;
    Ok(NotifyStatus {
        email: rec.email,
        email_verified: rec.email_verified,
        email_on: rec.email_on,
        push_on: rec.push_on,
        email_available: n.email_available(),
        push_key: n.push_key().map(str::to_string),
        push_devices: push_devices(state.store(), login)?.len() as u32,
    })
}

async fn get_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<NotifyStatus>> {
    let user = api::require_user(&state, &headers)?;
    Ok(Json(status(&state, &user.login)?))
}

async fn set_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<SetNotify>,
) -> ApiResult<Json<NotifyStatus>> {
    let user = api::require_user(&state, &headers)?;
    let store = state.store();
    let mut rec = settings(store, &user.login)?;
    rec.email_on = req.email_on;
    rec.push_on = req.push_on;
    let email = req.email.trim().to_string();
    if email != rec.email {
        if !email.is_empty() {
            if !valid_email(&email) {
                return Err(Error::bad_request("メールアドレスが正しくありません"));
            }
            if !state.config.notifier.email_available() {
                return Err(Error::bad_request(
                    "このサーバーではメールを送れません(管理者が SMTP_URL を設定すると使えます)",
                ));
            }
            let now = now_ms();
            if now < rec.verify_sent + VERIFY_INTERVAL_MS {
                return Err(Error::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "確認のメールは 1 分に 1 回までです。少し待ってから試してください",
                ));
            }
            let token = auth::random_hex(24);
            store.put_json(
                NOTIFY_VERIFY,
                &token,
                &VerifyRecord {
                    login: user.login.clone(),
                    email: email.clone(),
                    expires: now + VERIFY_TTL_MS,
                },
            )?;
            rec.verify_sent = now;
            state
                .config
                .notifier
                .send_verify(&email, &user.name, &token);
        }
        rec.email = email;
        rec.email_verified = false;
    }
    store.put_json(NOTIFY_SETTINGS, &user.login, &rec)?;
    Ok(Json(status(&state, &user.login)?))
}

#[derive(Deserialize)]
struct VerifyQuery {
    token: String,
}

/// 確認のメールのリンク。終わったらボード一覧へ戻す(`?notify=verified` か `?notify=invalid`)。
async fn verify(
    State(state): State<Arc<AppState>>,
    Query(q): Query<VerifyQuery>,
) -> ApiResult<Response> {
    let store = state.store();
    let token = q.token.trim();
    let ok = (|| -> Result<bool, StoreError> {
        if token.is_empty() || token.len() > 128 {
            return Ok(false);
        }
        let Some(v) = store.get_json::<VerifyRecord>(NOTIFY_VERIFY, token)? else {
            return Ok(false);
        };
        store.remove(NOTIFY_VERIFY, token)?;
        if v.expires < now_ms() {
            return Ok(false);
        }
        let mut rec = settings(store, &v.login)?;
        // 確認のメールを送ったあとでアドレスを変えていたら、古いリンクは使えない。
        if rec.email != v.email {
            return Ok(false);
        }
        rec.email_verified = true;
        store.put_json(NOTIFY_SETTINGS, &v.login, &rec)?;
        Ok(true)
    })()?;
    let to = if ok {
        "/?notify=verified"
    } else {
        "/?notify=invalid"
    };
    Ok(Redirect::to(to).into_response())
}

async fn subscribe(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PushSubscribe>,
) -> ApiResult<StatusCode> {
    let user = api::require_user(&state, &headers)?;
    let n = &state.config.notifier;
    if n.push_key().is_none() {
        return Err(Error::bad_request(
            "このサーバーではプッシュ通知を使えません",
        ));
    }
    if !n.push_allowed(&req.endpoint) {
        return Err(Error::bad_request(
            "このブラウザのプッシュ通知の送り先には対応していません",
        ));
    }
    let store = state.store();
    let key = endpoint_key(&req.endpoint);
    store.put_pair(
        PUSH_SUBS,
        &user.login,
        &key,
        &PushRecord {
            endpoint: req.endpoint,
            keys: req.keys.map(|k| PushKeys {
                p256dh: k.p256dh.chars().take(200).collect(),
                auth: k.auth.chars().take(100).collect(),
            }),
            created: now_ms(),
        },
    )?;
    let mut all = push_devices(store, &user.login)?;
    if all.len() > MAX_PUSH_DEVICES {
        all.sort_by_key(|(_, d)| d.created);
        for (k, _) in &all[..all.len() - MAX_PUSH_DEVICES] {
            store.remove_pair(PUSH_SUBS, &user.login, k)?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn unsubscribe(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PushUnsubscribe>,
) -> ApiResult<StatusCode> {
    let user = api::require_user(&state, &headers)?;
    state
        .store()
        .remove_pair(PUSH_SUBS, &user.login, &endpoint_key(&req.endpoint))?;
    Ok(StatusCode::NO_CONTENT)
}

/// 自分にテストの通知を送る(ベルの通知にも 1 件足す)。
async fn send_test(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<NotifyStatus>> {
    let user = api::require_user(&state, &headers)?;
    let st = status(&state, &user.login)?;
    if !(st.email_verified && st.email_available) && st.push_devices == 0 {
        return Err(Error::bad_request(
            "送り先がありません。メールアドレスを確認するか、このブラウザでプッシュ通知を有効にしてください",
        ));
    }
    let n = comments::test_notification(state.store(), &user)?;
    let store = state.store().clone();
    let notifier = state.config.notifier.clone();
    let rec = settings(&store, &user.login)?;
    // 種類の設定にかかわらず、登録した送り先すべてへ送る。
    if rec.email_verified && !rec.email.is_empty() {
        let (subject, body) = notifier.email_text(&n);
        notifier.send_email(rec.email.clone(), subject, body);
    }
    notifier.push_all(&store, &user.login);
    Ok(Json(st))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_and_offsets() {
        // 2026-11-01T00:00:00Z
        let t = 1_793_491_200_000;
        assert_eq!(format_time(t, 0), "2026/11/01 00:00");
        assert_eq!(format_time(t, 540), "2026/11/01 09:00");
        assert_eq!(format_time(t, -300), "2026/10/31 19:00");
        assert_eq!(parse_offset("+09:00"), Some(540));
        assert_eq!(parse_offset("-05:30"), Some(-330));
        assert_eq!(parse_offset("9"), Some(540));
        assert_eq!(parse_offset("x"), None);
    }

    #[test]
    fn vapid_jwt_verifies() {
        use p256::ecdsa::signature::Verifier;
        let store = Store::memory();
        let key = stored_vapid_key(&store).unwrap();
        // 2 回目は保存したものを使う。
        assert_eq!(stored_vapid_key(&store).unwrap().to_bytes(), key.to_bytes());
        let jwt = vapid_jwt(
            &key,
            "https://fcm.googleapis.com",
            "mailto:a@example.com",
            1,
        );
        let (input, sig) = jwt.rsplit_once('.').unwrap();
        let sig = Signature::from_slice(&B64.decode(sig).unwrap()).unwrap();
        key.verifying_key().verify(input.as_bytes(), &sig).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(input.split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
    }

    #[test]
    fn push_hosts() {
        let store = Store::memory();
        let (n, _) = Notifier::recording(&store, false);
        assert!(n.push_allowed("https://fcm.googleapis.com/fcm/send/abc"));
        assert!(n.push_allowed("https://updates.push.services.mozilla.com/wpush/v2/x"));
        assert!(n.push_allowed("https://web.push.apple.com/QK"));
        assert!(!n.push_allowed("http://fcm.googleapis.com/fcm/send/abc"));
        assert!(!n.push_allowed("https://evil.example.com/fcm.googleapis.com"));
        assert!(!n.push_allowed("https://push.apple.com.evil.example/x"));
        assert!(!n.push_allowed("https://user@fcm.googleapis.com/x"));
        assert!(!Notifier::default().push_allowed("https://fcm.googleapis.com/x"));
    }

    #[test]
    fn emails() {
        assert!(valid_email("taro@example.co.jp"));
        assert!(!valid_email("taro example.com"));
        assert!(!valid_email("not-an-address"));
    }
}
