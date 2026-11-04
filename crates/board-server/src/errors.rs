//! エラー監視。
//!
//! - サーバー: ログの ERROR とパニックを [`ErrorLayer`] で拾う。
//! - ブラウザ: クライアントが `POST /api/errors` で知らせる(JavaScript のエラー・wasm のパニック)。
//!
//! 同じメッセージ・場所のものは 1 つにまとめ([`ErrorGroup`])、回数と最後の 1 件の詳細を
//! redb の `errors` テーブルに残す。サーバー管理者は運用の画面で一覧を見て、解決済みにできる。
//!
//! 外への知らせ(どれも任意):
//! - `SENTRY_DSN`: 起きたエラーを Sentry(または互換のサービス)へ送る。ブラウザのエラーもサーバー経由で送る。
//! - `ERROR_SLACK_WEBHOOK`: 新しい種類のエラー(解決済みのものの再発を含む)を Slack に知らせる。
//! - `LOG_FORMAT=gcp`: ログを Cloud Logging の構造化ログ(JSON)で出す。ERROR は Error Reporting にも載る。
//!   `LOG_FORMAT=json` は一般的な JSON(Datadog・Loki など)。

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{patch, post};
use board_core::api::ops::{
    ClientErrorReport, ErrorGroup, ErrorSample, ErrorSource, ErrorStatus, SetErrorResolved,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::AppState;
use crate::api::{Error, require_user};
use crate::auth::{UserRecord, now_ms};
use crate::guard::{ClientIp, Window};
use crate::store::{ERRORS, Store, StoreError};

/// 残すまとまりの数の上限。超えたら古いものから消す。
pub const MAX_GROUPS: usize = 1000;
/// ブラウザから受け取る 1 件の大きさの上限。
pub const MAX_REPORT_BYTES: usize = 16 * 1024;
const MAX_MESSAGE_CHARS: usize = 1000;
const MAX_STACK_CHARS: usize = 8000;
const MAX_FIELD_CHARS: usize = 500;
/// 1 つの IP から受け取るブラウザのエラーの上限(1 分あたり)。
const CLIENT_REPORTS_PER_MINUTE: u32 = 30;
/// 保存の間隔。
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// Sentry へ送る上限(1 分あたり、全体で)。
const SENTRY_PER_MINUTE: u32 = 60;
/// Slack へ知らせる上限(1 時間あたり)。
const SLACK_PER_HOUR: u32 = 20;

/// サーバーの版(Sentry の release などに使う)。
pub const RELEASE: &str = env!("CARGO_PKG_VERSION");

// ---- 設定 ----

/// ログの形式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// 人が読む形式(既定)。
    Text,
    /// 1 行 1 件の JSON。
    Json,
    /// Cloud Logging の構造化ログ(`severity`・`sourceLocation`、ERROR は Error Reporting の形)。
    Gcp,
}

impl LogFormat {
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("json") => LogFormat::Json,
            Some("gcp") | Some("google") | Some("stackdriver") => LogFormat::Gcp,
            _ => LogFormat::Text,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LogFormat::Text => "text",
            LogFormat::Json => "json",
            LogFormat::Gcp => "gcp",
        }
    }
}

/// Sentry の DSN(`https://公開鍵@ホスト/プロジェクト ID`)を分けたもの。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentryDsn {
    pub envelope_url: String,
    pub public_key: String,
    pub raw: String,
}

impl SentryDsn {
    pub fn parse(dsn: &str) -> Option<Self> {
        let dsn = dsn.trim();
        let (scheme, rest) = dsn.split_once("://")?;
        if scheme != "https" && scheme != "http" {
            return None;
        }
        let (key, host_path) = rest.split_once('@')?;
        let public_key = key.split(':').next()?.to_string();
        let (host, path) = host_path.split_once('/')?;
        let path = path.trim_end_matches('/');
        let (prefix, project) = match path.rsplit_once('/') {
            Some((p, id)) => (format!("/{p}"), id),
            None => (String::new(), path),
        };
        if public_key.is_empty()
            || host.is_empty()
            || project.is_empty()
            || !project.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            envelope_url: format!("{scheme}://{host}{prefix}/api/{project}/envelope/"),
            public_key,
            raw: dsn.to_string(),
        })
    }
}

