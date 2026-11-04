//! アプリの設定(接続先など)。設定フォルダの `settings.json` に保存する。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 内蔵サーバーが最初に試すポート。毎回同じポートで開くと、ページのオリジン
/// (`http://127.0.0.1:47180`)が変わらないので、ログイン状態や端末内の保存(IndexedDB)が引き継がれる。
pub const DEFAULT_LOCAL_PORT: u16 = 47180;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// この PC の中だけで使う(内蔵サーバー)。ネットワークがなくてもすべて動く。
    #[default]
    Local,
    /// チームのサーバーにつなぐ。
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    /// つなぐサーバー(`https://board.example.com/` の形に整えたもの)。
    pub server_url: Option<String>,
    /// 内蔵サーバーが前回使ったポート。
    pub local_port: Option<u16>,
    /// 内蔵サーバーの AI 機能で使う Anthropic の API キー(任意)。
    pub anthropic_api_key: Option<String>,
    /// 一度でも開けたサーバー。オフラインでもアプリ本体とボードが端末に残っているので、
    /// つながらなくてもそのまま開く(初めてのサーバーは開けないので内蔵サーバーで開く)。
    pub opened_servers: Vec<String>,
    /// 接続先ごとに最後に開いていたボードのページ。次に起動したときはそこから開く
    /// (オフラインで開いたときも、新しい空のボードではなく前のボードが出る)。
    pub last_pages: BTreeMap<String, String>,
}

impl Settings {
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("settings.json")
    }

    /// 読めなければ既定値(壊れたファイルで起動できなくなるよりよい)。
    pub fn load(config_dir: &Path) -> Self {
        std::fs::read(Self::path(config_dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, config_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let path = Self::path(config_dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    /// `base`(接続先)で最後に開いていたページ。なければ `base` そのもの。
    pub fn start_page(&self, base: &str) -> String {
        self.last_pages
            .get(base)
            .filter(|p| p.starts_with(base))
            .cloned()
            .unwrap_or_else(|| base.to_string())
    }

    pub fn ai_key(&self) -> Option<&str> {
        self.anthropic_api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
    }
}

/// 入力されたサーバーの URL を `scheme://host[:port]/path/` の形に整える。
/// スキームを省いたら `https://` を補う。http / https 以外は受け付けない。
pub fn normalize_server_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("サーバーの URL を入力してください".into());
    }
    let with_scheme = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let mut url =
        url::Url::parse(&with_scheme).map_err(|_| "URL の形が正しくありません".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("http:// か https:// で始まる URL を入力してください".into());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("URL にホスト名がありません".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL にユーザー名やパスワードは入れられません".into());
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_server_url("board.example.com").unwrap(),
            "https://board.example.com/"
        );
        assert_eq!(
            normalize_server_url(" http://10.0.0.5:8080 ").unwrap(),
            "http://10.0.0.5:8080/"
        );
        assert_eq!(
            normalize_server_url("https://example.com/board?room=a#x").unwrap(),
            "https://example.com/board/"
        );
        assert!(normalize_server_url("").is_err());
        assert!(normalize_server_url("ftp://example.com").is_err());
        assert!(normalize_server_url("file:///etc/passwd").is_err());
        assert!(normalize_server_url("https://user:pw@example.com").is_err());
    }

    #[test]
    fn broken_settings_fall_back_to_default() {
        let dir = std::env::temp_dir().join(format!("graboard-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(Settings::path(&dir), b"{not json").unwrap();
        let s = Settings::load(&dir);
        assert_eq!(s.mode, Mode::Local);
        let s = Settings {
            mode: Mode::Remote,
            server_url: Some("https://example.com/".into()),
            local_port: Some(47181),
            ..Default::default()
        };
        s.save(&dir).unwrap();
        let back = Settings::load(&dir);
        assert_eq!(back.mode, Mode::Remote);
        assert_eq!(back.local_port, Some(47181));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
