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
//! ## 権限のサンドボックス
//!
//! プラグインはマニフェストで使いたい権限([`Permission`])を宣言し、外部と通信するなら
//! 接続先([`PluginManifest::hosts`])も宣言する。実行時に使えるのは
//! 「マニフェストで宣言したもの」∩「実行する人が許可したもの([`PluginGrant`])」∩
//! 「サーバーの管理者が認めたもの」だけ。ボードの読み書きは既定で許可済み、
//! 通信と保存領域は実行する人が一度許可するまで実行できない。
//!
//! Rust なら `graboard-plugin` クレートがこれを包んでいるので、ABI を意識する必要はない。

use std::collections::BTreeMap;

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
    /// マニフェストの `hosts` に書いた接続先にだけ、HTTPS で問い合わせる。
    #[serde(rename = "network")]
    Network,
    /// プラグイン専用の保存領域(実行する人ごと)を読み書きする。
    #[serde(rename = "storage")]
    Storage,
}

impl Permission {
    /// すべての権限(画面に並べる順)。
    pub const ALL: [Permission; 4] = [
        Permission::BoardRead,
        Permission::BoardWrite,
        Permission::Network,
        Permission::Storage,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Permission::BoardRead => "ボードを読む",
            Permission::BoardWrite => "ボードを編集する",
            Permission::Network => "外部と通信する",
            Permission::Storage => "データを保存する",
        }
    }

    /// 許可を求めるときの説明。
    pub fn detail(self) -> &'static str {
        match self {
            Permission::BoardRead => "ボードの要素(付箋・図形・文字など)を読みます",
            Permission::BoardWrite => "ボードの要素を作る・変える・消すことができます",
            Permission::Network => {
                "書かれた接続先にボードの内容を送ったり、そこからデータを取ったりできます"
            }
            Permission::Storage => {
                "このプラグイン専用の場所に、あなたごとのデータ(設定など)を残せます"
            }
        }
    }

    /// 実行する人の許可が要るか(ボードの読み書きは既定で許可済み)。
    pub fn needs_consent(self) -> bool {
        matches!(self, Permission::Network | Permission::Storage)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Permission::BoardRead => "board:read",
            Permission::BoardWrite => "board:write",
            Permission::Network => "network",
            Permission::Storage => "storage",
        }
    }
}

/// 宣言できる接続先の数。
pub const MAX_HOSTS: usize = 16;

/// 接続先の書き方が正しいか。`api.example.com`、`*.example.com`、`localhost:8080` など
/// (スキーム・パス・利用者名は書かない)。
pub fn valid_host_pattern(p: &str) -> bool {
    let (host, port) = split_port(p);
    if port == Some(None) {
        return false;
    }
    let host = host.strip_prefix("*.").unwrap_or(host);
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// `host[:port]` を分ける。ポートがあって読めなければ `Some(None)`。
fn split_port(s: &str) -> (&str, Option<Option<u16>>) {
    match s.rsplit_once(':') {
        Some((h, p)) => (h, Some(p.parse::<u16>().ok().filter(|p| *p != 0))),
        None => (s, None),
    }
}

/// 接続先の宣言 `pattern` が、実際の接続先 `host`(`host[:port]`、小文字)に当てはまるか。
/// ポートを書いていない宣言は既定のポートにだけ当てはまる。`*.example.com` は
/// `a.example.com` などに当てはまり、`example.com` 自身には当てはまらない。
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let (ph, pp) = split_port(pattern);
    let (hh, hp) = split_port(host);
    if pp != hp {
        return false;
    }
    match ph.strip_prefix("*.") {
        Some(suffix) => hh.len() > suffix.len() + 1 && hh.ends_with(&format!(".{suffix}")),
        None => ph == hh,
    }
}

/// 管理者の許可 `policy` が、プラグインの宣言 `pattern` を丸ごと含むか。
pub fn pattern_covered(policy: &str, pattern: &str) -> bool {
    if policy == pattern || host_matches(policy, pattern) {
        return true;
    }
    // `*.example.com` は `*.a.example.com` も含む。
    match (policy.strip_prefix("*."), pattern.strip_prefix("*.")) {
        (Some(ps), Some(qs)) => {
            let (qh, qp) = split_port(qs);
            let (ph, pp) = split_port(ps);
            qp == pp && qh.ends_with(&format!(".{ph}"))
        }
        _ => false,
    }
}

