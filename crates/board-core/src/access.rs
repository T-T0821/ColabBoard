//! 外部から使う API トークンの型(`/api/tokens` と `/api/v1/...`)。

use serde::{Deserialize, Serialize};

use crate::api::Role;

/// トークンでできること。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenScope {
    /// 読むだけ(閲覧者として扱う)。
    Read,
    /// 読み書き(編集者として扱う)。共有設定やボードの削除はできない。
    Write,
}

impl TokenScope {
    /// このトークンで使える権限の上限。
    pub fn max_role(self) -> Role {
        match self {
            TokenScope::Read => Role::Viewer,
            TokenScope::Write => Role::Editor,
        }
    }
}

/// トークンの作成(`POST /api/tokens`)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewToken {
    /// 用途がわかる名前(例: 「CI からの書き込み」)。
    pub name: String,
    pub scope: TokenScope,
    /// 使えるボードを限る(省略すると自分が開けるボードすべて)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boards: Option<Vec<String>>,
    /// 有効期間(日)。省略すると無期限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<u32>,
}

/// トークンの情報(秘密の部分は含まない)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub scope: TokenScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boards: Option<Vec<String>>,
    /// 作った時刻(UNIX ミリ秒)。
    pub created: u64,
    /// 最後に使った時刻(おおよそ)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<u64>,
}

/// 作ったトークン。`token` はこのときだけ返す。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreatedToken {
    /// `Authorization: Bearer <token>` で送る値。
    pub token: String,
    pub info: TokenInfo,
}

/// `GET /api/v1/me`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Whoami {
    pub login: String,
    pub name: String,
    /// トークンで呼んだときはその情報。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<TokenInfo>,
}
