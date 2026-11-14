//! ログの出力(`RUST_LOG` と `LOG_FORMAT`)。
//!
//! - `LOG_FORMAT=text`(既定): 人が読む形式。
//! - `LOG_FORMAT=json`: 1 行 1 件の JSON(Datadog・Loki などに送るとき)。

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

use crate::auth::now_ms;

/// サーバーの版(バックアップのヘッダーなどに使う)。
pub const RELEASE: &str = env!("CARGO_PKG_VERSION");

/// ログの形式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// 人が読む形式(既定)。
    Text,
    /// 1 行 1 件の JSON。
    Json,
}

impl LogFormat {
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("json") => LogFormat::Json,
            _ => LogFormat::Text,
        }
    }

    pub fn from_env() -> Self {
        Self::parse(std::env::var("LOG_FORMAT").ok().as_deref())
    }
}

/// ログの出力を始める(`RUST_LOG` と `LOG_FORMAT`)。
pub fn init_logging(format: LogFormat) {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let text = (format == LogFormat::Text).then(tracing_subscriber::fmt::layer);
    let json = (format == LogFormat::Json)
        .then(|| tracing_subscriber::fmt::layer().event_format(JsonFormat));
    tracing_subscriber::registry()
        .with(filter)
        .with(text)
        .with(json)
        .init();
}

/// JSON で 1 行ずつ出す。
pub struct JsonFormat;

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
        let line = json_line(event, now_ms());
        writeln!(writer, "{line}")
    }
}

/// 1 件分の JSON。
pub fn json_line(event: &Event<'_>, at_ms: u64) -> String {
    use serde_json::Value;
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
    obj.insert("timestamp".into(), rfc3339(at_ms).into());
    obj.insert("level".into(), meta.level().as_str().into());
    obj.insert("target".into(), meta.target().into());
    obj.insert("message".into(), message.into());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_format() {
        assert_eq!(LogFormat::parse(None), LogFormat::Text);
        assert_eq!(LogFormat::parse(Some(" JSON ")), LogFormat::Json);
        assert_eq!(LogFormat::parse(Some("gcp")), LogFormat::Text);
    }

    #[test]
    fn dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_793_491_200_123), "2026-11-01T00:00:00.123Z");
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }
}