/// 外への知らせの設定。
#[derive(Clone, Debug, Default)]
pub struct MonitorConfig {
    pub sentry: Option<SentryDsn>,
    pub slack_webhook: Option<String>,
    /// Sentry の environment(`SENTRY_ENVIRONMENT`)。
    pub environment: Option<String>,
    /// 通知に載せるリンクの土台(`PUBLIC_URL`)。
    pub public_url: Option<String>,
    pub log_format: Option<LogFormat>,
}

impl MonitorConfig {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let sentry = var("SENTRY_DSN").and_then(|d| {
            let parsed = SentryDsn::parse(&d);
            if parsed.is_none() {
                tracing::warn!("SENTRY_DSN の形が違うので使いません(https://鍵@ホスト/番号)");
            }
            parsed
        });
        Self {
            sentry,
            slack_webhook: var("ERROR_SLACK_WEBHOOK").filter(|u| u.starts_with("https://")),
            environment: var("SENTRY_ENVIRONMENT"),
            public_url: var("PUBLIC_URL"),
            log_format: Some(LogFormat::parse(var("LOG_FORMAT").as_deref())),
        }
    }
}

// ---- 集める ----

/// 起きたエラー 1 件。
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub source: Option<ErrorSource>,
    pub message: String,
    pub location: Option<String>,
    pub stack: Option<String>,
    pub url: Option<String>,
    pub user_agent: Option<String>,
    pub login: Option<String>,
    pub board: Option<String>,
    pub release: Option<String>,
}

impl Report {
    fn source(&self) -> ErrorSource {
        self.source.unwrap_or(ErrorSource::Server)
    }
}

/// 外へ送る 1 件。
#[derive(Clone, Debug)]
struct Outgoing {
    group: ErrorGroup,
    /// 新しい種類(または再発)か。
    is_new: bool,
}

#[derive(Default)]
struct Inner {
    groups: HashMap<String, ErrorGroup>,
    dirty: HashSet<String>,
    removed: HashSet<String>,
}

/// エラーを集めて残す。
pub struct Monitor {
    inner: Mutex<Inner>,
    outbox: Mutex<Option<tokio::sync::mpsc::Sender<Outgoing>>>,
    config: OnceLock<MonitorConfig>,
    client_window: Window,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL: LazyLock<Arc<Monitor>> = LazyLock::new(|| Arc::new(Monitor::new()));

/// ログとパニックを拾うプロセス全体の [`Monitor`]。
pub fn global() -> Arc<Monitor> {
    GLOBAL.clone()
}

impl Monitor {
    pub fn new() -> Self {
        Self {
            inner: Mutex::default(),
            outbox: Mutex::new(None),
            config: OnceLock::new(),
            client_window: Window::new(Duration::from_secs(60), CLIENT_REPORTS_PER_MINUTE),
        }
    }

    fn config(&self) -> MonitorConfig {
        self.config.get().cloned().unwrap_or_default()
    }

