//! WASM プラグインの実行環境と API。
//!
//! - プラグインは WebAssembly モジュール 1 つ。取り決めは [`board_core::plugin`]。
//! - [wasmi](https://github.com/wasmi-labs/wasmi)(Rust 製のインタープリター)で実行する。
//!   渡すのは `graboard.call` / `graboard.response` の 2 つのホスト関数だけで、WASI(ファイル・
//!   ネットワーク・時計など)はない。それ以外を読み込もうとするモジュールは実行できない。
//! - 命令数(燃料)とメモリに上限があり、超えたら止めて何も変えない。
//! - 権限のサンドボックス: 使えるのは「マニフェストで宣言した権限」∩「実行する人の許可」∩
//!   「管理者の方針」だけ。ボードの読み書きは既定で許可済み。通信(`network`)と保存領域
//!   (`storage`)は、実行する人が許可する(`PUT /api/v1/plugins/{id}/grant`)まで実行できない。
//! - 通信はホストが代わりに行う。宣言した接続先(`hosts`)の HTTPS だけで、リダイレクトは
//!   たどらず、内部のアドレス(127.0.0.1、10.0.0.0/8、169.254.169.254 など)には接続しない。
//! - ボードへの書き込みは、`board:write` が使え、かつ実行した人が編集者以上のときだけ。
//!   書き込みと保存領域への書き込みは溜めておき、実行が成功したときだけ残す。
//!
//! プラグインの置き場所は 2 つ。
//! - サーバーのプラグイン: 環境変数 `PLUGIN_DIR` のディレクトリにある `*.wasm`。全員が使える。
//! - ユーザーのプラグイン: `POST /api/v1/plugins` で各自が入れる。本人だけが使える
//!   (`USER_PLUGINS=0` で禁止できる)。

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post, put};
use board_core::Board;
use board_core::api::Role;
use board_core::plugin::{
    BoardRef, FetchRequest, FetchResponse, Fetcher, GrantRequest, HostCall, HostContext, Network,
    Permission, PluginGrant, PluginManifest, PluginStorage, PolicyDenied, Reply, RunInput,
    RunOutput, UserRef, pattern_covered, url_target,
};
pub use board_core::plugin::{PluginInfo, RunRequest, RunResult};
use serde::{Deserialize, Serialize};
use wasmi::{
    Caller, Config as WasmConfig, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder,
};

use crate::AppState;
use crate::api::Error;
use crate::auth::now_ms;
use crate::rest::{self, Actor};
use crate::store::{PLUGIN_DATA, PLUGIN_GRANTS, PLUGIN_WASM, PLUGINS, StoreError};

/// モジュールの大きさの上限。
pub const MAX_WASM_BYTES: usize = 8 * 1024 * 1024;
/// 1 人が入れられるプラグインの数。
pub const MAX_PLUGINS_PER_USER: usize = 30;

/// 実行の上限。
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// 命令数の目安(wasmi の燃料)。
    pub fuel: u64,
    /// 線形メモリの上限(バイト)。
    pub memory_bytes: usize,
    /// ホスト関数を呼べる回数。
    pub host_calls: usize,
    /// 1 回の呼び出し・応答の大きさ。
    pub message_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel: 2_000_000_000,
            memory_bytes: 64 * 1024 * 1024,
            host_calls: 10_000,
            message_bytes: 16 * 1024 * 1024,
        }
    }
}

/// プラグインの通信についての管理者の方針。
#[derive(Clone)]
pub struct NetPolicy {
    /// 通信を許すか(`PLUGIN_NETWORK=0` で全部止める)。
    pub enabled: bool,
    /// 接続してよい先の上限(`PLUGIN_NETWORK_HOSTS`)。`None` なら、実行する人が許可した先ならどこでも。
    pub allow: Option<Vec<String>>,
    /// http:// と内部のアドレスへの接続も許す(開発・テスト用。`PLUGIN_NETWORK_INSECURE=1`)。
    pub insecure: bool,
    /// 1 回の問い合わせの待ち時間。
    pub timeout: Duration,
    /// 1 回の実行で通信に使える時間の合計。
    pub budget: Duration,
    /// 応答の本文の大きさ。
    pub max_response: usize,
    http: reqwest::Client,
}

impl Default for NetPolicy {
    fn default() -> Self {
        Self::new(true, None, false)
    }
}

