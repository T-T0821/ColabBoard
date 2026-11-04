//! グラボード の WASM プラグインを Rust で書くためのクレート。
//!
//! ```ignore
//! use graboard_plugin::prelude::*;
//!
//! fn manifest() -> PluginManifest {
//!     PluginManifest::new("hello", "こんにちは")
//!         .permission(Permission::BoardWrite)
//!         .command("hello", "挨拶の付箋を置く")
//! }
//!
//! fn run(ctx: &mut Context) -> Result<RunOutput, String> {
//!     let me = ctx.user().map(|u| u.name.clone()).unwrap_or_default();
//!     ctx.apply(vec![Op::Create { element: NewElement::sticky(0.0, 0.0, format!("{me} さん、こんにちは")) }])?;
//!     Ok(RunOutput::message("付箋を置きました"))
//! }
//!
//! graboard_plugin::plugin!(manifest, run);
//! ```
//!
//! `wasm32-unknown-unknown` 向けに `cdylib` としてビルドし、できた `.wasm` をサーバーに入れる。
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown --release
//! curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/wasm' \
//!   --data-binary @target/wasm32-unknown-unknown/release/hello.wasm $URL/api/v1/plugins
//! ```
//!
//! WASM 以外(普段の `cargo test`)では、ホスト関数の代わりに手元の `board_core::Board` を使う
//! 擬似ホストで動く([`testing`])。プラグインの処理をそのまま単体テストできる。

#[cfg(not(target_arch = "wasm32"))]
pub use board_core;
pub use board_types;
pub use board_types::ops::{
    ConnectorStyle, ElementBody, ElementData, ElementPatch, ElementQuery, EndpointData, MetaPatch,
    NewElement, Op, OpsResult, ShapeStyle,
};
pub use board_types::plugin::{
    ABI_VERSION, BoardRef, FetchRequest, FetchResponse, HostCall, Permission, PluginCommand,
    PluginManifest, Reply, RunInput, RunOutput, UserRef,
};
pub use serde_json;

/// よく使うものをまとめて読み込む。
pub mod prelude {
    pub use crate::{
        Context, ElementBody, ElementData, ElementPatch, ElementQuery, EndpointData, FetchRequest,
        FetchResponse, ManifestExt, NewElement, Op, OpsResult, Permission, PluginManifest,
        RunOutputExt, ShapeStyle,
    };
    pub use board_types::plugin::RunOutput;
}

/// マニフェストを組み立てる近道。
pub trait ManifestExt {
    fn new(id: &str, name: &str) -> Self;
    fn version(self, v: &str) -> Self;
    fn description(self, d: &str) -> Self;
    fn permission(self, p: Permission) -> Self;
    /// 接続先を足す(`network` の権限も付く)。`api.example.com`、`*.example.com` など。
    fn host(self, pattern: &str) -> Self;
    fn command(self, id: &str, title: &str) -> Self;
}

impl ManifestExt for PluginManifest {
    fn new(id: &str, name: &str) -> Self {
        PluginManifest {
            id: id.into(),
            name: name.into(),
            version: String::new(),
            description: String::new(),
            permissions: Vec::new(),
            hosts: Vec::new(),
            commands: Vec::new(),
            abi: ABI_VERSION,
        }
    }

    fn version(mut self, v: &str) -> Self {
        self.version = v.into();
        self
    }

    fn description(mut self, d: &str) -> Self {
        self.description = d.into();
        self
    }

    /// 権限を足す(`BoardWrite` を足すと `BoardRead` も足す)。
    fn permission(mut self, p: Permission) -> Self {
        let add = |m: &mut Self, p| {
            if !m.permissions.contains(&p) {
                m.permissions.push(p);
            }
        };
        if p == Permission::BoardWrite {
            add(&mut self, Permission::BoardRead);
        }
        add(&mut self, p);
        self
    }

    fn host(mut self, pattern: &str) -> Self {
        let pattern = pattern.to_ascii_lowercase();
        if !self.hosts.contains(&pattern) {
            self.hosts.push(pattern);
        }
        self.permission(Permission::Network)
    }

    fn command(mut self, id: &str, title: &str) -> Self {
        self.commands.push(PluginCommand {
            id: id.into(),
            title: title.into(),
            description: String::new(),
        });
        self
    }
}