    /// 1 件取り込む。ロックを短く持つだけで、保存や送信はしない(ログの中から呼ばれるため)。
    pub fn capture(&self, report: Report) {
        let source = report.source();
        let message = truncate(report.message.trim(), MAX_MESSAGE_CHARS);
        if message.is_empty() {
            return;
        }
        let headline = message.lines().next().unwrap_or("").to_string();
        let id = fingerprint(source, report.location.as_deref(), &headline);
        let now = now_ms();
        let sample = ErrorSample {
            at: now,
            message: message.clone(),
            stack: report.stack.map(|s| truncate(&s, MAX_STACK_CHARS)),
            url: report.url.map(|s| truncate(&s, MAX_FIELD_CHARS)),
            user_agent: report.user_agent.map(|s| truncate(&s, MAX_FIELD_CHARS)),
            login: report.login,
            board: report.board.map(|s| truncate(&s, 64)),
            release: report.release.map(|s| truncate(&s, 64)),
        };
        let out = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mut is_new = false;
            let group = inner.groups.entry(id.clone()).or_insert_with(|| {
                is_new = true;
                ErrorGroup {
                    id: id.clone(),
                    source,
                    message: headline.clone(),
                    location: report.location.clone(),
                    first_seen: now,
                    last_seen: now,
                    count: 0,
                    resolved: false,
                    sample: sample.clone(),
                }
            });
            if group.resolved {
                // 解決済みにしたものがまた起きた。
                group.resolved = false;
                is_new = true;
            }
            group.count += 1;
            group.last_seen = now;
            group.sample = sample;
            let snapshot = group.clone();
            inner.removed.remove(&id);
            inner.dirty.insert(id);
            if inner.groups.len() > MAX_GROUPS {
                prune(&mut inner);
            }
            Outgoing {
                group: snapshot,
                is_new,
            }
        };
        if let Some(tx) = self
            .outbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = tx.try_send(out);
        }
    }

    /// 保存してあるものを読み込む(起動時)。それまでに拾ったものはそのまま残す。
    pub fn load(&self, store: &Store) -> Result<(), StoreError> {
        let saved: Vec<ErrorGroup> = store.scan_all_json(ERRORS)?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for g in saved {
            match inner.groups.get_mut(&g.id) {
                Some(cur) => {
                    cur.count += g.count;
                    cur.first_seen = cur.first_seen.min(g.first_seen);
                }
                None => {
                    inner.groups.insert(g.id.clone(), g);
                }
            }
        }
        Ok(())
    }

    /// 変わったものを書き出す。
    pub fn flush(&self, store: &Store) -> Result<(), StoreError> {
        let (changed, removed) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let dirty = std::mem::take(&mut inner.dirty);
            let removed: Vec<String> = inner.removed.drain().collect();
            let changed: Vec<ErrorGroup> = dirty
                .iter()
                .filter_map(|id| inner.groups.get(id).cloned())
                .collect();
            (changed, removed)
        };
        let res = (|| {
            for g in &changed {
                store.put_json(ERRORS, &g.id, g)?;
            }
            for id in &removed {
                store.remove(ERRORS, id)?;
            }
            Ok(())
        })();
        if res.is_err() {
            // 次の回にやり直す。
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.dirty.extend(changed.into_iter().map(|g| g.id));
            inner.removed.extend(removed);
        }
        res
    }

    /// 最後に起きた順。
    pub fn list(&self, include_resolved: bool) -> Vec<ErrorGroup> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<ErrorGroup> = inner
            .groups
            .values()
            .filter(|g| include_resolved || !g.resolved)
            .cloned()
            .collect();
        v.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then(a.id.cmp(&b.id)));
        v
    }

    pub fn open_count(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.groups.values().filter(|g| !g.resolved).count()
    }

    pub fn set_resolved(&self, id: &str, resolved: bool) -> Option<ErrorGroup> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let g = inner.groups.get_mut(id)?;
        g.resolved = resolved;
        let out = g.clone();
        inner.dirty.insert(id.to_string());
        Some(out)
    }

    pub fn remove(&self, id: &str) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.groups.remove(id).is_none() {
            return false;
        }
        inner.dirty.remove(id);
        inner.removed.insert(id.to_string());
        true
    }

    /// 画面に出す設定の状態。
    pub fn status(&self, include_resolved: bool) -> ErrorStatus {
        let c = self.config();
        ErrorStatus {
            groups: self.list(include_resolved),
            open: self.open_count(),
            sentry: c.sentry.is_some(),
            slack: c.slack_webhook.is_some(),
            log_format: c.log_format.unwrap_or(LogFormat::Text).as_str().into(),
        }
    }
}

/// 解決済みと古いものから消して、上限に収める。
fn prune(inner: &mut Inner) {
    let mut order: Vec<(bool, u64, String)> = inner
        .groups
        .values()
        .map(|g| (!g.resolved, g.last_seen, g.id.clone()))
        .collect();
    order.sort();
    let excess = inner.groups.len().saturating_sub(MAX_GROUPS);
    for (_, _, id) in order.into_iter().take(excess) {
        inner.groups.remove(&id);
        inner.dirty.remove(&id);
        inner.removed.insert(id);
    }
}