impl NetPolicy {
    pub fn new(enabled: bool, allow: Option<Vec<String>>, insecure: bool) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(PublicOnly { insecure })
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap_or_default();
        Self {
            enabled,
            allow,
            insecure,
            timeout: Duration::from_secs(10),
            budget: Duration::from_secs(30),
            max_response: 4 * 1024 * 1024,
            http,
        }
    }

    /// この方針で接続してよい宣言か。
    pub fn permits(&self, pattern: &str) -> bool {
        self.enabled
            && self
                .allow
                .as_ref()
                .is_none_or(|list| list.iter().any(|p| pattern_covered(p, pattern)))
    }

    /// 1 回の実行のための通信の窓口。
    fn fetcher(&self, handle: tokio::runtime::Handle) -> Fetcher {
        let http = self.http.clone();
        let (insecure, timeout, max) = (self.insecure, self.timeout, self.max_response);
        let deadline = Instant::now() + self.budget;
        Box::new(move |req: &FetchRequest| {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("1 回の実行で通信に使える時間を使い切りました".into());
            }
            handle.block_on(fetch(&http, req, insecure, timeout.min(left), max))
        })
    }
}

/// 公開されたアドレスか(内部のネットワークやクラウドのメタデータには接続させない)。
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // fe80::/10
                || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32
                || (s[0] == 0x0064 && s[1] == 0xff9b) // 64:ff9b::/96(NAT64)
                || (s[0] == 0x2002)) // 6to4
        }
    }
}

/// 名前を引き、公開されたアドレスだけを返す(DNS で内部のアドレスに向けられても接続しない)。
struct PublicOnly {
    insecure: bool,
}

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let insecure = self.insecure;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| insecure || is_public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(
                    format!("{host} は内部のアドレスを指しているので接続できません").into(),
                );
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

async fn fetch(
    http: &reqwest::Client,
    req: &FetchRequest,
    insecure: bool,
    timeout: Duration,
    max: usize,
) -> Result<FetchResponse, String> {
    let target = url_target(&req.url)?;
    let bare = target
        .host
        .rsplit_once(':')
        .map_or(target.host.as_str(), |(h, _)| h);
    if !insecure {
        if target.scheme != "https" {
            return Err("https:// にだけ接続できます".into());
        }
        if let Ok(ip) = bare.parse::<IpAddr>()
            && !is_public(ip)
        {
            return Err(format!("{bare} は内部のアドレスなので接続できません"));
        }
    }
    // 実際に接続する先が、確かめた先と同じであること(URL の読み方の違いで別の先に行かせない)。
    let url = reqwest::Url::parse(&req.url).map_err(|e| format!("URL が読めません: {e}"))?;
    let actual = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{}:{p}", h.trim_end_matches('.')),
        (Some(h), None) => h.trim_end_matches('.').to_string(),
        _ => String::new(),
    };
    if actual != target.host || url.scheme() != target.scheme {
        return Err(format!("URL「{}」の接続先が読めません", req.url));
    }
    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .map_err(|_| format!("メソッド {} は使えません", req.method))?;
    let mut b = http.request(method, url).timeout(timeout);
    for (k, v) in &req.headers {
        b = b.header(k.as_str(), v.as_str());
    }
    if !req.headers.contains_key("user-agent") {
        b = b.header("user-agent", "graboard-plugin/1");
    }
    if let Some(body) = &req.body {
        b = b.body(body.clone());
    }
    let mut res = b.send().await.map_err(|e| {
        let e = e.without_url();
        format!("{} に接続できません: {e}", target.host)
    })?;
    let status = res.status().as_u16();
    let headers: BTreeMap<String, String> = res
        .headers()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    let mut body = Vec::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| format!("応答を読めません: {}", e.without_url()))?
    {
        if body.len() + chunk.len() > max {
            return Err(format!("応答が大きすぎます({} MB まで)", max / 1024 / 1024));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(FetchResponse {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// プラグインまわりの設定。
#[derive(Clone)]
pub struct PluginConfig {
    pub host: Arc<PluginHost>,
    /// サーバーのプラグイン(全員が使える)。
    pub server: Vec<Arc<LoadedPlugin>>,
    /// ユーザーが自分でプラグインを入れてよいか。
    pub user_plugins: bool,
    /// 通信の方針。
    pub network: NetPolicy,
    /// 保存領域を使わせるか(`PLUGIN_STORAGE=0` で止める)。
    pub storage: bool,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            host: Arc::new(PluginHost::new(Limits::default())),
            server: Vec::new(),
            user_plugins: true,
            network: NetPolicy::default(),
            storage: true,
        }
    }
}

fn env_flag(name: &str) -> Option<bool> {
    std::env::var(name)
        .ok()
        .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off"))
}

impl PluginConfig {
    /// サーバーの設定で使えないもの。
    pub fn denied(&self, m: &PluginManifest) -> PolicyDenied {
        let mut d = PolicyDenied::default();
        if m.can(Permission::Network) && !self.network.enabled {
            d.permissions.push(Permission::Network);
        } else if m.can(Permission::Network) {
            d.hosts = m
                .hosts
                .iter()
                .filter(|h| !self.network.permits(h))
                .cloned()
                .collect();
        }
        if m.can(Permission::Storage) && !self.storage {
            d.permissions.push(Permission::Storage);
        }
        d
    }

    /// 環境変数(`PLUGIN_DIR`、`USER_PLUGINS`、`PLUGIN_NETWORK*`、`PLUGIN_STORAGE`)から作る。
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Some(v) = env_flag("USER_PLUGINS") {
            c.user_plugins = v;
        }
        if let Some(v) = env_flag("PLUGIN_STORAGE") {
            c.storage = v;
        }
        let allow = std::env::var("PLUGIN_NETWORK_HOSTS").ok().map(|v| {
            v.split([',', ' '])
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect::<Vec<_>>()
        });
        c.network = NetPolicy::new(
            env_flag("PLUGIN_NETWORK").unwrap_or(true),
            allow,
            env_flag("PLUGIN_NETWORK_INSECURE").unwrap_or(false),
        );
        if let Ok(dir) = std::env::var("PLUGIN_DIR")
            && !dir.trim().is_empty()
        {
            c.server = c.host.load_dir(Path::new(dir.trim()));
        }
        c
    }
}

/// 読み込んだプラグイン。
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub module: Module,
    pub size: usize,
}