/// 出力を作る近道。
pub trait RunOutputExt {
    fn message(msg: impl Into<String>) -> Self;
    fn select(self, ids: Vec<String>) -> Self;
}

impl RunOutputExt for RunOutput {
    fn message(msg: impl Into<String>) -> Self {
        RunOutput {
            message: Some(msg.into()),
            ..Default::default()
        }
    }

    fn select(mut self, ids: Vec<String>) -> Self {
        self.select = Some(ids);
        self
    }
}

/// 実行中のプラグインから見たボードと入力。
pub struct Context {
    input: RunInput,
}

impl Context {
    pub fn new(input: RunInput) -> Self {
        Self { input }
    }

    pub fn input(&self) -> &RunInput {
        &self.input
    }

    /// 実行するコマンドの ID。
    pub fn command(&self) -> &str {
        &self.input.command
    }

    pub fn args(&self) -> &serde_json::Value {
        &self.input.args
    }

    pub fn board(&self) -> &BoardRef {
        &self.input.board
    }

    pub fn user(&self) -> Option<&UserRef> {
        self.input.user.as_ref()
    }

    /// 選択中の要素の ID。
    pub fn selection(&self) -> &[String] {
        &self.input.selection
    }

    /// 書き込める(プラグインが `board:write` を持ち、実行した人が編集者以上)か。
    pub fn can_write(&self) -> bool {
        self.input.can_write
    }

    /// この実行で使える権限か(宣言していても、実行する人が許可していなければ `false`)。
    pub fn can(&self, p: Permission) -> bool {
        self.input.permissions.contains(&p)
    }

    /// この実行で接続してよい先。
    pub fn hosts(&self) -> &[String] {
        &self.input.hosts
    }

    /// HTTP で問い合わせる(`network`)。接続先はマニフェストの `hosts` に書き、
    /// 実行する人に許可されたものだけ。リダイレクトはたどらない。
    pub fn fetch(&mut self, req: FetchRequest) -> Result<FetchResponse, String> {
        host(&HostCall::Fetch(req))
    }

    /// `GET` して本文を JSON として読む(2xx 以外はエラー)。
    pub fn get_json<T: serde::de::DeserializeOwned>(&mut self, url: &str) -> Result<T, String> {
        let r = self.fetch(FetchRequest::get(url).header("accept", "application/json"))?;
        if !r.ok() {
            return Err(format!("{url} が {} を返しました", r.status));
        }
        r.json()
    }

    /// 保存領域から読む(`storage`)。なければ `None`。
    pub fn load<T: serde::de::DeserializeOwned>(&mut self, key: &str) -> Result<Option<T>, String> {
        let v: serde_json::Value = host(&HostCall::StorageGet { key: key.into() })?;
        if v.is_null() {
            return Ok(None);
        }
        serde_json::from_value(v)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    /// 保存領域に書く(`storage`)。実行が成功したときだけ残る。
    pub fn save<T: serde::Serialize>(&mut self, key: &str, value: &T) -> Result<(), String> {
        let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
        let _: serde_json::Value = host(&HostCall::StorageSet {
            key: key.into(),
            value,
        })?;
        Ok(())
    }

    /// 保存領域から消す(`storage`)。
    pub fn remove(&mut self, key: &str) -> Result<(), String> {
        let _: serde_json::Value = host(&HostCall::StorageDelete { key: key.into() })?;
        Ok(())
    }

    /// 保存領域のキー(`storage`)。
    pub fn keys(&mut self) -> Result<Vec<String>, String> {
        host(&HostCall::StorageKeys)
    }

    /// 要素を探す。
    pub fn query(&mut self, q: &ElementQuery) -> Result<Vec<ElementData>, String> {
        host(&HostCall::Query { query: q.clone() })
    }

    /// 全要素。
    pub fn elements(&mut self) -> Result<Vec<ElementData>, String> {
        self.query(&ElementQuery::default())
    }

    /// 選択中の要素(何も選んでいなければ空)。
    pub fn selected(&mut self) -> Result<Vec<ElementData>, String> {
        if self.input.selection.is_empty() {
            return Ok(Vec::new());
        }
        self.query(&ElementQuery {
            ids: Some(self.input.selection.clone()),
            ..Default::default()
        })
    }

    /// 要素を 1 つ取る。
    pub fn get(&mut self, id: &str) -> Result<Option<ElementData>, String> {
        host(&HostCall::Get { id: id.into() })
    }

    /// 操作を適用する(1 つでも失敗したら何も変わらない)。
    pub fn apply(&mut self, ops: Vec<Op>) -> Result<OpsResult, String> {
        host(&HostCall::Apply { ops })
    }

    /// ログに残す。
    pub fn log(&mut self, message: impl Into<String>) {
        let _: Result<serde_json::Value, String> = host(&HostCall::Log {
            message: message.into(),
        });
    }
}

/// ホスト関数を呼ぶ。
pub fn host<T: serde::de::DeserializeOwned>(call: &HostCall) -> Result<T, String> {
    let req = serde_json::to_vec(call).map_err(|e| e.to_string())?;
    let bytes = sys::call(&req);
    let reply: Reply<T> =
        serde_json::from_slice(&bytes).map_err(|e| format!("応答が読めません: {e}"))?;
    reply.into()
}

#[cfg(target_arch = "wasm32")]
mod sys {
    #[link(wasm_import_module = "graboard")]
    unsafe extern "C" {
        #[link_name = "call"]
        fn gb_call(ptr: *const u8, len: usize) -> i32;
        #[link_name = "response"]
        fn gb_response(dst: *mut u8);
    }