/// まとめるための ID。数字や長い 16 進(ID・時刻・アドレス)は同じものとみなす。
pub fn fingerprint(source: ErrorSource, location: Option<&str>, message: &str) -> String {
    let mut h = Sha256::new();
    h.update(match source {
        ErrorSource::Server => b"s" as &[u8],
        ErrorSource::Client => b"c",
    });
    h.update(b"\0");
    h.update(location.unwrap_or("").as_bytes());
    h.update(b"\0");
    h.update(normalize(message).as_bytes());
    let d = h.finalize();
    d[..8].iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn normalize(message: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let has_digit = word.bytes().any(|b| b.is_ascii_digit());
        let all_digits = word.bytes().all(|b| b.is_ascii_digit());
        let hexish = word.len() >= 8 && word.bytes().all(|b| b.is_ascii_hexdigit());
        if all_digits || (has_digit && hexish) {
            out.push('#');
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in message.chars().take(300) {
        if c.is_ascii_alphanumeric() {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

/// ブラウザからのエラーを取り込む形にする。
pub fn client_report(
    r: ClientErrorReport,
    login: Option<String>,
    user_agent: Option<String>,
) -> Report {
    // スクリプトの位置は行・列とクエリを外して、版が変わっても同じまとまりにする。
    let location = r.source.as_deref().map(|s| {
        let s = s.split(['?', '#']).next().unwrap_or(s);
        let mut s = s.to_string();
        for _ in 0..2 {
            if let Some((head, tail)) = s.rsplit_once(':')
                && !tail.is_empty()
                && tail.bytes().all(|b| b.is_ascii_digit())
            {
                s = head.to_string();
            }
        }
        truncate(&s, MAX_FIELD_CHARS)
    });
    let kind = r.kind.trim();
    let message = if kind.is_empty() || kind == "error" {
        r.message
    } else {
        format!("[{}] {}", truncate(kind, 32), r.message)
    };
    Report {
        source: Some(ErrorSource::Client),
        message,
        location,
        stack: r.stack,
        url: r.url,
        user_agent,
        login,
        board: r.board.filter(|b| crate::valid_room_name(b)),
        release: r.release,
    }
}

// ---- tracing ----

/// ログの ERROR を拾う層。
pub struct ErrorLayer {
    monitor: Arc<Monitor>,
}

impl ErrorLayer {
    pub fn new(monitor: Arc<Monitor>) -> Self {
        Self { monitor }
    }
}

thread_local! {
    /// 取り込みの中でまたログを出しても、二重に拾わない。
    static CAPTURING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl<S: Subscriber> Layer<S> for ErrorLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if *meta.level() != Level::ERROR || CAPTURING.with(|c| c.replace(true)) {
            return;
        }
        let mut v = FieldVisitor::default();
        event.record(&mut v);
        let location = v.take("location").or_else(|| {
            Some(match (meta.file(), meta.line()) {
                (Some(f), Some(l)) => format!("{} ({f}:{l})", meta.target()),
                _ => meta.target().to_string(),
            })
        });
        let stack = v.take("backtrace");
        let board = v.take("room").or_else(|| v.take("board"));
        let message = v.text();
        self.monitor.capture(Report {
            source: Some(ErrorSource::Server),
            message,
            location,
            stack,
            board,
            release: Some(RELEASE.to_string()),
            ..Default::default()
        });
        CAPTURING.with(|c| c.set(false));
    }
}

/// メッセージとほかのフィールドを集める。
#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl FieldVisitor {
    fn take(&mut self, name: &str) -> Option<String> {
        let i = self.fields.iter().position(|(k, _)| k == name)?;
        Some(self.fields.remove(i).1)
    }

    /// メッセージのあとに残りのフィールドを `k=v` で付ける。
    fn text(&self) -> String {
        let mut s = self.message.clone();
        for (k, v) in &self.fields {
            let _ = write!(s, " {k}={v}");
        }
        s
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let v = format!("{value:?}");
        if field.name() == "message" {
            self.message = v;
        } else {
            self.fields.push((field.name().to_string(), v));
        }
    }
}

/// パニックをログの ERROR として出す(→ [`ErrorLayer`] が拾い、Error Reporting にも載る)。
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(内容不明)".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let bt = std::backtrace::Backtrace::force_capture().to_string();
        tracing::error!(
            target: "panic",
            location = %location,
            backtrace = %truncate(&bt, MAX_STACK_CHARS),
            "パニック: {msg}"
        );
        prev(info);
    }));
}

