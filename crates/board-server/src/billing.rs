//! 料金プラン(無料・プロ)と Stripe での支払い。
//!
//! - 決め方は環境変数で切り替える([`BillingConfig::from_env`])。
//!   - 何も設定しなければ **off**: 料金プランを使わない(今までどおり、全員が上限なし)。自分たちだけで
//!     使うセルフホストはこれでよい。
//!   - `STRIPE_SECRET_KEY`・`STRIPE_WEBHOOK_SECRET`・`STRIPE_PRICE_PRO` をそろえると **stripe**:
//!     Stripe Checkout で申し込み、カスタマーポータルで支払い方法の変更・解約をする。
//!     プランの切り替えは Stripe からの Webhook(署名を確かめる)だけで行う。
//!   - `BILLING=mock` で **mock**: Stripe の代わりにこのサーバーの「テスト決済」画面を出す。
//!     キーなしで画面と上限の動きを確かめるためのもので、本番では使わない。
//! - プランは「ボードのオーナー」に付く。ボード数はその人の持つボード、メンバー数・動画メモの容量は
//!   ボードごと、AI 機能の回数はボードのオーナー(オーナーのいないボードなら呼んだ人)の月ごとの回数で数える。
//! - 上限に達したら `402 Payment Required` と理由を返す。プランを下げても、すでにあるボードや
//!   メンバーはそのまま残り、新しく増やせなくなるだけ。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use board_core::api::{Billing, BillingRedirect, PlanId, PlanLimits, PlanOffer, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AppState;
use crate::api::{self, ApiResult, Error};
use crate::audit::{self, Entry};
use crate::auth::{now_ms, random_hex};
use crate::boards;
use crate::storage::{hex, hmac_sha256};
use crate::store::{BILLING, BILLING_CUSTOMERS, BILLING_EVENTS, Store, StoreError, USAGE};

/// Webhook の署名の時刻のずれの許容(Stripe の既定と同じ 5 分)。
const SIGNATURE_TOLERANCE_SECS: u64 = 300;
/// テスト決済の申し込みの有効期限。
const MOCK_SESSION_TTL_MS: u64 = 30 * 60 * 1000;

// ---- 設定 ----

#[derive(Clone, Debug)]
pub struct StripeConfig {
    /// `sk_test_...` か `sk_live_...`。
    pub secret_key: String,
    /// Webhook の署名の鍵(`whsec_...`)。
    pub webhook_secret: String,
    /// プロプランの価格 ID(`price_...`)。
    pub price_pro: String,
    /// テストで偽の API に向けるため(既定 `https://api.stripe.com`)。
    pub api_base: String,
}

#[derive(Clone, Debug, Default)]
pub enum Mode {
    /// 料金プランを使わない。
    #[default]
    Off,
    /// テスト用の決済画面(Stripe を使わない)。
    Mock,
    Stripe(StripeConfig),
}

impl Mode {
    pub fn name(&self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Mock => "mock",
            Mode::Stripe(_) => "stripe",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BillingConfig {
    pub mode: Mode,
    pub free: PlanLimits,
    pub pro: PlanLimits,
    /// 料金画面に出すプロプランの値段。
    pub pro_price_label: String,
    /// 決済のあとに戻る先の URL の基点(`PUBLIC_URL`)。なければリクエストのホストから作る。
    pub public_url: Option<String>,
}

impl Default for BillingConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            free: PlanLimits {
                boards: 3,
                ai_per_month: 20,
                members_per_board: 5,
                media_mb_per_board: 100,
            },
            pro: PlanLimits {
                boards: 0,
                ai_per_month: 1000,
                members_per_board: 0,
                media_mb_per_board: 2048,
            },
            pro_price_label: "¥1,200 / 月".into(),
            public_url: None,
        }
    }
}

impl BillingConfig {
    pub fn enabled(&self) -> bool {
        !matches!(self.mode, Mode::Off)
    }

    pub fn limits(&self, plan: PlanId) -> PlanLimits {
        match plan {
            PlanId::Free => self.free,
            PlanId::Pro => self.pro,
        }
    }

    pub fn offers(&self) -> Vec<PlanOffer> {
        vec![
            PlanOffer {
                id: PlanId::Free,
                name: "無料".into(),
                price: "¥0".into(),
                limits: self.free,
            },
            PlanOffer {
                id: PlanId::Pro,
                name: "プロ".into(),
                price: self.pro_price_label.clone(),
                limits: self.pro,
            },
        ]
    }