/// 実行の失敗。
#[derive(Debug, PartialEq)]
pub enum PluginError {
    /// モジュールが読めない・取り決めに合わない。
    Invalid(String),
    /// 実行中に止まった(上限超過・プラグイン自身のエラーなど)。
    Failed(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::Invalid(m) => write!(f, "プラグインを読み込めません: {m}"),
            PluginError::Failed(m) => write!(f, "プラグインの実行に失敗しました: {m}"),
        }
    }
}

/// 実行ごとに渡す、権限で守られた資源。
#[derive(Default)]
pub struct Sandbox {
    /// 保存領域(`storage` が使えるとき)。
    pub storage: Option<PluginStorage>,
    /// 通信の窓口(`network` が使えるとき)。
    pub network: Option<Network>,
}

/// サンドボックスの中の状態。
struct HostState {
    ctx: HostContext,
    limits: StoreLimits,
    max_message: usize,
    calls_left: usize,
    /// 直前の `call` の応答。
    response: Vec<u8>,
}

/// wasmi のエンジン(コンパイル済みコードの置き場)。
pub struct PluginHost {
    engine: Engine,
    limits: Limits,
}

fn invalid(e: impl std::fmt::Display) -> PluginError {
    PluginError::Invalid(e.to_string())
}

fn failed(e: impl std::fmt::Display) -> PluginError {
    PluginError::Failed(e.to_string())
}

/// `ptr << 32 | len` を分ける。
fn unpack(v: i64) -> (usize, usize) {
    let v = v as u64;
    ((v >> 32) as usize, (v & 0xffff_ffff) as usize)
}