/// ログの出力を始める(`RUST_LOG` と `LOG_FORMAT`)。ERROR は [`global`] に集める。
pub fn init_logging(format: LogFormat) {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let text = (format == LogFormat::Text).then(tracing_subscriber::fmt::layer);
    let json = (format != LogFormat::Text).then(|| {
        tracing_subscriber::fmt::layer().event_format(JsonFormat {
            gcp: format == LogFormat::Gcp,
        })
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(text)
        .with(json)
        .with(ErrorLayer::new(global()))
        .init();
}

/// JSON で 1 行ずつ出す。`gcp` なら Cloud Logging の特別なフィールドを使う。
pub struct JsonFormat {
    pub gcp: bool,
}

#[derive(Default)]
struct JsonVisitor(serde_json::Map<String, serde_json::Value>);

impl Visit for JsonVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().into(), format!("{value:?}").into());
    }
}

impl<S, N> FormatEvent<S, N> for JsonFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let line = json_line(event, self.gcp, now_ms());
        writeln!(writer, "{line}")
    }
}

/// 1 件分の JSON。
pub fn json_line(event: &Event<'_>, gcp: bool, at_ms: u64) -> String {
    use serde_json::{Value, json};
    let meta = event.metadata();
    let mut v = JsonVisitor::default();
    event.record(&mut v);
    let mut fields = v.0;
    let message = match fields.remove("message") {
        Some(Value::String(s)) => s,
        Some(other) => other.to_string(),
        None => String::new(),
    };
    let mut obj = serde_json::Map::new();
    let level = *meta.level();
    if gcp {
        let severity = match level {
            Level::ERROR => "ERROR",
            Level::WARN => "WARNING",
            Level::INFO => "INFO",
            _ => "DEBUG",
        };
        obj.insert("severity".into(), severity.into());
        obj.insert("time".into(), rfc3339(at_ms).into());
        let file = meta.file().unwrap_or("");
        let line = meta.line().unwrap_or(0);
        obj.insert(
            "logging.googleapis.com/sourceLocation".into(),
            json!({ "file": file, "line": line.to_string(), "function": meta.target() }),
        );
        if level == Level::ERROR {
            // Error Reporting に載せる形。スタックトレースがなくても reportLocation があれば拾われる。
            obj.insert(
                "@type".into(),
                "type.googleapis.com/google.devtools.clouderrorreporting.v1beta1.ReportedErrorEvent"
                    .into(),
            );
            obj.insert(
                "serviceContext".into(),
                json!({ "service": "graboard", "version": RELEASE }),
            );
            obj.insert(
                "context".into(),
                json!({ "reportLocation": {
                    "filePath": file, "lineNumber": line, "functionName": meta.target()
                } }),
            );
        }
        let message = match fields.get("backtrace").and_then(Value::as_str) {
            Some(bt) => format!("{message}\n{bt}"),
            None => message,
        };
        obj.insert("message".into(), message.into());
    } else {
        obj.insert("timestamp".into(), rfc3339(at_ms).into());
        obj.insert("level".into(), level.as_str().into());
        obj.insert("target".into(), meta.target().into());
        obj.insert("message".into(), message.into());
    }
    for (k, val) in fields {
        obj.entry(k).or_insert(val);
    }
    Value::Object(obj).to_string()
}

/// UNIX ミリ秒を `2026-11-01T00:00:00.000Z` にする。
pub fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Howard Hinnant の civil_from_days。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

// ---- 外への知らせ ----