    /// 環境変数から読む。
    ///
    /// - `STRIPE_SECRET_KEY` / `STRIPE_WEBHOOK_SECRET` / `STRIPE_PRICE_PRO`: そろえば Stripe で支払う。
    /// - `STRIPE_API_BASE`: Stripe の API の基点(テスト用)。
    /// - `BILLING`: `mock` でテスト決済、`off` で(キーがあっても)使わない。
    /// - `PRO_PRICE_LABEL`: 料金画面の値段の表示(既定 `¥1,200 / 月`)。
    /// - 上限: `FREE_BOARDS` `FREE_AI_PER_MONTH` `FREE_MEMBERS_PER_BOARD` `FREE_MEDIA_MB_PER_BOARD`
    ///   と、同じ名前の `PRO_...`。`0` は無制限。
    pub fn from_env() -> Self {
        Self::from_vars(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let d = Self::default();
        let num = |name: &str, default: u32| -> u32 {
            var(name)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(default)
        };
        let limits = |prefix: &str, base: PlanLimits| PlanLimits {
            boards: num(&format!("{prefix}_BOARDS"), base.boards),
            ai_per_month: num(&format!("{prefix}_AI_PER_MONTH"), base.ai_per_month),
            members_per_board: num(
                &format!("{prefix}_MEMBERS_PER_BOARD"),
                base.members_per_board,
            ),
            media_mb_per_board: num(
                &format!("{prefix}_MEDIA_MB_PER_BOARD"),
                base.media_mb_per_board,
            ),
        };
        let stripe = match (
            var("STRIPE_SECRET_KEY"),
            var("STRIPE_WEBHOOK_SECRET"),
            var("STRIPE_PRICE_PRO"),
        ) {
            (Some(secret_key), Some(webhook_secret), Some(price_pro)) => Some(StripeConfig {
                secret_key: secret_key.trim().into(),
                webhook_secret: webhook_secret.trim().into(),
                price_pro: price_pro.trim().into(),
                api_base: var("STRIPE_API_BASE")
                    .map(|u| u.trim().trim_end_matches('/').to_string())
                    .unwrap_or_else(|| "https://api.stripe.com".into()),
            }),
            (None, None, None) => None,
            _ => {
                tracing::warn!(
                    "Stripe を使うには STRIPE_SECRET_KEY・STRIPE_WEBHOOK_SECRET・STRIPE_PRICE_PRO をそろえてください"
                );
                None
            }
        };
        let mode = match var("BILLING").as_deref().map(str::trim) {
            Some("off") | Some("0") => Mode::Off,
            Some("mock") => Mode::Mock,
            _ => stripe.map_or(Mode::Off, Mode::Stripe),
        };
        Self {
            mode,
            free: limits("FREE", d.free),
            pro: limits("PRO", d.pro),
            pro_price_label: var("PRO_PRICE_LABEL").unwrap_or(d.pro_price_label),
            public_url: var("PUBLIC_URL").map(|u| u.trim().trim_end_matches('/').to_string()),
        }
    }
}

// ---- 保存するもの ----

/// 1 人分の契約。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BillingRecord {
    pub plan: PlanId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Stripe の顧客 ID(`cus_...`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer: Option<String>,
    /// Stripe のサブスクリプション ID(`sub_...`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_end: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancel_at_period_end: bool,
    #[serde(default)]
    pub updated: u64,
}

pub fn record_of(store: &Store, login: &str) -> Result<BillingRecord, StoreError> {
    Ok(store.get_json(BILLING, login)?.unwrap_or_default())
}

fn put_record(store: &Store, login: &str, rec: &BillingRecord) -> Result<(), StoreError> {
    store.put_json(BILLING, login, rec)?;
    if let Some(c) = &rec.customer {
        store.put_json(BILLING_CUSTOMERS, c, &login)?;
    }
    Ok(())
}

fn login_of_customer(store: &Store, customer: &str) -> Result<Option<String>, StoreError> {
    store.get_json(BILLING_CUSTOMERS, customer)
}

/// 有料プランとして扱うサブスクリプションの状態(支払いが遅れている間も、すぐには止めない)。
pub fn status_is_paid(status: &str) -> bool {
    matches!(status, "active" | "trialing" | "past_due")
}

/// UNIX ミリ秒の「年-月」(UTC)。
pub fn month_of(ms: u64) -> String {
    // Howard Hinnant の civil_from_days。
    let days = (ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}")
}

fn usage_key(login: &str, month: &str) -> String {
    format!("{login}\n{month}")
}

pub fn ai_used(store: &Store, login: &str, month: &str) -> Result<u32, StoreError> {
    Ok(store
        .get_json::<u64>(USAGE, &usage_key(login, month))?
        .unwrap_or(0) as u32)
}

// ---- 上限の確かめ ----

fn payment_required(msg: String) -> Error {
    Error::new(StatusCode::PAYMENT_REQUIRED, msg)
}

const UPGRADE_HINT: &str = "ホーム画面の「プラン」からプロにすると増やせます";

fn plan_name(plan: PlanId) -> &'static str {
    match plan {
        PlanId::Free => "無料プラン",
        PlanId::Pro => "プロプラン",
    }
}

fn hint(plan: PlanId) -> &'static str {
    match plan {
        PlanId::Free => UPGRADE_HINT,
        PlanId::Pro => "使わないものを整理してください",
    }
}