impl PluginHost {
    pub fn new(limits: Limits) -> Self {
        let mut config = WasmConfig::default();
        config.consume_fuel(true);
        Self {
            engine: Engine::new(&config),
            limits,
        }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// モジュールを読み込み、マニフェストを取り出して確かめる。
    pub fn load(&self, wasm: &[u8]) -> Result<LoadedPlugin, PluginError> {
        if wasm.len() > MAX_WASM_BYTES {
            return Err(invalid(format!(
                "モジュールは {} MB までです",
                MAX_WASM_BYTES / 1024 / 1024
            )));
        }
        let module = Module::new(&self.engine, wasm).map_err(invalid)?;
        let mut store = self.store(HostContext::new(Board::new(), false, false));
        let instance = self.instantiate(&mut store, &module)?;
        let f = instance
            .get_typed_func::<(), i64>(&store, "gb_manifest")
            .map_err(|_| invalid("gb_manifest がありません"))?;
        let packed = f.call(&mut store, ()).map_err(failed)?;
        let bytes = read_guest(&store, &instance, packed, self.limits.message_bytes)?;
        let manifest: PluginManifest = serde_json::from_slice(&bytes)
            .map_err(|e| invalid(format!("マニフェストが読めません: {e}")))?;
        manifest.validate().map_err(PluginError::Invalid)?;
        Ok(LoadedPlugin {
            manifest,
            module,
            size: wasm.len(),
        })
    }

    /// ディレクトリの `*.wasm` を全部読む(読めないものはログに出して飛ばす)。
    pub fn load_dir(&self, dir: &Path) -> Vec<Arc<LoadedPlugin>> {
        let mut out: Vec<Arc<LoadedPlugin>> = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            tracing::warn!(dir = %dir.display(), "PLUGIN_DIR を読めません");
            return out;
        };
        let mut paths: Vec<_> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wasm"))
            .collect();
        paths.sort();
        for p in paths {
            let loaded = std::fs::read(&p)
                .map_err(invalid)
                .and_then(|b| self.load(&b));
            match loaded {
                Ok(l) if out.iter().any(|o| o.manifest.id == l.manifest.id) => {
                    tracing::warn!(path = %p.display(), id = %l.manifest.id, "同じ ID のプラグインがあるので飛ばします");
                }
                Ok(l) => {
                    tracing::info!(id = %l.manifest.id, name = %l.manifest.name, "プラグインを読み込みました");
                    out.push(Arc::new(l));
                }
                Err(e) => tracing::warn!(path = %p.display(), "{e}"),
            }
        }
        out
    }

    fn store(&self, ctx: HostContext) -> Store<HostState> {
        let state = HostState {
            ctx,
            limits: StoreLimitsBuilder::new()
                .memory_size(self.limits.memory_bytes)
                .instances(1)
                .memories(1)
                .tables(4)
                .trap_on_grow_failure(true)
                .build(),
            max_message: self.limits.message_bytes,
            calls_left: self.limits.host_calls,
            response: Vec::new(),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        let _ = store.set_fuel(self.limits.fuel);
        store
    }

    fn instantiate(
        &self,
        store: &mut Store<HostState>,
        module: &Module,
    ) -> Result<wasmi::Instance, PluginError> {
        let mut linker = <Linker<HostState>>::new(&self.engine);
        linker
            .func_wrap("graboard", "call", host_call)
            .map_err(invalid)?
            .func_wrap("graboard", "response", host_response)
            .map_err(invalid)?;
        linker.instantiate_and_start(store, module).map_err(|e| {
            invalid(format!(
                "{e}(使えるのは graboard.call と graboard.response だけです)"
            ))
        })
    }

    /// コマンドを実行する。`board` への変更は成功したときだけ残る(失敗したら呼び出し側で捨てる)。
    ///
    /// 使える権限は `input.permissions`(宣言 ∩ 許可 ∩ 方針を計算済みのもの)。保存領域と通信は
    /// `sandbox` で渡したときだけ使える。成功したら、変わった保存領域も返す。
    pub fn run(
        &self,
        plugin: &LoadedPlugin,
        board: &mut Board,
        input: &RunInput,
        sandbox: Sandbox,
    ) -> Result<(RunResult, Option<PluginStorage>), PluginError> {
        let has = |p| input.permissions.contains(&p) && plugin.manifest.can(p);
        let can_read = has(Permission::BoardRead);
        let can_write = input.can_write && has(Permission::BoardWrite);
        let owned = std::mem::take(board);
        let mut ctx = HostContext::new(owned, can_read, can_write);
        if has(Permission::Storage) {
            ctx.storage = sandbox.storage;
        }
        if has(Permission::Network) {
            ctx.network = sandbox.network;
        }
        let mut store = self.store(ctx);
        let instance = self.instantiate(&mut store, &plugin.module)?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&store, "gb_alloc")
            .map_err(|_| invalid("gb_alloc がありません"))?;
        let run = instance
            .get_typed_func::<(i32, i32), i64>(&store, "gb_run")
            .map_err(|_| invalid("gb_run がありません"))?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| invalid("memory がありません"))?;