/// 保存と外への知らせを裏で回す。`config` は起動時の設定。
pub fn spawn(
    monitor: Arc<Monitor>,
    store: Arc<Store>,
    config: MonitorConfig,
) -> tokio::task::JoinHandle<()> {
    if let Err(e) = monitor.load(&store) {
        tracing::warn!("エラーの記録を読めません: {e}");
    }
    let wants_outbox = config.sentry.is_some() || config.slack_webhook.is_some();
    let _ = monitor.config.set(config.clone());
    if wants_outbox {
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        *monitor.outbox.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        tokio::spawn(deliver(rx, config));
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(FLUSH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let (m, s) = (monitor.clone(), store.clone());
            match tokio::task::spawn_blocking(move || m.flush(&s)).await {
                Ok(Err(e)) => eprintln!("エラーの記録を保存できません: {e}"),
                Err(e) => eprintln!("エラーの記録の保存が止まりました: {e}"),
                Ok(Ok(())) => {}
            }
        }
    })
}

async fn deliver(mut rx: tokio::sync::mpsc::Receiver<Outgoing>, config: MonitorConfig) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTP クライアント");
    let sentry_window = Window::new(Duration::from_secs(60), SENTRY_PER_MINUTE);
    let slack_window = Window::new(Duration::from_secs(3600), SLACK_PER_HOUR);
    while let Some(out) = rx.recv().await {
        // 送れなかったことは eprintln で出す(ログの ERROR にすると、また送ろうとしてしまう)。
        if let Some(dsn) = &config.sentry
            && sentry_window.take("sentry").is_none()
        {
            let (body, auth) = sentry_envelope(dsn, &out.group, config.environment.as_deref());
            let res = http
                .post(&dsn.envelope_url)
                .header("X-Sentry-Auth", auth)
                .header(header::CONTENT_TYPE, "application/x-sentry-envelope")
                .body(body)
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => eprintln!("Sentry に送れませんでした: HTTP {}", r.status()),
                Err(e) => eprintln!("Sentry に送れませんでした: {e}"),
            }
        }
        if out.is_new
            && let Some(url) = &config.slack_webhook
            && slack_window.take("slack").is_none()
        {
            let text = slack_text(&out.group, config.public_url.as_deref());
            let res = http
                .post(url)
                .json(&serde_json::json!({ "text": text }))
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => eprintln!("Slack に送れませんでした: HTTP {}", r.status()),
                Err(e) => eprintln!("Slack に送れませんでした: {e}"),
            }
        }
    }
}

/// Sentry の envelope(本文)と `X-Sentry-Auth` ヘッダー。
pub fn sentry_envelope(
    dsn: &SentryDsn,
    g: &ErrorGroup,
    environment: Option<&str>,
) -> (String, String) {
    use serde_json::json;
    let mut raw = [0u8; 16];
    let _ = getrandom::fill(&mut raw);
    let event_id: String = raw.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    let s = &g.sample;
    let platform = match g.source {
        ErrorSource::Server => "native",
        ErrorSource::Client => "javascript",
    };
    let mut event = json!({
        "event_id": event_id,
        "timestamp": s.at as f64 / 1000.0,
        "platform": platform,
        "level": "error",
        "logger": g.location.clone().unwrap_or_default(),
        "release": format!("graboard@{}", s.release.as_deref().unwrap_or(RELEASE)),
        "message": { "formatted": s.message },
        "fingerprint": [g.id],
        "tags": { "source": match g.source { ErrorSource::Server => "server", ErrorSource::Client => "client" } },
    });
    if let Some(env) = environment {
        event["environment"] = env.into();
    }
    if let Some(stack) = &s.stack {
        event["extra"] = json!({ "stack": stack });
    }
    if let Some(board) = &s.board {
        event["tags"]["board"] = board.clone().into();
    }
    if let Some(login) = &s.login {
        event["user"] = json!({ "username": login });
    }
    if s.url.is_some() || s.user_agent.is_some() {
        event["request"] = json!({
            "url": s.url,
            "headers": { "User-Agent": s.user_agent },
        });
    }
    let body = format!(
        "{}\n{}\n{}\n",
        json!({ "event_id": event_id, "dsn": dsn.raw }),
        json!({ "type": "event" }),
        event
    );
    let auth = format!(
        "Sentry sentry_version=7, sentry_key={}, sentry_client=graboard/{RELEASE}",
        dsn.public_key
    );
    (body, auth)
}

