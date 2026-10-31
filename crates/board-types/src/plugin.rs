//! WASM プラグインの取り決め(マニフェスト・実行の入出力・ホスト関数)。
//!
//! プラグインは `wasm32-unknown-unknown` 向けの WebAssembly モジュール 1 つ。
//! サーバーがサンドボックス(燃料=命令数の上限、メモリの上限、WASI なし)で実行し、
//! ボードには下のホスト関数を通してだけ触れる。書き込みは使い捨てのボードに溜め、
//! 実行が成功したときだけ CRDT の更新として全員に配る。
//!
//! ## ABI(バージョン 1)
//!
//! プラグインが出すもの:
//!
//! | 名前 | 型 | 内容 |
//! |------|----|------|
//! | `memory` | メモリ | 線形メモリ |
//! | `gb_alloc` | `(len: i32) -> i32` | `len` バイトの領域を確保して先頭を返す |
//! | `gb_manifest` | `() -> i64` | [`PluginManifest`] の JSON(`ptr << 32 \| len`) |
//! | `gb_run` | `(ptr: i32, len: i32) -> i64` | [`RunInput`] の JSON を受け取り、[`Reply`]`<`[`RunOutput`]`>` の JSON を返す |
//!
//! ホストが渡すもの(モジュール `graboard`):
//!
//! | 名前 | 型 | 内容 |
//! |------|----|------|
//! | `call` | `(ptr: i32, len: i32) -> i32` | [`HostCall`] の JSON を送る。返り値は応答([`Reply`])の長さ |
//! | `response` | `(dst: i32)` | 直前の `call` の応答を `dst` に書く |
//!
//! Rust なら `graboard-plugin` クレートがこれを包んでいるので、ABI を意識する必要はない。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ops::{ElementQuery, Op};

/// いまの ABI のバージョン。
pub const ABI_VERSION: u32 = 1;

/// プラグインが求める権限。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Permission {
    /// ボードの要素を読む。
    #[serde(rename = "board:read")]
    BoardRead,
    /// ボードの要素を作る・変える・消す。
    #[serde(rename = "board:write")]
    BoardWrite,
}

impl Permission {
    pub fn label(self) -> &'static str {
        match self {
            Permission::BoardRead => "ボードを読む",
            Permission::BoardWrite => "ボードを編集する",
        }
    }
}

/// プラグインの自己紹介。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// 英小文字・数字・`-`・`_`(1〜64 文字)。
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    pub commands: Vec<PluginCommand>,
    #[serde(default = "default_abi")]
    pub abi: u32,
}

fn default_abi() -> u32 {
    ABI_VERSION
}

impl PluginManifest {
    pub fn can(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }

    /// 形を確かめる。
    pub fn validate(&self) -> Result<(), String> {
        let id_ok = (1..=64).contains(&self.id.len())
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !id_ok {
            return Err(format!(
                "プラグイン ID「{}」は英小文字・数字・-・_ の 1〜64 文字にしてください",
                self.id
            ));
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 80 {
            return Err("プラグインの名前は 1〜80 文字にしてください".into());
        }
        if self.abi != ABI_VERSION {
            return Err(format!(
                "ABI バージョン {} には対応していません({ABI_VERSION} にしてください)",
                self.abi
            ));
        }
        if self.commands.is_empty() || self.commands.len() > 50 {
            return Err("コマンドは 1〜50 個にしてください".into());
        }
        for c in &self.commands {
            if c.id.is_empty() || c.id.len() > 64 || c.title.trim().is_empty() {
                return Err(format!(
                    "コマンド「{}」の ID か名前が正しくありません",
                    c.id
                ));
            }
        }
        if self.description.chars().count() > 2000 {
            return Err("説明が長すぎます".into());
        }
        Ok(())
    }

    pub fn command(&self, id: &str) -> Option<&PluginCommand> {
        self.commands.iter().find(|c| c.id == id)
    }
}

/// メニューに出すコマンド。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginCommand {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
}

/// 実行するボード。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardRef {
    pub id: String,
    pub title: String,
}

/// 実行した人。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UserRef {
    pub login: String,
    pub name: String,
}

/// `gb_run` に渡す入力。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunInput {
    pub command: String,
    /// 呼び出し側が渡す任意の引数。
    #[serde(default)]
    pub args: Value,
    pub board: BoardRef,
    #[serde(default)]
    pub user: Option<UserRef>,
    /// 選択中の要素の ID。
    #[serde(default)]
    pub selection: Vec<String>,
    /// 書き込みが許されているか。
    #[serde(default)]
    pub can_write: bool,
}

/// `gb_run` の出力。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunOutput {
    /// 実行した人に見せる短いメッセージ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 実行後に選択してほしい要素。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Vec<String>>,
    /// 呼び出し側に返す任意のデータ(REST API で使う)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// 成功か失敗か。`{"ok": …}` か `{"err": "…"}`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reply<T> {
    Ok(T),
    Err(String),
}

impl<T> From<Result<T, String>> for Reply<T> {
    fn from(r: Result<T, String>) -> Self {
        match r {
            Ok(v) => Reply::Ok(v),
            Err(e) => Reply::Err(e),
        }
    }
}

impl<T> From<Reply<T>> for Result<T, String> {
    fn from(r: Reply<T>) -> Self {
        match r {
            Reply::Ok(v) => Ok(v),
            Reply::Err(e) => Err(e),
        }
    }
}

/// プラグインからホストへの呼び出し。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "fn", rename_all = "snake_case")]
pub enum HostCall {
    /// 要素を探す(`board:read`)→ [`crate::ops::ElementData`] の配列。
    Query {
        #[serde(default)]
        query: ElementQuery,
    },
    /// 要素を 1 つ取る(`board:read`)→ [`crate::ops::ElementData`] か `null`。
    Get { id: String },
    /// 操作を適用する(`board:write`)→ [`crate::ops::OpsResult`]。
    /// 失敗した一括操作は何も変えない。成功したものは実行の最後にまとめて全員へ配られる。
    Apply { ops: Vec<Op> },
    /// ログに残す(実行結果と一緒に返る)。
    Log { message: String },
}

// ---- REST API(`/api/v1/plugins`)----

/// 一覧に出すプラグイン。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    #[serde(flatten)]
    pub manifest: PluginManifest,
    /// `server`(サーバーのプラグイン)か `user`(自分で入れたもの)。
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<u64>,
    pub size: usize,
}

/// 実行のリクエスト。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RunRequest {
    pub command: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub selection: Vec<String>,
}

/// 実行の結果。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub output: RunOutput,
    /// ボードに適用した操作の数。
    pub ops: usize,
    pub logs: Vec<String>,
    /// 使った燃料(命令数の目安)。
    pub fuel_used: u64,
}