        let input_bytes = serde_json::to_vec(input).map_err(failed)?;
        let ptr = alloc
            .call(&mut store, input_bytes.len() as i32)
            .map_err(trap)?;
        memory
            .write(&mut store, ptr as u32 as usize, &input_bytes)
            .map_err(|_| failed("入力を書き込めません"))?;
        let packed = run
            .call(&mut store, (ptr, input_bytes.len() as i32))
            .map_err(trap)?;
        let out = read_guest(&store, &instance, packed, self.limits.message_bytes)?;
        let reply: Reply<RunOutput> =
            serde_json::from_slice(&out).map_err(|e| failed(format!("出力が読めません: {e}")))?;
        let fuel_used = self.limits.fuel - store.get_fuel().unwrap_or(0);
        let state = store.into_data();
        *board = state.ctx.board;
        let output = Result::<RunOutput, String>::from(reply).map_err(PluginError::Failed)?;
        Ok((
            RunResult {
                output,
                ops: state.ctx.ops_applied,
                logs: state.ctx.logs,
                fuel_used,
                fetches: state.ctx.fetches,
            },
            state.ctx.storage.filter(|s| s.changed),
        ))
    }
}

/// 実行時エラーをわかりやすくする。
fn trap(e: wasmi::Error) -> PluginError {
    use wasmi::TrapCode;
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => failed("命令数の上限を超えました(無限ループかもしれません)"),
        Some(TrapCode::GrowthOperationLimited) => failed("メモリの上限を超えました"),
        _ => failed(e),
    }
}

/// プラグインのメモリから `ptr << 32 | len` の範囲を読む。
fn read_guest(
    store: &Store<HostState>,
    instance: &wasmi::Instance,
    packed: i64,
    max: usize,
) -> Result<Vec<u8>, PluginError> {
    let (ptr, len) = unpack(packed);
    if len > max {
        return Err(failed("出力が大きすぎます"));
    }
    let memory = instance
        .get_memory(store, "memory")
        .ok_or_else(|| invalid("memory がありません"))?;
    let data = memory.data(store);
    data.get(ptr..ptr.saturating_add(len))
        .map(<[u8]>::to_vec)
        .ok_or_else(|| failed("メモリの範囲外を指しています"))
}

/// `graboard.call(ptr, len) -> i32`
fn host_call(mut caller: Caller<'_, HostState>, ptr: i32, len: i32) -> Result<i32, wasmi::Error> {
    let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
    let state = caller.data();
    if state.calls_left == 0 {
        return Err(wasmi::Error::new(
            "ホスト関数の呼び出し回数の上限を超えました",
        ));
    }
    if len > state.max_message {
        return Err(wasmi::Error::new("呼び出しが大きすぎます"));
    }
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmi::Error::new("memory がありません"))?;
    let req = memory
        .data(&caller)
        .get(ptr..ptr.saturating_add(len))
        .ok_or_else(|| wasmi::Error::new("メモリの範囲外を指しています"))?
        .to_vec();
    let state = caller.data_mut();
    state.calls_left -= 1;
    let reply: Reply<serde_json::Value> = match serde_json::from_slice::<HostCall>(&req) {
        Ok(call) => state.ctx.dispatch(call).into(),
        Err(e) => Reply::Err(format!("呼び出しが読めません: {e}")),
    };
    let bytes = serde_json::to_vec(&reply).map_err(|e| wasmi::Error::new(e.to_string()))?;
    if bytes.len() > state.max_message {
        state.response = serde_json::to_vec(&Reply::<()>::Err(
            "応答が大きすぎます。query の limit や bbox で絞ってください".into(),
        ))
        .unwrap_or_default();
    } else {
        state.response = bytes;
    }
    Ok(state.response.len() as i32)
}

/// `graboard.response(dst)`
fn host_response(mut caller: Caller<'_, HostState>, dst: i32) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmi::Error::new("memory がありません"))?;
    let bytes = std::mem::take(&mut caller.data_mut().response);
    memory
        .write(&mut caller, dst as u32 as usize, &bytes)
        .map_err(|_| wasmi::Error::new("応答を書き込めません(メモリの範囲外)"))
}

// ---- 保存 ----

/// ユーザーが入れたプラグインの情報。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginRecord {
    pub manifest: PluginManifest,
    pub installed: u64,
    pub size: usize,
}

fn server_info(p: &LoadedPlugin) -> PluginInfo {
    PluginInfo {
        manifest: p.manifest.clone(),
        source: "server".into(),
        installed: None,
        size: p.size,
        grant: None,
        pending: Default::default(),
        denied: Default::default(),
        storage_bytes: None,
    }
}