fn slack_text(g: &ErrorGroup, public_url: Option<&str>) -> String {
    let mut t = format!(
        ":rotating_light: グラボードで{}のエラー({})\n```{}```",
        if g.count > 1 {
            "再発"
        } else {
            "新しい種類"
        },
        g.source.label(),
        crate::slack::escape(&truncate(&g.message, 300)),
    );
    if let Some(loc) = &g.location {
        let _ = write!(t, "\n場所: {}", crate::slack::escape(loc));
    }
    if let Some(base) = public_url {
        let base = base.trim_end_matches('/');
        let _ = write!(t, "\n<{base}/?ops=errors|運用の画面で見る>");
    }
    t
}

// ---- HTTP ----

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/errors", post(report))
        .route("/admin/errors", axum::routing::get(list_groups))
        .route(
            "/admin/errors/{id}",
            patch(set_resolved).delete(remove_group),
        )
}

/// サーバー管理者か確かめる。
pub(crate) fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<UserRecord, Error> {
    let user = require_user(state, headers)?;
    if !state.config.is_admin(&user.login) {
        return Err(Error::forbidden());
    }
    Ok(user)
}

async fn report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ClientIp(ip): ClientIp,
    body: Bytes,
) -> StatusCode {
    // ほかのサイトから大量に送りつけられないように。
    if !crate::guard::same_origin(&headers, &state.config.limits.allowed_origins) {
        return StatusCode::FORBIDDEN;
    }
    if body.len() > MAX_REPORT_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }
    let monitor = &state.config.monitor;
    let key = ip.map_or_else(|| "unknown".into(), |ip| ip.to_string());
    if monitor.client_window.take(&key).is_some() {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    // sendBeacon は text/plain で送ってくるので、形式は見ずに JSON として読む。
    let Ok(r) = serde_json::from_slice::<ClientErrorReport>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let login = crate::api::current_user(&state, &headers)
        .ok()
        .flatten()
        .map(|u| u.login);
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    monitor.capture(client_report(r, login, ua));
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    all: Option<String>,
}