/// この人のプランと上限。料金プランを使わないサーバーなら `None`(上限なし)。
pub fn plan_of(state: &AppState, login: &str) -> Result<Option<(PlanId, PlanLimits)>, Error> {
    let cfg = &state.config.billing;
    if !cfg.enabled() {
        return Ok(None);
    }
    let plan = record_of(state.store(), login)?.plan;
    Ok(Some((plan, cfg.limits(plan))))
}

/// ボードを新しく作れるか。
pub fn check_new_board(state: &AppState, login: &str) -> ApiResult<()> {
    let Some((plan, limits)) = plan_of(state, login)? else {
        return Ok(());
    };
    let max = limits.boards as usize;
    if max > 0 && boards::owned_count(state.store(), login)? >= max {
        return Err(payment_required(format!(
            "{}で持てるボードは {max} 個までです。{}",
            plan_name(plan),
            hint(plan)
        )));
    }
    Ok(())
}

/// ボードにメンバーを 1 人増やせるか(`current` は今のメンバー数)。
pub fn check_new_member(state: &AppState, owner: Option<&str>, current: usize) -> ApiResult<()> {
    let Some(owner) = owner else { return Ok(()) };
    let Some((plan, limits)) = plan_of(state, owner)? else {
        return Ok(());
    };
    let max = limits.members_per_board as usize;
    if max > 0 && current >= max {
        return Err(payment_required(format!(
            "{}で 1 つのボードに招待できるのは {max} 人までです。{}",
            plan_name(plan),
            hint(plan)
        )));
    }
    Ok(())
}

/// ボードの動画メモに `adding` バイト足せるか。
pub fn check_media(state: &AppState, board: &str, adding: usize) -> ApiResult<()> {
    let cfg = &state.config.billing;
    if !cfg.enabled() {
        return Ok(());
    }
    let owner = boards::get(state.store(), board)?.and_then(|m| m.owner);
    let plan = match &owner {
        Some(o) => record_of(state.store(), o)?.plan,
        None => PlanId::Free,
    };
    let max_mb = cfg.limits(plan).media_mb_per_board as usize;
    if max_mb == 0 {
        return Ok(());
    }
    let used = crate::media::total_bytes(state.store(), board)?;
    if used + adding > max_mb * 1024 * 1024 {
        return Err(payment_required(format!(
            "{}で 1 つのボードに置ける動画メモは合計 {max_mb} MB までです。{}",
            plan_name(plan),
            hint(plan)
        )));
    }
    Ok(())
}

/// AI 機能を 1 回使う(今月の回数を数える)。数えるのはボードのオーナー、
/// オーナーのいないボードならログイン中の本人。どちらもいなければログインを求める。
pub fn charge_ai(state: &AppState, headers: &HeaderMap, room: &str) -> ApiResult<()> {
    let cfg = &state.config.billing;
    if !cfg.enabled() {
        return Ok(());
    }
    let owner = boards::get(state.store(), room)?.and_then(|m| m.owner);
    let payer = match owner {
        Some(o) => o,
        None => match api::current_user(state, headers)? {
            Some(u) => u.login,
            None => {
                return Err(Error::new(
                    StatusCode::UNAUTHORIZED,
                    "このボードで AI 機能を使うにはログインしてください",
                ));
            }
        },
    };
    let plan = record_of(state.store(), &payer)?.plan;
    let max = cfg.limits(plan).ai_per_month;
    let key = usage_key(&payer, &month_of(now_ms()));
    if state.store().increment(USAGE, &key, max.into())?.is_none() {
        return Err(payment_required(format!(
            "{}の AI 機能は 1 か月 {max} 回までです(今月の分を使い切りました)。{}",
            plan_name(plan),
            hint(plan)
        )));
    }
    Ok(())
}

// ---- 契約の変更(Stripe の通知とテスト決済の両方から) ----

/// サブスクリプションの今の状態。
#[derive(Clone, Debug, Default)]
pub struct Subscription {
    pub id: Option<String>,
    pub customer: Option<String>,
    pub status: String,
    pub period_end: Option<u64>,
    pub cancel_at_period_end: bool,
}

/// サブスクリプションの状態をこの人の契約に写す。プランが変わったら `Some(新しいプラン)`。
pub fn apply_subscription(
    store: &Store,
    login: &str,
    sub: &Subscription,
) -> Result<Option<PlanId>, StoreError> {
    let mut rec = record_of(store, login)?;
    // 別のサブスクリプションの古い通知で、今の契約を消さない。
    if let (Some(current), Some(id)) = (&rec.subscription, &sub.id)
        && current != id
        && rec.plan == PlanId::Pro
        && !status_is_paid(&sub.status)
    {
        return Ok(None);
    }
    let before = rec.plan;
    rec.plan = if status_is_paid(&sub.status) {
        PlanId::Pro
    } else {
        PlanId::Free
    };
    rec.status = Some(sub.status.clone());
    if sub.customer.is_some() {
        rec.customer = sub.customer.clone();
    }
    if sub.id.is_some() {
        rec.subscription = sub.id.clone();
    }
    rec.period_end = sub.period_end;
    rec.cancel_at_period_end = sub.cancel_at_period_end;
    rec.updated = now_ms();
    put_record(store, login, &rec)?;
    Ok((before != rec.plan).then_some(rec.plan))
}