fn user_info(r: PluginRecord) -> PluginInfo {
    PluginInfo {
        manifest: r.manifest,
        source: "user".into(),
        installed: Some(r.installed),
        size: r.size,
        grant: None,
        pending: Default::default(),
        denied: Default::default(),
        storage_bytes: None,
    }
}

/// 一覧に、この人の許可・まだ決めていないもの・管理者の方針を足す。
fn with_grant(
    state: &AppState,
    login: Option<&str>,
    mut info: PluginInfo,
) -> ApiResult<PluginInfo> {
    let m = &info.manifest;
    info.denied = state.config.plugins.denied(m);
    let grant = match login {
        Some(login) => load_grant(state, login, &m.id)?,
        None => PluginGrant::default(),
    };
    info.pending = grant.pending(m, &info.denied);
    if let Some(login) = login {
        if m.can(Permission::Storage) {
            info.storage_bytes = Some(
                state
                    .store()
                    .get_pair_bytes(PLUGIN_DATA, login, &m.id)?
                    .map_or(0, |b| b.len()),
            );
        }
        info.grant = Some(grant);
    }
    Ok(info)
}

fn load_grant(state: &AppState, login: &str, pid: &str) -> Result<PluginGrant, StoreError> {
    Ok(
        match state.store().get_pair_bytes(PLUGIN_GRANTS, login, pid)? {
            Some(b) => serde_json::from_slice(&b)?,
            None => PluginGrant::default(),
        },
    )
}

fn load_storage(state: &AppState, login: &str, pid: &str) -> Result<PluginStorage, StoreError> {
    let data: BTreeMap<String, serde_json::Value> =
        match state.store().get_pair_bytes(PLUGIN_DATA, login, pid)? {
            Some(b) => serde_json::from_slice(&b)?,
            None => BTreeMap::new(),
        };
    Ok(PluginStorage::new(data))
}

fn save_storage(
    state: &AppState,
    login: &str,
    pid: &str,
    storage: &PluginStorage,
) -> Result<(), StoreError> {
    if storage.data.is_empty() {
        state.store().remove_pair(PLUGIN_DATA, login, pid)
    } else {
        state
            .store()
            .put_pair(PLUGIN_DATA, login, pid, &storage.data)
    }
}

fn user_plugins(state: &AppState, login: &str) -> Result<Vec<PluginRecord>, StoreError> {
    Ok(state
        .store()
        .scan_pair::<PluginRecord>(PLUGINS, login)?
        .into_iter()
        .map(|(_, r)| r)
        .collect())
}

// ---- API ----

/// `/api/v1` の下に足すルート。
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/plugins",
            get(list)
                .post(install)
                .layer(DefaultBodyLimit::max(MAX_WASM_BYTES + 1024)),
        )
        .route("/plugins/{pid}", delete(uninstall))
        .route("/plugins/{pid}/grant", put(set_grant).delete(reset_grant))
        .route("/plugins/{pid}/storage", delete(clear_storage))
        .route("/boards/{id}/plugins/{pid}/run", post(run_plugin))
}

type ApiResult<T> = Result<T, Error>;

fn plugin_error(e: PluginError) -> Error {
    Error::bad_request(e.to_string())
}

fn not_found() -> Error {
    Error::new(StatusCode::NOT_FOUND, "プラグインが見つかりません")
}

async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<PluginInfo>>> {
    let actor = rest::actor(&state, &headers)?;
    let login = actor.login();
    let mut out: Vec<PluginInfo> = state
        .config
        .plugins
        .server
        .iter()
        .map(|p| server_info(p))
        .collect();
    if let Some(login) = login {
        out.extend(user_plugins(&state, login)?.into_iter().map(user_info));
    }
    let out = out
        .into_iter()
        .map(|i| with_grant(&state, login, i))
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(out))
}

/// 自分のプラグインを管理してよいか(ボードを限らない書き込みトークンか、ログイン)。
fn manager(state: &AppState, actor: &Actor) -> ApiResult<String> {
    if !state.config.plugins.user_plugins {
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            "このサーバーでは自分でプラグインを入れることはできません",
        ));
    }
    let login = actor.login().ok_or_else(Error::unauthorized)?.to_string();
    if let Some(t) = actor.token()
        && (t.boards.is_some() || t.scope.max_role() < Role::Editor)
    {
        return Err(Error::forbidden());
    }
    Ok(login)
}

