//! WASM プラグインの実行環境と API。
//!
//! - プラグインは WebAssembly モジュール 1 つ。取り決めは [`board_core::plugin`]。
//! - [wasmi](https://github.com/wasmi-labs/wasmi)(Rust 製のインタープリター)で実行する。
//!   渡すのは `graboard.call` / `graboard.response` の 2 つのホスト関数だけで、WASI(ファイル・
//!   ネットワーク・時計など)はない。それ以外を読み込もうとするモジュールは実行できない。
//! - 命令数(燃料)とメモリに上限があり、超えたら止めて何も変えない。
//! - ボードへの書き込みは、プラグインのマニフェストが `board:write` を求め、かつ実行した人が
//!   編集者以上のときだけ。書き込みは使い捨てのボードに溜め、実行が成功したときだけ全員に配る。
//!
//! プラグインの置き場所は 2 つ。
//! - サーバーのプラグイン: 環境変数 `PLUGIN_DIR` のディレクトリにある `*.wasm`。全員が使える。
//! - ユーザーのプラグイン: `POST /api/v1/plugins` で各自が入れる。本人だけが使える
//!   (`USER_PLUGINS=0` で禁止できる)。

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use board_core::Board;
use board_core::api::Role;
use board_core::plugin::{
    BoardRef, HostCall, HostContext, Permission, PluginManifest, Reply, RunInput, RunOutput,
    UserRef,
};
use serde::{Deserialize, Serialize};
use wasmi::{
    Caller, Config as WasmConfig, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder,
};

use crate::AppState;
use crate::api::Error;
use crate::auth::now_ms;
use crate::rest::{self, Actor};
use crate::store::{PLUGIN_WASM, PLUGINS, StoreError};

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

/// プラグインまわりの設定。
#[derive(Clone)]
pub struct PluginConfig {
    pub host: Arc<PluginHost>,
    /// サーバーのプラグイン(全員が使える)。
    pub server: Vec<Arc<LoadedPlugin>>,
    /// ユーザーが自分でプラグインを入れてよいか。
    pub user_plugins: bool,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            host: Arc::new(PluginHost::new(Limits::default())),
            server: Vec::new(),
            user_plugins: true,
        }
    }
}

impl PluginConfig {
    /// 環境変数(`PLUGIN_DIR`、`USER_PLUGINS`)から作る。
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Ok(v) = std::env::var("USER_PLUGINS") {
            c.user_plugins = !matches!(v.trim(), "0" | "false" | "no" | "off");
        }
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
    pub fn run(
        &self,
        plugin: &LoadedPlugin,
        board: &mut Board,
        input: &RunInput,
    ) -> Result<RunResult, PluginError> {
        let can_read = plugin.manifest.can(Permission::BoardRead);
        let can_write = input.can_write && plugin.manifest.can(Permission::BoardWrite);
        let owned = std::mem::take(board);
        let mut store = self.store(HostContext::new(owned, can_read, can_write));
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
        Ok(RunResult {
            output,
            ops: state.ctx.ops_applied,
            logs: state.ctx.logs,
            fuel_used,
        })
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

fn server_info(p: &LoadedPlugin) -> PluginInfo {
    PluginInfo {
        manifest: p.manifest.clone(),
        source: "server".into(),
        installed: None,
        size: p.size,
    }
}

fn user_info(r: PluginRecord) -> PluginInfo {
    PluginInfo {
        manifest: r.manifest,
        source: "user".into(),
        installed: Some(r.installed),
        size: r.size,
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
    let mut out: Vec<PluginInfo> = state
        .config
        .plugins
        .server
        .iter()
        .map(|p| server_info(p))
        .collect();
    if let Some(login) = actor.login() {
        out.extend(user_plugins(&state, login)?.into_iter().map(user_info));
    }
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
    Ok(StatusCode::NO_CONTENT)
}

/// 実行のリクエスト。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RunRequest {
    pub command: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default)]
    pub selection: Vec<String>,
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
    let plugin = {
        let state = state.clone();
        let login = actor.login().map(str::to_string);
        tokio::task::spawn_blocking(move || find(&state, login.as_deref(), &pid))
            .await
            .map_err(|e| Error::from(StoreError::from(e.to_string())))??
    };
    if plugin.manifest.command(&req.command).is_none() {
        return Err(Error::bad_request(format!(
            "コマンド「{}」はありません",
            req.command
        )));
    }
    if plugin.manifest.can(Permission::BoardWrite) && !role.can_edit() {
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            "このプラグインはボードを編集するので、編集権限が必要です",
        ));
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
    };
    let hub = state.hub.clone();
    let host = state.config.plugins.host.clone();
    let pid = plugin.manifest.id.clone();
    let result = tokio::task::spawn_blocking(move || {
        hub.edit(&id, |board| {
            host.run(&plugin, board, &input).map_err(plugin_error)
        })
    })
    .await
    .map_err(|e| Error::from(StoreError::from(e.to_string())))??;
    tracing::info!(plugin = %pid, ops = result.ops, fuel = result.fuel_used, "プラグインを実行しました");
    Ok(Json(result))
}