fn audit_plan(state: &AppState, headers: &HeaderMap, login: &str, plan: PlanId) {
    audit::record(
        state,
        headers,
        Entry::new("billing.plan")
            .actor(login)
            .target(login)
            .detail(plan.as_str()),
    );
}

// ---- Stripe ----

/// `Stripe-Signature: t=...,v1=...` を確かめる。`now` は UNIX 秒。
pub fn verify_signature(payload: &[u8], header: &str, secret: &str, now: u64) -> bool {
    let mut timestamp = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => timestamp = v.parse::<u64>().ok(),
            Some(("v1", v)) => sigs.push(v.to_ascii_lowercase()),
            _ => {}
        }
    }
    let Some(t) = timestamp else { return false };
    if t.abs_diff(now) > SIGNATURE_TOLERANCE_SECS {
        return false;
    }
    let mut msg = format!("{t}.").into_bytes();
    msg.extend_from_slice(payload);
    let expected = hex(&hmac_sha256(secret.as_bytes(), &msg));
    sigs.iter()
        .any(|s| constant_time_eq(s.as_bytes(), expected.as_bytes()))
}

/// テストや偽の通知を作るため、`verify_signature` が受け付ける署名を作る。
pub fn sign(payload: &[u8], secret: &str, t: u64) -> String {
    let mut msg = format!("{t}.").into_bytes();
    msg.extend_from_slice(payload);
    format!("t={t},v1={}", hex(&hmac_sha256(secret.as_bytes(), &msg)))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `application/x-www-form-urlencoded` の本文。
fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("HTTP クライアントを作れません")
    })
}

async fn stripe_post(cfg: &StripeConfig, path: &str, pairs: &[(&str, &str)]) -> ApiResult<Value> {
    let res = http_client()
        .post(format!("{}{path}", cfg.api_base))
        .bearer_auth(&cfg.secret_key)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(form_body(pairs))
        .send()
        .await
        .map_err(|e| {
            tracing::error!("Stripe につながりません: {e}");
            Error::new(StatusCode::BAD_GATEWAY, "決済サービスにつながりません")
        })?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = body["error"]["message"].as_str().unwrap_or("不明なエラー");
        tracing::error!(%status, "Stripe がエラーを返しました: {msg}");
        return Err(Error::new(
            StatusCode::BAD_GATEWAY,
            "決済サービスでエラーが起きました。しばらくしてからもう一度お試しください",
        ));
    }
    Ok(body)
}

fn subscription_from(obj: &Value) -> Subscription {
    // 新しい API バージョンでは期間の終わりが items の中にある。
    let period_end = obj["current_period_end"]
        .as_u64()
        .or_else(|| obj["items"]["data"][0]["current_period_end"].as_u64())
        .map(|s| s * 1000);
    Subscription {
        id: obj["id"].as_str().map(String::from),
        customer: obj["customer"].as_str().map(String::from),
        status: obj["status"].as_str().unwrap_or("canceled").to_string(),
        period_end,
        cancel_at_period_end: obj["cancel_at_period_end"].as_bool().unwrap_or(false),
    }
}

/// Stripe のイベントを処理する。対象のログイン名とプランの変化を返す。
pub fn handle_event(
    store: &Store,
    event: &Value,
) -> Result<Option<(String, Option<PlanId>)>, StoreError> {
    let obj = &event["data"]["object"];
    match event["type"].as_str().unwrap_or_default() {
        "checkout.session.completed" => {
            if obj["mode"].as_str() != Some("subscription") {
                return Ok(None);
            }
            let Some(login) = obj["client_reference_id"]
                .as_str()
                .or_else(|| obj["metadata"]["login"].as_str())
            else {
                return Ok(None);
            };
            // 状態の詳細は customer.subscription.* で届く。ここでは結びつけと有効化だけ。
            let sub = Subscription {
                id: obj["subscription"].as_str().map(String::from),
                customer: obj["customer"].as_str().map(String::from),
                status: "active".into(),
                ..Default::default()
            };
            let mut rec = record_of(store, login)?;
            if rec.subscription.is_some() && rec.subscription == sub.id && rec.status.is_some() {
                // サブスクリプションの通知が先に届いていれば、そちらを正とする。
                rec.customer = sub.customer.clone().or(rec.customer);
                put_record(store, login, &rec)?;
                return Ok(Some((login.to_string(), None)));
            }
            let changed = apply_subscription(store, login, &sub)?;
            Ok(Some((login.to_string(), changed)))
        }
        "customer.subscription.created"
        | "customer.subscription.updated"
        | "customer.subscription.deleted" => {
            let mut sub = subscription_from(obj);
            if event["type"] == "customer.subscription.deleted" {
                sub.status = "canceled".into();
            }
            let login = match obj["metadata"]["login"].as_str() {
                Some(l) => Some(l.to_string()),
                None => match &sub.customer {
                    Some(c) => login_of_customer(store, c)?,
                    None => None,
                },
            };
            let Some(login) = login else {
                tracing::warn!("誰のものかわからないサブスクリプションの通知を無視しました");
                return Ok(None);
            };
            let changed = apply_subscription(store, &login, &sub)?;
            Ok(Some((login, changed)))
        }
        _ => Ok(None),
    }
}