async fn install(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<PluginInfo>)> {
    let actor = rest::actor(&state, &headers)?;
    let login = manager(&state, &actor)?;
    let host = state.config.plugins.host.clone();
    let wasm = body.to_vec();
    let (loaded, wasm) = tokio::task::spawn_blocking(move || host.load(&wasm).map(|l| (l, wasm)))
        .await
        .map_err(|e| Error::from(StoreError::from(e.to_string())))?
        .map_err(plugin_error)?;
    let id = loaded.manifest.id.clone();
    if state
        .config
        .plugins
        .server
        .iter()
        .any(|p| p.manifest.id == id)
    {
        return Err(Error::bad_request(format!(
            "「{id}」はサーバーのプラグインと同じ ID です。別の ID にしてください"
        )));
    }
    let existing = user_plugins(&state, &login)?;
    if !existing.iter().any(|r| r.manifest.id == id) && existing.len() >= MAX_PLUGINS_PER_USER {
        return Err(Error::bad_request(format!(
            "プラグインは {MAX_PLUGINS_PER_USER} 個までです"
        )));
    }
    let rec = PluginRecord {
        manifest: loaded.manifest,
        installed: now_ms(),
        size: loaded.size,
    };
    state
        .store()
        .put_pair_bytes(PLUGIN_WASM, &login, &id, &wasm)?;
    state.store().put_pair(PLUGINS, &login, &id, &rec)?;
    tracing::info!(login = %login, plugin = %id, bytes = rec.size, "プラグインを入れました");
    Ok((StatusCode::CREATED, Json(user_info(rec))))
}

async fn uninstall(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath(pid): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let actor = rest::actor(&state, &headers)?;
    let login = manager(&state, &actor)?;
    if !user_plugins(&state, &login)?
        .iter()
        .any(|r| r.manifest.id == pid)
    {
        return Err(not_found());
    }
    state.store().remove_pair(PLUGINS, &login, &pid)?;
    state.store().remove_pair(PLUGIN_WASM, &login, &pid)?;
    state.store().remove_pair(PLUGIN_GRANTS, &login, &pid)?;
    state.store().remove_pair(PLUGIN_DATA, &login, &pid)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 許可を決める・変える本人(ログインか、ボードを限らない書き込みトークン)。
fn grantor(actor: &Actor) -> ApiResult<String> {
    let login = actor.login().ok_or_else(Error::unauthorized)?.to_string();
    if let Some(t) = actor.token()
        && (t.boards.is_some() || t.scope.max_role() < Role::Editor)
    {
        return Err(Error::forbidden());
    }
    Ok(login)
}

async fn find_blocking(
    state: &Arc<AppState>,
    login: Option<&str>,
    pid: &str,
) -> ApiResult<Arc<LoadedPlugin>> {
    let state = state.clone();
    let login = login.map(str::to_string);
    let pid = pid.to_string();
    tokio::task::spawn_blocking(move || find(&state, login.as_deref(), &pid))
        .await
        .map_err(|e| Error::from(StoreError::from(e.to_string())))?
}

/// 許可を決める(求められている権限・接続先のうち、`allowed` / `hosts` にあるものを許可する)。
async fn set_grant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath(pid): UrlPath<String>,
    Json(req): Json<GrantRequest>,
) -> ApiResult<Json<PluginGrant>> {
    let actor = rest::actor(&state, &headers)?;
    let login = grantor(&actor)?;
    let plugin = find_blocking(&state, Some(&login), &pid).await?;
    let grant = PluginGrant::decide(&plugin.manifest, &req, now_ms());
    state
        .store()
        .put_pair(PLUGIN_GRANTS, &login, &plugin.manifest.id, &grant)?;
    if !grant.allows(Permission::Storage) {
        // 保存を許可しないなら、残っているデータも消す。
        state
            .store()
            .remove_pair(PLUGIN_DATA, &login, &plugin.manifest.id)?;
    }
    tracing::info!(login = %login, plugin = %plugin.manifest.id,
        allowed = ?grant.allowed.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        hosts = ?grant.hosts, "プラグインの許可を変えました");
    Ok(Json(grant))
}

/// 許可を最初の状態(ボードの読み書きだけ)に戻す。次の実行でまた確認する。
async fn reset_grant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath(pid): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let actor = rest::actor(&state, &headers)?;
    let login = grantor(&actor)?;
    state.store().remove_pair(PLUGIN_GRANTS, &login, &pid)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 保存領域を空にする。