    pub fn call(req: &[u8]) -> Vec<u8> {
        // SAFETY: ホストは req の範囲だけを読み、len バイトを buf に書く。
        unsafe {
            let len = gb_call(req.as_ptr(), req.len()) as usize;
            let mut buf = vec![0u8; len];
            gb_response(buf.as_mut_ptr());
            buf
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod sys {
    pub fn call(req: &[u8]) -> Vec<u8> {
        crate::testing::call(req)
    }
}

/// 確保した領域をホストに渡すための、ABI の実装(マクロから使う)。
#[doc(hidden)]
pub mod abi {
    use super::*;

    pub fn alloc(len: i32) -> i32 {
        let mut v = Vec::<u8>::with_capacity(len.max(0) as usize);
        let p = v.as_mut_ptr();
        std::mem::forget(v);
        p as usize as i32
    }

    fn leak(bytes: Vec<u8>) -> i64 {
        let len = bytes.len() as u64;
        let ptr = Box::leak(bytes.into_boxed_slice()).as_ptr() as usize as u64;
        ((ptr << 32) | len) as i64
    }

    pub fn manifest(m: PluginManifest) -> i64 {
        leak(serde_json::to_vec(&m).unwrap_or_default())
    }

    /// # Safety
    /// `ptr` から `len` バイトはホストが書いた入力。
    pub unsafe fn run(ptr: i32, len: i32, f: fn(&mut Context) -> Result<RunOutput, String>) -> i64 {
        // SAFETY: gb_alloc で確保した領域にホストが書いたもの。
        let input =
            unsafe { Vec::from_raw_parts(ptr as usize as *mut u8, len as usize, len as usize) };
        let reply: Reply<RunOutput> = match serde_json::from_slice::<RunInput>(&input) {
            Ok(input) => f(&mut Context::new(input)).into(),
            Err(e) => Reply::Err(format!("入力が読めません: {e}")),
        };
        leak(serde_json::to_vec(&reply).unwrap_or_default())
    }
}

/// プラグインの入口(`gb_alloc`・`gb_manifest`・`gb_run`)を作る。
///
/// `plugin!(マニフェストを返す関数, 実行する関数)`。
#[macro_export]
macro_rules! plugin {
    ($manifest:path, $run:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn gb_alloc(len: i32) -> i32 {
            $crate::abi::alloc(len)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn gb_manifest() -> i64 {
            $crate::abi::manifest($manifest())
        }

        /// # Safety
        /// ホストからだけ呼ばれる。
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn gb_run(ptr: i32, len: i32) -> i64 {
            unsafe { $crate::abi::run(ptr, len, $run) }
        }
    };
}

/// WASM 以外で、手元のボードを相手にプラグインを動かす(単体テスト用)。
#[cfg(not(target_arch = "wasm32"))]
pub mod testing {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use board_core::Board;
    use board_core::plugin::{HostContext, Network};

    use super::*;

    thread_local! {
        static HOST: RefCell<Option<HostContext>> = const { RefCell::new(None) };
    }

    pub(crate) fn call(req: &[u8]) -> Vec<u8> {
        HOST.with(|h| {
            let mut h = h.borrow_mut();
            let reply: Reply<serde_json::Value> = match h.as_mut() {
                None => Reply::Err("testing::run の中で呼んでください".into()),
                Some(ctx) => match serde_json::from_slice::<HostCall>(req) {
                    Ok(c) => ctx.dispatch(c).into(),
                    Err(e) => Reply::Err(e.to_string()),
                },
            };
            serde_json::to_vec(&reply).unwrap_or_default()
        })
    }

    pub use board_core::plugin::{Fetcher, PluginStorage};

    /// 実行の結果。
    pub struct Outcome {
        pub result: Result<RunOutput, String>,
        pub board: Board,
        pub logs: Vec<String>,
        /// 実行後の保存領域(`storage` を使えたとき)。
        pub storage: BTreeMap<String, serde_json::Value>,
        /// 通信の記録。
        pub fetches: Vec<board_core::plugin::FetchRecord>,
    }

    /// 実行の環境(擬似ホストの設定)。
    #[derive(Default)]
    pub struct Env {
        /// 最初の保存領域。
        pub storage: BTreeMap<String, serde_json::Value>,
        /// 通信の代わりに呼ぶ関数(なければ、通信はエラーになる)。
        pub fetch: Option<Fetcher>,
        /// 実行する人が許可しなかった権限。
        pub deny: Vec<Permission>,
    }

    impl Env {
        /// 通信の代わりに `f` を呼ぶ。
        pub fn fetch(
            mut self,
            f: impl FnMut(&FetchRequest) -> Result<FetchResponse, String> + Send + 'static,
        ) -> Self {
            self.fetch = Some(Box::new(f));
            self
        }

        pub fn storage(mut self, key: &str, value: serde_json::Value) -> Self {
            self.storage.insert(key.into(), value);
            self
        }

        pub fn deny(mut self, p: Permission) -> Self {
            self.deny.push(p);
            self
        }
    }

    /// プラグインを `board` に対して実行する。権限はマニフェストのとおり(すべて許可する)。
    pub fn run(
        manifest: &PluginManifest,
        f: fn(&mut Context) -> Result<RunOutput, String>,
        board: Board,
        input: RunInput,
    ) -> Outcome {
        run_with(manifest, f, board, input, Env::default())
    }

    /// 保存領域・通信・許可しない権限を指定して実行する。
    /// 接続先の確かめ方(宣言した `hosts` だけ、https だけ)はサーバーと同じ。
    pub fn run_with(
        manifest: &PluginManifest,
        f: fn(&mut Context) -> Result<RunOutput, String>,
        board: Board,
        mut input: RunInput,
        env: Env,
    ) -> Outcome {
        let perms: Vec<Permission> = manifest
            .permissions
            .iter()
            .copied()
            .filter(|p| !env.deny.contains(p))
            .collect();
        let has = |p| perms.contains(&p);
        input.can_write = has(Permission::BoardWrite);
        input.hosts = if has(Permission::Network) {
            manifest.hosts.clone()
        } else {
            Vec::new()
        };
        let mut ctx = HostContext::new(
            board,
            has(Permission::BoardRead),
            has(Permission::BoardWrite),
        );
        if has(Permission::Storage) {
            ctx = ctx.with_storage(PluginStorage::new(env.storage));
        }
        if has(Permission::Network) {
            let fetcher = env.fetch.unwrap_or_else(|| {
                Box::new(|_: &FetchRequest| {
                    Err("testing::Env::fetch で通信の代わりを渡してください".to_string())
                })
            });
            ctx = ctx.with_network(Network::new(manifest.hosts.clone(), fetcher));
        }
        input.permissions = perms;
        HOST.with(|h| *h.borrow_mut() = Some(ctx));
        let result = f(&mut Context::new(input));
        let ctx = HOST.with(|h| h.borrow_mut().take()).expect("擬似ホスト");
        Outcome {
            result,
            board: ctx.board,
            logs: ctx.logs,
            storage: ctx.storage.map(|s| s.data).unwrap_or_default(),
            fetches: ctx.fetches,
        }
    }

    /// コマンドだけ指定した入力。
    pub fn input(command: &str) -> RunInput {
        RunInput {
            command: command.into(),
            board: BoardRef {
                id: "test".into(),
                title: "テスト".into(),
            },
            ..Default::default()
        }
    }
}