// ---- テスト決済 ----

#[derive(Default)]
pub struct MockSessions {
    sessions: Mutex<HashMap<String, (String, u64)>>,
}

impl MockSessions {
    fn create(&self, login: &str) -> String {
        let id = format!("cs_mock_{}", random_hex(16));
        let now = now_ms();
        let mut map = self.sessions.lock().unwrap();
        map.retain(|_, (_, t)| now < *t + MOCK_SESSION_TTL_MS);
        map.insert(id.clone(), (login.to_string(), now));
        id
    }

    fn login(&self, id: &str) -> Option<String> {
        let map = self.sessions.lock().unwrap();
        map.get(id)
            .filter(|(_, t)| now_ms() < *t + MOCK_SESSION_TTL_MS)
            .map(|(l, _)| l.clone())
    }

    fn take(&self, id: &str) -> Option<String> {
        let login = self.login(id)?;
        self.sessions.lock().unwrap().remove(id);
        Some(login)
    }
}

// ---- HTTP ----

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/billing", get(status))
        .route("/billing/checkout", post(checkout))
        .route("/billing/portal", post(portal))
        .route("/billing/webhook", post(webhook))
        .route(
            "/billing/mock/checkout/{sid}",
            get(mock_checkout_page).post(mock_checkout_complete),
        )
        .route("/billing/mock/portal", get(mock_portal_page))
        .route("/billing/mock/cancel", post(mock_cancel))
}

fn base_url(state: &AppState, headers: &HeaderMap) -> String {
    state
        .config
        .billing
        .public_url
        .clone()
        .unwrap_or_else(|| crate::public::origin(state, headers))
}

fn disabled() -> Error {
    Error::new(
        StatusCode::NOT_FOUND,
        "このサーバーでは料金プランを使っていません",
    )
}

async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Billing>> {
    let user = api::require_user(&state, &headers)?;
    let cfg = &state.config.billing;
    let store = state.store();
    let rec = if cfg.enabled() {
        record_of(store, &user.login)?
    } else {
        BillingRecord::default()
    };
    let usage = Usage {
        boards: boards::owned_count(store, &user.login)? as u32,
        ai_this_month: ai_used(store, &user.login, &month_of(now_ms()))?,
    };
    let can_manage = match &cfg.mode {
        Mode::Stripe(_) => rec.customer.is_some(),
        Mode::Mock => rec.plan == PlanId::Pro,
        Mode::Off => false,
    };
    Ok(Json(Billing {
        mode: cfg.mode.name().into(),
        plan: rec.plan,
        status: rec.status,
        period_end: rec.period_end,
        cancel_at_period_end: rec.cancel_at_period_end,
        can_manage,
        limits: if cfg.enabled() {
            cfg.limits(rec.plan)
        } else {
            PlanLimits::default()
        },
        usage,
        plans: if cfg.enabled() {
            cfg.offers()
        } else {
            Vec::new()
        },
    }))
}

async fn checkout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<BillingRedirect>> {
    let user = api::require_user(&state, &headers)?;
    let rec = record_of(state.store(), &user.login)?;
    if rec.plan == PlanId::Pro {
        return Err(Error::bad_request("すでにプロプランです"));
    }
    let base = base_url(&state, &headers);
    let url = match &state.config.billing.mode {
        Mode::Off => return Err(disabled()),
        Mode::Mock => {
            let sid = state.billing_mock.create(&user.login);
            format!("{base}/api/billing/mock/checkout/{sid}")
        }
        Mode::Stripe(cfg) => {
            let success = format!("{base}/?billing=success");
            let cancel = format!("{base}/?billing=cancel");
            let mut pairs = vec![
                ("mode", "subscription"),
                ("line_items[0][price]", cfg.price_pro.as_str()),
                ("line_items[0][quantity]", "1"),
                ("success_url", success.as_str()),
                ("cancel_url", cancel.as_str()),
                ("client_reference_id", user.login.as_str()),
                ("metadata[login]", user.login.as_str()),
                ("subscription_data[metadata][login]", user.login.as_str()),
                ("allow_promotion_codes", "true"),
            ];
            if let Some(c) = &rec.customer {
                pairs.push(("customer", c.as_str()));
            }
            let session = stripe_post(cfg, "/v1/checkout/sessions", &pairs).await?;
            session["url"]
                .as_str()
                .ok_or_else(|| {
                    Error::new(StatusCode::BAD_GATEWAY, "決済サービスの応答が読めません")
                })?
                .to_string()
        }
    };
    tracing::info!(login = %user.login, "有料プランの申し込みを開始");
    Ok(Json(BillingRedirect { url }))
}