async fn clear_storage(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath(pid): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let actor = rest::actor(&state, &headers)?;
    let login = grantor(&actor)?;
    state.store().remove_pair(PLUGIN_DATA, &login, &pid)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 実行するプラグインを探す(自分のものが先)。
fn find(state: &AppState, login: Option<&str>, pid: &str) -> ApiResult<Arc<LoadedPlugin>> {
    if let Some(login) = login
        && state.config.plugins.user_plugins
        && let Some(wasm) = state.store().get_pair_bytes(PLUGIN_WASM, login, pid)?
    {
        return state
            .config
            .plugins
            .host
            .load(&wasm)
            .map(Arc::new)
            .map_err(plugin_error);
    }
    state
        .config
        .plugins
        .server
        .iter()
        .find(|p| p.manifest.id == pid)
        .cloned()
        .ok_or_else(not_found)
}

async fn run_plugin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath((id, pid)): UrlPath<(String, String)>,
    Query(q): Query<rest::KeyQuery>,
    Json(req): Json<RunRequest>,
) -> ApiResult<Json<RunResult>> {
    let actor = rest::actor(&state, &headers)?;
    let (meta, role) = rest::access(&state, &actor, &id, q.key.as_deref())?;
    let login = actor.login().map(str::to_string);
    let plugin = find_blocking(&state, login.as_deref(), &pid).await?;
    let m = &plugin.manifest;
    if m.command(&req.command).is_none() {
        return Err(Error::bad_request(format!(
            "コマンド「{}」はありません",
            req.command
        )));
    }
    // 権限: 宣言 ∩ 実行する人の許可 ∩ 管理者の方針。
    let denied = state.config.plugins.denied(m);
    let grant = match &login {
        Some(l) => load_grant(&state, l, &m.id)?,
        None => PluginGrant::default(),
    };
    let pending = grant.pending(m, &denied);
    if !pending.is_empty() {
        let mut what: Vec<String> = pending
            .permissions
            .iter()
            .map(|p| format!("「{}」", p.label()))
            .collect();
        what.extend(pending.hosts.iter().map(|h| format!("{h} への通信")));
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            if login.is_some() {
                format!(
                    "このプラグインを実行するには、{} を許可するかどうか先に決めてください",
                    what.join("・")
                )
            } else {
                format!(
                    "このプラグインは {} を求めているので、ログインして許可してから実行してください",
                    what.join("・")
                )
            },
        ));
    }
    let (permissions, hosts) = grant.effective(m, &denied);
    if permissions.contains(&Permission::BoardWrite) && !role.can_edit() {
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            "このプラグインはボードを編集するので、編集権限が必要です",
        ));
    }
    let mut sandbox = Sandbox::default();
    if let Some(l) = &login
        && permissions.contains(&Permission::Storage)
    {
        sandbox.storage = Some(load_storage(&state, l, &m.id)?);
    }
    if permissions.contains(&Permission::Network) && !hosts.is_empty() {
        let policy = &state.config.plugins.network;
        let mut net = Network::new(
            hosts.clone(),
            policy.fetcher(tokio::runtime::Handle::current()),
        );
        net.allow_http = policy.insecure;
        sandbox.network = Some(net);
    }
    let input = RunInput {
        command: req.command,
        args: req.args,
        board: BoardRef {
            title: meta
                .as_ref()
                .map_or_else(|| id.clone(), |m| m.title.clone()),
            id: id.clone(),
        },
        user: actor.user().map(|u| UserRef {
            login: u.login.clone(),
            name: u.name.clone(),
        }),
        selection: req.selection,
        can_write: role.can_edit(),
        permissions,
        hosts,
    };
    let hub = state.hub.clone();
    let host = state.config.plugins.host.clone();
    let pid = plugin.manifest.id.clone();
    let (result, storage) = tokio::task::spawn_blocking(move || {
        hub.edit(&id, |board| {
            host.run(&plugin, board, &input, sandbox)
                .map_err(plugin_error)
        })
    })
    .await
    .map_err(|e| Error::from(StoreError::from(e.to_string())))??;
    if let (Some(l), Some(st)) = (&login, &storage) {
        save_storage(&state, l, &pid, st)?;
    }
    tracing::info!(plugin = %pid, ops = result.ops, fuel = result.fuel_used,
        fetches = result.fetches.len(), "プラグインを実行しました");
    Ok(Json(result))
}
