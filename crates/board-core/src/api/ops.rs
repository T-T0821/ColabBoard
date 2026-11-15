//! 運用の画面(サーバー管理者向け)でやり取りする型: バックアップ。
//!
//! | メソッド | パス | 内容 |
//! |----------|------|------|
//! | `GET` | `/api/admin/backups` | バックアップの設定と一覧([`BackupStatus`]) |
//! | `POST` | `/api/admin/backups` | 今すぐバックアップを取る(→ [`BackupInfo`]) |
//! | `GET` | `/api/admin/backups/{id}/download` | バックアップのファイル(`.grabak.gz`) |
//! | `GET` | `/api/admin/backups/{id}/boards` | バックアップに入っているボード([`BackupBoard`] の配列) |
//! | `POST` | `/api/admin/backups/{id}/boards/{board}/restore` | ボードを 1 つ戻す(→ [`RestoreResult`]) |
//!
//! `/api/admin/...` はサーバー管理者(`ADMIN_LOGINS`)だけが呼べる。

use serde::{Deserialize, Serialize};

use super::VersionInfo;

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