/// 通信先の URL を分けたもの。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlTarget {
    /// `https` か `http`。
    pub scheme: String,
    /// `host` か `host:port`(小文字)。
    pub host: String,
}

/// URL を確かめて、スキームと接続先を取り出す。
pub fn url_target(url: &str) -> Result<UrlTarget, String> {
    if url.len() > 8192 {
        return Err("URL が長すぎます".into());
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("URL「{url}」が読めません"))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err(format!("{scheme}:// には接続できません(https:// だけです)"));
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return Err("URL に利用者名やパスワードは書けません".into());
    }
    if authority.contains(['[', ']', '\\', ' ']) || rest.contains('\\') {
        return Err(format!("URL「{url}」が読めません"));
    }
    let mut host = authority.to_ascii_lowercase();
    // 既定のポートは書かなかったことにする。
    for (s, p) in [("https", ":443"), ("http", ":80")] {
        if scheme == s && host.ends_with(p) {
            host.truncate(host.len() - p.len());
        }
    }
    let host = host.trim_end_matches('.').to_string();
    if !valid_host_pattern(&host) || host.starts_with("*.") {
        return Err(format!("URL「{url}」の接続先が読めません"));
    }
    Ok(UrlTarget { scheme, host })
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
    /// `network` で接続してよい先(`api.example.com`、`*.example.com`、`host:port`)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
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
        if self.hosts.len() > MAX_HOSTS {
            return Err(format!("接続先は {MAX_HOSTS} 個までです"));
        }
        for h in &self.hosts {
            if !valid_host_pattern(h) {
                return Err(format!(
                    "接続先「{h}」が正しくありません(api.example.com や *.example.com のように書いてください)"
                ));
            }
        }
        if self.can(Permission::Network) && self.hosts.is_empty() {
            return Err("network の権限を求めるときは、接続先(hosts)を書いてください".into());
        }
        if !self.can(Permission::Network) && !self.hosts.is_empty() {
            return Err("接続先(hosts)を書くときは network の権限も求めてください".into());
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
    /// この実行で実際に使える権限(宣言 ∩ 実行する人の許可 ∩ サーバーの方針)。
    #[serde(default)]
    pub permissions: Vec<Permission>,
    /// この実行で接続してよい先。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
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
    /// HTTP で問い合わせる(`network`、宣言した接続先だけ)→ [`FetchResponse`]。
    Fetch(FetchRequest),
    /// 保存領域から読む(`storage`)→ 値か `null`。
    StorageGet { key: String },
    /// 保存領域に書く(`storage`)→ `null`。実行が成功したときだけ残る。
    StorageSet { key: String, value: Value },
    /// 保存領域から消す(`storage`)→ `null`。
    StorageDelete { key: String },
    /// 保存領域のキーの一覧(`storage`)→ 文字列の配列。
    StorageKeys,
}

/// HTTP の問い合わせ。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FetchRequest {
    /// `GET`・`POST`・`PUT`・`PATCH`・`DELETE`・`HEAD`。
    #[serde(default = "default_method")]
    pub method: String,
    /// `https://` で始まる URL(接続先はマニフェストの `hosts` のどれかに当てはまること)。
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// 本文(UTF-8 の文字列)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

fn default_method() -> String {
    "GET".into()
}

impl FetchRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: default_method(),
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
        }
    }

    /// JSON を本文にした POST。
    pub fn post_json(url: impl Into<String>, body: &Value) -> Self {
        let mut r = Self::get(url);
        r.method = "POST".into();
        r.headers
            .insert("content-type".into(), "application/json".into());
        r.body = Some(body.to_string());
        r
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.insert(name.to_ascii_lowercase(), value.into());
        self
    }
}

/// HTTP の応答。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FetchResponse {
    pub status: u16,
    /// ヘッダー(名前は小文字)。
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 本文(UTF-8 として読めない部分は置き換える)。
    #[serde(default)]
    pub body: String,
}

impl FetchResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// 本文を JSON として読む。
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_str(&self.body).map_err(|e| format!("応答の JSON が読めません: {e}"))
    }
}

/// 通信の記録(実行結果で、プラグインがどこに接続したかを見せる)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FetchRecord {
    pub method: String,
    pub url: String,
    /// 応答の状態コード(つながらなかったら `None`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

// ---- 実行する人の許可 ----

