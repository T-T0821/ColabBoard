//! 運用の画面(サーバー管理者向け)でやり取りする型: エラー監視とバックアップ。
//!
//! | メソッド | パス | 内容 |
//! |----------|------|------|
//! | `POST` | `/api/errors` | ブラウザで起きたエラーを知らせる([`ClientErrorReport`]、ログイン不要) |
//! | `GET` | `/api/admin/errors?all=1` | 集めたエラー([`ErrorStatus`]。既定は未解決だけ) |
//! | `PATCH` | `/api/admin/errors/{id}` | 解決済みにする・戻す([`SetErrorResolved`] → [`ErrorGroup`]) |
//! | `DELETE` | `/api/admin/errors/{id}` | 記録を消す |
//! | `GET` | `/api/admin/backups` | バックアップの設定と一覧([`BackupStatus`]) |
//! | `POST` | `/api/admin/backups` | 今すぐバックアップを取る(→ [`BackupInfo`]) |
//! | `GET` | `/api/admin/backups/{id}/download` | バックアップのファイル(`.grabak.gz`) |
//! | `GET` | `/api/admin/backups/{id}/boards` | バックアップに入っているボード([`BackupBoard`] の配列) |
//! | `POST` | `/api/admin/backups/{id}/boards/{board}/restore` | ボードを 1 つ戻す(→ [`RestoreResult`]) |
//!
//! `/api/admin/...` はサーバー管理者(`ADMIN_LOGINS`)だけが呼べる。

use serde::{Deserialize, Serialize};

use super::VersionInfo;

/// エラーが起きた場所。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorSource {
    /// サーバー(ログの ERROR とパニック)。
    Server,
    /// 利用者のブラウザ。
    Client,
}

impl ErrorSource {
    pub fn label(self) -> &'static str {
        match self {
            ErrorSource::Server => "サーバー",
            ErrorSource::Client => "ブラウザ",
        }
    }
}

/// 同じ種類のエラーをまとめたもの。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorGroup {
    /// メッセージと場所から作る ID(16 進 16 文字)。
    pub id: String,
    pub source: ErrorSource,
    /// 代表のメッセージ(1 行目)。
    pub message: String,
    /// 起きた場所(サーバーはモジュールとファイル、ブラウザはスクリプトの URL)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub first_seen: u64,
    pub last_seen: u64,
    pub count: u64,
    /// 解決済みにしたか。もう一度起きると未解決に戻る。
    #[serde(default)]
    pub resolved: bool,
    /// いちばん新しい 1 件の詳細。
    pub sample: ErrorSample,
}

/// エラー 1 件の詳細。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ErrorSample {
    pub at: u64,
    pub message: String,
    /// スタックトレース(あれば)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// 開いていたページ(ブラウザ)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// ログインしていた人。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    /// 起きたときの版。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
}

/// ブラウザから送るエラー。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientErrorReport {
    /// `error`・`unhandledrejection`・`panic` など。
    #[serde(default)]
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub stack: Option<String>,
    /// エラーの起きたスクリプト(`ファイル:行:列`)。
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub board: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
}

/// エラーの一覧と、外への通知の設定。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ErrorStatus {
    /// 最後に起きた順。
    pub groups: Vec<ErrorGroup>,
    /// 未解決のまとまりの数(絞り込みに関係なく)。
    pub open: usize,
    /// Sentry に送っているか。
    pub sentry: bool,
    /// Slack に知らせているか。
    pub slack: bool,
    /// ログの形式(`text`・`json`・`gcp`)。
    pub log_format: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetErrorResolved {
    pub resolved: bool,
}

/// バックアップ 1 つ。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupInfo {
    pub id: u64,
    pub created: u64,
    /// ファイルの大きさ(圧縮後)。
    pub bytes: u64,
    pub boards: usize,
    pub users: usize,
    /// `auto`(定期)・`manual`(手動)・`before-restore`(戻す前に自動で取ったもの)。
    pub trigger: String,
    /// 手動で取った人。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// 読めなかったテーブル(新しい種類のキーなど)。空なら全部入っている。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

impl BackupInfo {
    pub fn trigger_label(&self) -> &'static str {
        match self.trigger.as_str() {
            "auto" => "定期",
            "manual" => "手動",
            "before-restore" => "復元の前",
            _ => "その他",
        }
    }
}

/// バックアップの設定と一覧。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupStatus {
    /// バックアップ先があるか(`DATA_DIR=` で保存しないときなどは false)。
    pub enabled: bool,
    /// 保存先の説明(認証情報は含めない)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// 定期バックアップの間隔(時間)。0 なら手動だけ。
    pub interval_hours: u64,
    /// 残す数(種類ごと)。
    pub keep: usize,
    /// 次の定期バックアップの予定(UNIX ミリ秒)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<u64>,
    /// 最後に失敗したときの理由。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 取っている最中か。
    #[serde(default)]
    pub running: bool,
    /// 新しい順。
    pub backups: Vec<BackupInfo>,
}

/// バックアップの中のボードと、今のサーバーでの状態。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupBoard {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub updated: u64,
    /// 中身の大きさ(バイト)。
    pub bytes: usize,
    pub comments: usize,
    pub versions: usize,
    /// 今のサーバーでの状態: `exists`(ある)・`deleted`(削除済み)・`missing`(ない)。
    pub current: String,
}

/// ボードを戻した結果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestoreResult {
    pub board: String,
    /// 削除されていたボードを作り直したか(false なら中身だけ戻した)。
    pub recreated: bool,
    /// 戻す前の状態を残したバージョン(中身だけ戻したとき)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<VersionInfo>,
}