async fn portal(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<BillingRedirect>> {
    let user = api::require_user(&state, &headers)?;
    let rec = record_of(state.store(), &user.login)?;
    let base = base_url(&state, &headers);
    let url = match &state.config.billing.mode {
        Mode::Off => return Err(disabled()),
        Mode::Mock => {
            if rec.plan != PlanId::Pro {
                return Err(Error::bad_request("有料プランを契約していません"));
            }
            format!("{base}/api/billing/mock/portal")
        }
        Mode::Stripe(cfg) => {
            let customer = rec
                .customer
                .ok_or_else(|| Error::bad_request("まだ支払いの記録がありません"))?;
            let back = format!("{base}/");
            let session = stripe_post(
                cfg,
                "/v1/billing_portal/sessions",
                &[
                    ("customer", customer.as_str()),
                    ("return_url", back.as_str()),
                ],
            )
            .await?;
            session["url"]
                .as_str()
                .ok_or_else(|| {
                    Error::new(StatusCode::BAD_GATEWAY, "決済サービスの応答が読めません")
                })?
                .to_string()
        }
    };
    Ok(Json(BillingRedirect { url }))
}

async fn webhook(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let Mode::Stripe(cfg) = &state.config.billing.mode else {
        return disabled().into_response();
    };
    let sig = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !verify_signature(&body, sig, &cfg.webhook_secret, now_ms() / 1000) {
        tracing::warn!("署名の合わない Stripe の通知を断りました");
        return (StatusCode::BAD_REQUEST, "invalid signature").into_response();
    }
    let Ok(event) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid json").into_response();
    };
    let store = state.store();
    // 同じ通知が何度か届くことがあるので、処理済みなら何もしない。
    if let Some(id) = event["id"].as_str() {
        match store.insert_new_json(BILLING_EVENTS, id, &now_ms()) {
            Ok(true) => {}
            Ok(false) => return StatusCode::OK.into_response(),
            Err(e) => return Error::from(e).into_response(),
        }
    }
    match handle_event(store, &event) {
        Ok(Some((login, changed))) => {
            tracing::info!(login = %login, kind = event["type"].as_str().unwrap_or(""), "Stripe の通知を処理");
            if let Some(plan) = changed {
                audit_plan(&state, &headers, &login, plan);
            }
            StatusCode::OK.into_response()
        }
        Ok(None) => StatusCode::OK.into_response(),
        Err(e) => {
            // 失敗を返せば Stripe が送り直すので、処理済みの印を外す。
            if let Some(id) = event["id"].as_str() {
                let _ = store.remove(BILLING_EVENTS, id);
            }
            Error::from(e).into_response()
        }
    }
}

fn is_mock(state: &AppState) -> bool {
    matches!(state.config.billing.mode, Mode::Mock)
}