/// 実行する人が、あるプラグインに与えた許可。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginGrant {
    /// 許可した権限。
    #[serde(default)]
    pub allowed: Vec<Permission>,
    /// 許可した接続先(マニフェストの `hosts` のうち)。
    #[serde(default)]
    pub hosts: Vec<String>,
    /// 許可するかどうかを決めた権限(新しい版で増えた権限は、また確認する)。
    #[serde(default)]
    pub reviewed: Vec<Permission>,
    /// 許可するかどうかを決めた接続先。
    #[serde(default)]
    pub reviewed_hosts: Vec<String>,
    /// 最後に決めた時刻(UNIX ミリ秒)。まだ決めていなければ `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<u64>,
}

impl Default for PluginGrant {
    /// まだ何も決めていないとき: ボードの読み書きだけ許可済み。
    fn default() -> Self {
        let board = vec![Permission::BoardRead, Permission::BoardWrite];
        Self {
            allowed: board.clone(),
            hosts: Vec::new(),
            reviewed: board,
            reviewed_hosts: Vec::new(),
            updated: None,
        }
    }
}

impl PluginGrant {
    pub fn allows(&self, p: Permission) -> bool {
        self.allowed.contains(&p)
    }

    pub fn allows_host(&self, h: &str) -> bool {
        self.hosts.iter().any(|x| x == h)
    }

    /// まだ許可するかどうか決めていないもの(`policy` で使えないものは除く)。
    pub fn pending(&self, m: &PluginManifest, policy: &PolicyDenied) -> Pending {
        let permissions: Vec<Permission> = m
            .permissions
            .iter()
            .copied()
            .filter(|p| !self.reviewed.contains(p) && !policy.permissions.contains(p))
            .collect();
        let hosts =
            if m.can(Permission::Network) && !policy.permissions.contains(&Permission::Network) {
                m.hosts
                    .iter()
                    .filter(|h| !self.reviewed_hosts.contains(h) && !policy.hosts.contains(h))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
        Pending { permissions, hosts }
    }

    /// 決めた内容に置き換える(マニフェストにないものは捨てる)。
    pub fn decide(m: &PluginManifest, req: &GrantRequest, now: u64) -> Self {
        let allowed = m
            .permissions
            .iter()
            .copied()
            .filter(|p| req.allowed.contains(p))
            .collect();
        let hosts = m
            .hosts
            .iter()
            .filter(|h| req.hosts.contains(h))
            .cloned()
            .collect();
        let mut reviewed = m.permissions.clone();
        for p in [Permission::BoardRead, Permission::BoardWrite] {
            if !reviewed.contains(&p) {
                reviewed.push(p);
            }
        }
        Self {
            allowed,
            hosts,
            reviewed,
            reviewed_hosts: m.hosts.clone(),
            updated: Some(now),
        }
    }

    /// 実際に使える権限と接続先。
    pub fn effective(
        &self,
        m: &PluginManifest,
        policy: &PolicyDenied,
    ) -> (Vec<Permission>, Vec<String>) {
        let perms: Vec<Permission> = m
            .permissions
            .iter()
            .copied()
            .filter(|p| self.allows(*p) && !policy.permissions.contains(p))
            .collect();
        let hosts = if perms.contains(&Permission::Network) {
            m.hosts
                .iter()
                .filter(|h| self.allows_host(h) && !policy.hosts.contains(h))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        (perms, hosts)
    }
}

/// まだ許可するかどうか決めていないもの。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    #[serde(default)]
    pub permissions: Vec<Permission>,
    #[serde(default)]
    pub hosts: Vec<String>,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.permissions.is_empty() && self.hosts.is_empty()
    }
}

/// サーバーの管理者の設定で使えないもの。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PolicyDenied {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<Permission>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
}

/// 許可を決めるリクエスト(`PUT /api/v1/plugins/{id}/grant`)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GrantRequest {
    #[serde(default)]
    pub allowed: Vec<Permission>,
    #[serde(default)]
    pub hosts: Vec<String>,
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
    /// 自分が与えた許可(ログインしているとき)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<PluginGrant>,
    /// まだ許可するかどうか決めていないもの。空でなければ、決めるまで実行できない。
    #[serde(default)]
    pub pending: Pending,
    /// サーバーの設定で使えないもの。
    #[serde(default)]
    pub denied: PolicyDenied,
    /// 保存領域に使っている大きさ(バイト)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_bytes: Option<usize>,
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
    /// 外部への通信。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fetches: Vec<FetchRecord>,
}