async fn list_groups(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<ErrorStatus>, Error> {
    require_admin(&state, &headers)?;
    let all = matches!(q.all.as_deref(), Some("1") | Some("true"));
    Ok(Json(state.config.monitor.status(all)))
}

async fn set_resolved(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SetErrorResolved>,
) -> Result<Json<ErrorGroup>, Error> {
    require_admin(&state, &headers)?;
    state
        .config
        .monitor
        .set_resolved(&id, req.resolved)
        .map(Json)
        .ok_or_else(|| Error::new(StatusCode::NOT_FOUND, "そのエラーは見つかりません"))
}

async fn remove_group(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, Error> {
    require_admin(&state, &headers)?;
    if state.config.monitor.remove(&id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Error::new(
            StatusCode::NOT_FOUND,
            "そのエラーは見つかりません",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(msg: &str) -> Report {
        Report {
            message: msg.into(),
            location: Some("board_server::room".into()),
            ..Default::default()
        }
    }

    #[test]
    fn groups_similar_errors() {
        let m = Monitor::new();
        m.capture(server("保存に失敗: room 123 ab12cd34ef56"));
        m.capture(server("保存に失敗: room 456 99aa88bb77cc"));
        m.capture(server("別のエラー"));
        let list = m.list(false);
        assert_eq!(list.len(), 2);
        let saved = list.iter().find(|g| g.message.starts_with("保存")).unwrap();
        assert_eq!(saved.count, 2);
        assert!(saved.sample.message.contains("456"));
        assert_ne!(
            fingerprint(ErrorSource::Server, None, "x"),
            fingerprint(ErrorSource::Client, None, "x")
        );
    }

    #[test]
    fn resolve_and_regress() {
        let m = Monitor::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        *m.outbox.lock().unwrap() = Some(tx);
        m.capture(server("落ちた"));
        assert!(rx.try_recv().unwrap().is_new);
        m.capture(server("落ちた"));
        assert!(!rx.try_recv().unwrap().is_new);
        let id = m.list(false)[0].id.clone();
        assert!(m.set_resolved(&id, true).unwrap().resolved);
        assert!(m.list(false).is_empty());
        assert_eq!(m.list(true).len(), 1);
        m.capture(server("落ちた"));
        assert!(
            rx.try_recv().unwrap().is_new,
            "再発は新しいものとして知らせる"
        );
        assert_eq!(m.open_count(), 1);
    }

    #[test]
    fn persists_and_prunes() {
        let store = Store::memory();
        let m = Monitor::new();
        m.capture(server("一つ目"));
        m.capture(server("二つ目"));
        m.flush(&store).unwrap();
        let id = m.list(false)[0].id.clone();
        assert!(m.remove(&id));
        m.flush(&store).unwrap();

        let again = Monitor::new();
        again.load(&store).unwrap();
        assert_eq!(again.list(true).len(), 1);

        let big = Monitor::new();
        for i in 0..MAX_GROUPS + 5 {
            big.capture(Report {
                message: format!("エラー{}", "あ".repeat(i % 50 + 1)),
                location: Some(format!("場所{}", "x".repeat(i))),
                ..Default::default()
            });
        }
        assert_eq!(big.list(true).len(), MAX_GROUPS);
    }

    #[test]
    fn client_reports_drop_line_numbers() {
        let a = client_report(
            ClientErrorReport {
                kind: "panic".into(),
                message: "index out of bounds".into(),
                source: Some("https://b.example/board-web-abc.js?v=1:10:20".into()),
                board: Some("../bad".into()),
                ..Default::default()
            },
            Some("alice".into()),
            None,
        );
        assert_eq!(
            a.location.as_deref(),
            Some("https://b.example/board-web-abc.js")
        );
        assert_eq!(a.message, "[panic] index out of bounds");
        assert_eq!(a.board, None);
    }

    #[test]
    fn sentry_dsn() {
        let d = SentryDsn::parse("https://abc123@o1.ingest.sentry.io/4505").unwrap();
        assert_eq!(
            d.envelope_url,
            "https://o1.ingest.sentry.io/api/4505/envelope/"
        );
        assert_eq!(d.public_key, "abc123");
        let d = SentryDsn::parse("http://k:secret@localhost:9000/sentry/7").unwrap();
        assert_eq!(
            d.envelope_url,
            "http://localhost:9000/sentry/api/7/envelope/"
        );
        assert!(SentryDsn::parse("https://o1.ingest.sentry.io/4505").is_none());
        assert!(SentryDsn::parse("ftp://k@h/1").is_none());

        let m = Monitor::new();
        m.capture(server("落ちた"));
        let g = m.list(false).remove(0);
        let (body, auth) = sentry_envelope(&d, &g, Some("prod"));
        let lines: Vec<serde_json::Value> = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1]["type"], "event");
        assert_eq!(lines[2]["message"]["formatted"], "落ちた");
        assert_eq!(lines[2]["environment"], "prod");
        assert_eq!(lines[2]["fingerprint"][0], g.id);
        assert!(auth.contains("sentry_key=k"));
    }

    #[test]
    fn dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_793_491_200_123), "2026-11-01T00:00:00.123Z");
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn layer_captures_errors_and_gcp_format() {
        use tracing_subscriber::layer::SubscriberExt;
        let m = Arc::new(Monitor::new());
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        struct Capture(Arc<Mutex<Vec<String>>>);
        impl<S: Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
                self.0.lock().unwrap().push(json_line(event, true, 0));
            }
        }
        let subscriber = tracing_subscriber::registry()
            .with(ErrorLayer::new(m.clone()))
            .with(Capture(lines.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("ふつうのログ");
            tracing::error!(room = "abc", code = 7, "保存に失敗: disk full");
        });
        let list = m.list(false);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].sample.board.as_deref(), Some("abc"));
        assert!(list[0].message.contains("code=7"));
        let lines = lines.lock().unwrap();
        let info: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(info["severity"], "INFO");
        assert!(info.get("@type").is_none());
        let err: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(err["severity"], "ERROR");
        assert_eq!(err["message"], "保存に失敗: disk full");
        assert_eq!(err["room"], "abc");
        assert!(
            err["@type"]
                .as_str()
                .unwrap()
                .ends_with("ReportedErrorEvent")
        );
        assert!(
            err["context"]["reportLocation"]["lineNumber"]
                .as_u64()
                .unwrap()
                > 0
        );
    }
}