fn mock_page(title: &str, inner: &str) -> Response {
    let body = format!(
        r#"<div class="body"><p class="badge">テストモード — 実際の請求は発生しません</p>{inner}</div>"#
    );
    let head = r#"<style>
.badge { display:inline-block; background:#fff4d6; color:#7a5200; border-radius:999px; padding:4px 12px; font-size:13px; }
.price { font-size:28px; font-weight:700; color:var(--text); margin:8px 0 4px; }
ul { color:var(--muted); line-height:1.8; padding-left:20px; margin:0 0 20px; }
form { display:flex; gap:12px; align-items:center; flex-wrap:wrap; }
button { font:inherit; border:0; background:var(--accent); color:#fff; font-weight:600; padding:10px 18px; border-radius:999px; cursor:pointer; }
button.danger { background:#d4394a; }
a.quiet { color:var(--muted); }
</style>
"#;
    let mut res = Html(crate::public::page_shell(title, &body, head)).into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    res
}

fn limit_text(n: u32, unit: &str) -> String {
    if n == 0 {
        "無制限".into()
    } else {
        format!("{n} {unit}")
    }
}

fn limits_list(l: &PlanLimits) -> String {
    format!(
        "<ul><li>ボード: {}</li><li>AI 機能: {}</li><li>ボードのメンバー: {}</li><li>動画メモ: {}</li></ul>",
        limit_text(l.boards, "個"),
        if l.ai_per_month == 0 {
            "無制限".into()
        } else {
            format!("月 {} 回", l.ai_per_month)
        },
        limit_text(l.members_per_board, "人 / ボード"),
        limit_text(l.media_mb_per_board, "MB / ボード"),
    )
}

async fn mock_checkout_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(sid): Path<String>,
) -> Response {
    if !is_mock(&state) {
        return disabled().into_response();
    }
    let user = match api::require_user(&state, &headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if state.billing_mock.login(&sid).as_deref() != Some(user.login.as_str()) {
        return mock_page(
            "申し込みの期限切れ",
            r#"<h1>申し込みの期限が切れました</h1><p>ホーム画面の「プラン」からもう一度お試しください。</p><a class="quiet" href="/">ホームに戻る</a>"#,
        );
    }
    let cfg = &state.config.billing;
    let inner = format!(
        r#"<h1>プロプランに申し込む</h1>
<p>@{login} さんのアカウントをプロプランにします。</p>
<p class="price">{price}</p>
{limits}
<form method="post" action="/api/billing/mock/checkout/{sid}">
<button type="submit">テスト決済を完了する</button>
<a class="quiet" href="/?billing=cancel">やめる</a>
</form>"#,
        login = crate::public::escape(&user.login),
        price = crate::public::escape(&cfg.pro_price_label),
        limits = limits_list(&cfg.pro),
        sid = crate::public::escape(&sid),
    );
    mock_page("テスト決済", &inner)
}

async fn mock_checkout_complete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(sid): Path<String>,
) -> Response {
    if !is_mock(&state) {
        return disabled().into_response();
    }
    let user = match api::require_user(&state, &headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if state.billing_mock.login(&sid).as_deref() != Some(user.login.as_str()) {
        return Redirect::to("/?billing=cancel").into_response();
    }
    state.billing_mock.take(&sid);
    let sub = Subscription {
        id: Some(format!("sub_mock_{}", random_hex(8))),
        customer: Some(format!("cus_mock_{}", user.login)),
        status: "active".into(),
        period_end: Some(now_ms() + 30 * 86_400_000),
        cancel_at_period_end: false,
    };
    match apply_subscription(state.store(), &user.login, &sub) {
        Ok(changed) => {
            if let Some(plan) = changed {
                audit_plan(&state, &headers, &user.login, plan);
            }
            tracing::info!(login = %user.login, "テスト決済でプロプランにしました");
            Redirect::to("/?billing=success").into_response()
        }
        Err(e) => Error::from(e).into_response(),
    }
}

async fn mock_portal_page(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !is_mock(&state) {
        return disabled().into_response();
    }
    let user = match api::require_user(&state, &headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let rec = match record_of(state.store(), &user.login) {
        Ok(r) => r,
        Err(e) => return Error::from(e).into_response(),
    };
    let inner = if rec.plan == PlanId::Pro {
        format!(
            r#"<h1>お支払いの管理</h1>
<p>@{login} さんは今プロプランです。解約すると、すぐに無料プランに戻ります(ボードやメンバーはそのまま残ります)。</p>
<form method="post" action="/api/billing/mock/cancel">
<button type="submit" class="danger">解約して無料プランに戻す</button>
<a class="quiet" href="/">ホームに戻る</a>
</form>"#,
            login = crate::public::escape(&user.login),
        )
    } else {
        r#"<h1>お支払いの管理</h1><p>有料プランを契約していません。</p><a class="quiet" href="/">ホームに戻る</a>"#.into()
    };
    mock_page("お支払いの管理", &inner)
}

async fn mock_cancel(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !is_mock(&state) {
        return disabled().into_response();
    }
    let user = match api::require_user(&state, &headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let rec = match record_of(state.store(), &user.login) {
        Ok(r) => r,
        Err(e) => return Error::from(e).into_response(),
    };
    let sub = Subscription {
        id: rec.subscription.clone(),
        customer: rec.customer.clone(),
        status: "canceled".into(),
        period_end: None,
        cancel_at_period_end: false,
    };
    match apply_subscription(state.store(), &user.login, &sub) {
        Ok(changed) => {
            if let Some(plan) = changed {
                audit_plan(&state, &headers, &user.login, plan);
            }
            Redirect::to("/?billing=canceled").into_response()
        }
        Err(e) => Error::from(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months() {
        assert_eq!(month_of(0), "1970-01");
        // 2026-11-01T00:00:00Z
        assert_eq!(month_of(1_793_491_200_000), "2026-11");
        // 2024-02-29T23:59:59Z
        assert_eq!(month_of(1_709_251_199_000), "2024-02");
        assert_eq!(month_of(1_709_251_200_000), "2024-03");
    }

    #[test]
    fn signatures() {
        let body = br#"{"id":"evt_1"}"#;
        let header = sign(body, "whsec_test", 1_000_000);
        assert!(verify_signature(body, &header, "whsec_test", 1_000_000));
        assert!(verify_signature(body, &header, "whsec_test", 1_000_299));
        // 時刻が古すぎる・鍵が違う・本文が違う。
        assert!(!verify_signature(body, &header, "whsec_test", 1_000_301));
        assert!(!verify_signature(body, &header, "whsec_other", 1_000_000));
        assert!(!verify_signature(b"{}", &header, "whsec_test", 1_000_000));
        assert!(!verify_signature(body, "v1=abc", "whsec_test", 1_000_000));
        // 鍵の入れ替え中は v1 が複数並ぶ。
        let rotated = format!("{header},v1=00");
        assert!(verify_signature(body, &rotated, "whsec_test", 1_000_000));
    }

    #[test]
    fn form_encoding() {
        assert_eq!(
            form_body(&[
                ("line_items[0][price]", "price_1"),
                ("success_url", "https://x/?a=b")
            ]),
            "line_items%5B0%5D%5Bprice%5D=price_1&success_url=https%3A%2F%2Fx%2F%3Fa%3Db"
        );
    }

    #[test]
    fn config_from_vars() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(matches!(
            BillingConfig::from_vars(vars(&[])).mode,
            Mode::Off
        ));
        assert!(matches!(
            BillingConfig::from_vars(vars(&[("BILLING", "mock")])).mode,
            Mode::Mock
        ));
        // 3 つそろわなければ使わない。
        assert!(matches!(
            BillingConfig::from_vars(vars(&[("STRIPE_SECRET_KEY", "sk_test_x")])).mode,
            Mode::Off
        ));
        let c = BillingConfig::from_vars(vars(&[
            ("STRIPE_SECRET_KEY", "sk_test_x"),
            ("STRIPE_WEBHOOK_SECRET", "whsec_x"),
            ("STRIPE_PRICE_PRO", "price_x"),
            ("FREE_BOARDS", "10"),
            ("PRO_AI_PER_MONTH", "0"),
        ]));
        let Mode::Stripe(s) = &c.mode else {
            panic!("stripe のはず")
        };
        assert_eq!(s.api_base, "https://api.stripe.com");
        assert_eq!(c.free.boards, 10);
        assert_eq!(c.free.ai_per_month, 20);
        assert_eq!(c.pro.ai_per_month, 0);
    }

    #[test]
    fn subscription_events() {
        let store = Store::memory();
        let completed = serde_json::json!({
            "id": "evt_1", "type": "checkout.session.completed",
            "data": { "object": {
                "mode": "subscription", "client_reference_id": "alice",
                "customer": "cus_1", "subscription": "sub_1",
            }},
        });
        assert_eq!(
            handle_event(&store, &completed).unwrap(),
            Some(("alice".into(), Some(PlanId::Pro)))
        );
        let rec = record_of(&store, "alice").unwrap();
        assert_eq!(rec.customer.as_deref(), Some("cus_1"));

        // メタデータがなくても顧客 ID から引ける。解約予定と期間の終わり(新しい形)を写す。
        let updated = serde_json::json!({
            "id": "evt_2", "type": "customer.subscription.updated",
            "data": { "object": {
                "id": "sub_1", "customer": "cus_1", "status": "active",
                "cancel_at_period_end": true,
                "items": { "data": [{ "current_period_end": 1_800_000_000u64 }] },
            }},
        });
        assert_eq!(
            handle_event(&store, &updated).unwrap(),
            Some(("alice".into(), None))
        );
        let rec = record_of(&store, "alice").unwrap();
        assert!(rec.cancel_at_period_end);
        assert_eq!(rec.period_end, Some(1_800_000_000_000));

        // 古い別のサブスクリプションの解約では落とさない。
        let other = serde_json::json!({
            "id": "evt_3", "type": "customer.subscription.deleted",
            "data": { "object": { "id": "sub_old", "customer": "cus_1", "status": "canceled" }},
        });
        handle_event(&store, &other).unwrap();
        assert_eq!(record_of(&store, "alice").unwrap().plan, PlanId::Pro);

        let deleted = serde_json::json!({
            "id": "evt_4", "type": "customer.subscription.deleted",
            "data": { "object": { "id": "sub_1", "customer": "cus_1", "status": "canceled" }},
        });
        assert_eq!(
            handle_event(&store, &deleted).unwrap(),
            Some(("alice".into(), Some(PlanId::Free)))
        );
        // 知らない顧客は無視する。
        let unknown = serde_json::json!({
            "id": "evt_5", "type": "customer.subscription.updated",
            "data": { "object": { "id": "sub_9", "customer": "cus_9", "status": "active" }},
        });
        assert_eq!(handle_event(&store, &unknown).unwrap(), None);
    }

    #[test]
    fn usage_counter() {
        let store = Store::memory();
        assert_eq!(store.increment(USAGE, "k", 2).unwrap(), Some(1));
        assert_eq!(store.increment(USAGE, "k", 2).unwrap(), Some(2));
        assert_eq!(store.increment(USAGE, "k", 2).unwrap(), None);
        assert_eq!(store.increment(USAGE, "k", 0).unwrap(), Some(3));
    }
}
