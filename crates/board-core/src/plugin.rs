//! WASM プラグインの取り決め。型は [`board_types::plugin`] にあり、ここではホスト関数の処理
//! ([`HostContext`])を持つ。サーバーと、プラグインのテスト用の擬似ホストで共有する。

pub use board_types::plugin::*;

use std::collections::BTreeMap;

use serde_json::Value;

use crate::board::Board;
use crate::ops;

/// 外部への問い合わせを実際に行う関数(サーバーは reqwest、テストは作り物)。
pub type Fetcher = Box<dyn FnMut(&FetchRequest) -> Result<FetchResponse, String> + Send>;

/// `network` の権限があるときの通信の窓口。
pub struct Network {
    /// 接続してよい先(マニフェストの宣言のうち、許可されたもの)。
    pub hosts: Vec<String>,
    /// `http://` も許すか(開発用)。
    pub allow_http: bool,
    /// 1 回の実行で問い合わせられる残りの回数。
    pub fetches_left: usize,
    pub fetcher: Fetcher,
}

impl Network {
    pub fn new(hosts: Vec<String>, fetcher: Fetcher) -> Self {
        Self {
            hosts,
            allow_http: false,
            fetches_left: MAX_FETCHES,
            fetcher,
        }
    }
}

/// `storage` の権限があるときの保存領域(実行する人 × プラグインごと)。
#[derive(Clone, Debug, Default)]
pub struct PluginStorage {
    pub data: BTreeMap<String, Value>,
    /// 使っている大きさ(キーと値の JSON のバイト数の合計)。
    pub used: usize,
    /// 実行中に変わったか。
    pub changed: bool,
}

impl PluginStorage {
    pub fn new(data: BTreeMap<String, Value>) -> Self {
        let used = data.iter().map(|(k, v)| entry_size(k, v)).sum();
        Self {
            data,
            used,
            changed: false,
        }
    }

    pub fn get(&self, key: &str) -> Value {
        self.data.get(key).cloned().unwrap_or(Value::Null)
    }

    pub fn set(&mut self, key: String, value: Value) -> Result<(), String> {
        if key.is_empty() || key.chars().count() > MAX_STORAGE_KEY_CHARS {
            return Err(format!(
                "保存のキーは 1〜{MAX_STORAGE_KEY_CHARS} 文字にしてください"
            ));
        }
        let old = self.data.get(&key).map_or(0, |v| entry_size(&key, v));
        if old == 0 && self.data.len() >= MAX_STORAGE_KEYS {
            return Err(format!("保存できるのは {MAX_STORAGE_KEYS} 件までです"));
        }
        let new = entry_size(&key, &value);
        let used = self.used - old + new;
        if used > MAX_STORAGE_BYTES {
            return Err(format!(
                "保存領域がいっぱいです({} KB まで)",
                MAX_STORAGE_BYTES / 1024
            ));
        }
        self.used = used;
        self.data.insert(key, value);
        self.changed = true;
        Ok(())
    }

    pub fn delete(&mut self, key: &str) {
        if let Some(v) = self.data.remove(key) {
            self.used -= entry_size(key, &v);
            self.changed = true;
        }
    }
}

fn entry_size(k: &str, v: &Value) -> usize {
    k.len() + serde_json::to_vec(v).map_or(0, |b| b.len())
}

/// ホスト関数を実行する側の状態。サーバーと、プラグインのテスト用の擬似ホストで共有する。
pub struct HostContext {
    /// 操作するボード(実行の後に取り出す)。
    pub board: Board,
    pub can_read: bool,
    pub can_write: bool,
    pub logs: Vec<String>,
    /// 適用した操作の数。
    pub ops_applied: usize,
    /// 保存領域(`storage` が使えなければ `None`)。
    pub storage: Option<PluginStorage>,
    /// 通信の窓口(`network` が使えなければ `None`)。
    pub network: Option<Network>,
    /// 通信の記録。
    pub fetches: Vec<FetchRecord>,
}

/// ログの上限。
pub const MAX_LOG_LINES: usize = 200;
pub const MAX_LOG_CHARS: usize = 1000;
/// 1 回の実行で問い合わせられる回数。
pub const MAX_FETCHES: usize = 20;
/// 問い合わせの本文の大きさ。
pub const MAX_FETCH_BODY: usize = 1024 * 1024;
/// 問い合わせに付けられるヘッダーの数。
pub const MAX_FETCH_HEADERS: usize = 32;
/// 保存領域の上限。
pub const MAX_STORAGE_BYTES: usize = 1024 * 1024;
pub const MAX_STORAGE_KEYS: usize = 1000;
pub const MAX_STORAGE_KEY_CHARS: usize = 200;

/// プラグインが付けてはいけないヘッダー(接続の仕組みに関わるもの)。
const FORBIDDEN_HEADERS: [&str; 12] = [
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "proxy-authorization",
    "proxy-connection",
    "forwarded",
    "x-forwarded-for",
];

const METHODS: [&str; 6] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];

fn no_permission(p: Permission) -> String {
    format!(
        "このプラグインには「{}」権限がありません(マニフェストで求めていないか、実行する人が許可していません)",
        p.label()
    )
}

impl HostContext {
    pub fn new(board: Board, can_read: bool, can_write: bool) -> Self {
        Self {
            board,
            can_read,
            can_write,
            logs: Vec::new(),
            ops_applied: 0,
            storage: None,
            network: None,
            fetches: Vec::new(),
        }
    }

    /// 保存領域を使えるようにする。
    pub fn with_storage(mut self, storage: PluginStorage) -> Self {
        self.storage = Some(storage);
        self
    }

    /// 通信できるようにする。
    pub fn with_network(mut self, network: Network) -> Self {
        self.network = Some(network);
        self
    }

    /// 1 回の呼び出しを処理して JSON の値を返す。
    pub fn dispatch(&mut self, call: HostCall) -> Result<Value, String> {
        let need_read = |ok: bool| {
            if ok {
                Ok(())
            } else {
                Err(no_permission(Permission::BoardRead))
            }
        };
        match call {
            HostCall::Query { query } => {
                need_read(self.can_read)?;
                Ok(serde_json::to_value(ops::query(&self.board, &query)).unwrap_or(Value::Null))
            }
            HostCall::Get { id } => {
                need_read(self.can_read)?;
                Ok(serde_json::to_value(ops::get(&self.board, &id)).unwrap_or(Value::Null))
            }
            HostCall::Apply { ops: list } => {
                if !self.can_write {
                    return Err(no_permission(Permission::BoardWrite));
                }
                // 先に写しで試し、失敗する一括操作は手元のボードに一切触れない。
                let mut trial = Board::new();
                trial.apply_remote(&self.board.encode_diff(None)?)?;
                ops::apply(&mut trial, &list).map_err(|e| e.to_string())?;
                let r = ops::apply(&mut self.board, &list).map_err(|e| e.to_string())?;
                self.ops_applied += list.len();
                Ok(serde_json::to_value(r).unwrap_or(Value::Null))
            }
            HostCall::Log { message } => {
                if self.logs.len() < MAX_LOG_LINES {
                    self.logs
                        .push(message.chars().take(MAX_LOG_CHARS).collect());
                }
                Ok(Value::Null)
            }
            HostCall::Fetch(req) => self.fetch(req),
            HostCall::StorageGet { key } => Ok(self.storage()?.get(&key)),
            HostCall::StorageSet { key, value } => {
                self.storage()?.set(key, value)?;
                Ok(Value::Null)
            }
            HostCall::StorageDelete { key } => {
                self.storage()?.delete(&key);
                Ok(Value::Null)
            }
            HostCall::StorageKeys => Ok(Value::from(
                self.storage()?.data.keys().cloned().collect::<Vec<_>>(),
            )),
        }
    }

    fn storage(&mut self) -> Result<&mut PluginStorage, String> {
        self.storage
            .as_mut()
            .ok_or_else(|| no_permission(Permission::Storage))
    }

    fn fetch(&mut self, mut req: FetchRequest) -> Result<Value, String> {
        let net = self
            .network
            .as_mut()
            .ok_or_else(|| no_permission(Permission::Network))?;
        req.method = req.method.to_ascii_uppercase();
        if !METHODS.contains(&req.method.as_str()) {
            return Err(format!("メソッド {} は使えません", req.method));
        }
        let target = url_target(&req.url)?;
        if target.scheme == "http" && !net.allow_http {
            return Err(
                "暗号化されていない http:// には接続できません(https:// にしてください)".into(),
            );
        }
        if !net.hosts.iter().any(|p| host_matches(p, &target.host)) {
            return Err(format!(
                "「{}」への通信は許可されていません(マニフェストの hosts に書き、実行する人に許可してもらってください)",
                target.host
            ));
        }
        if req.headers.len() > MAX_FETCH_HEADERS {
            return Err(format!("ヘッダーは {MAX_FETCH_HEADERS} 個までです"));
        }
        let mut headers = BTreeMap::new();
        for (k, v) in req.headers {
            let k = k.trim().to_ascii_lowercase();
            if k.is_empty() || FORBIDDEN_HEADERS.contains(&k.as_str()) || k.starts_with("sec-") {
                return Err(format!("ヘッダー「{k}」は付けられません"));
            }
            if v.contains(['\r', '\n']) {
                return Err(format!("ヘッダー「{k}」の値に改行は入れられません"));
            }
            headers.insert(k, v);
        }
        req.headers = headers;
        if req.body.as_ref().is_some_and(|b| b.len() > MAX_FETCH_BODY) {
            return Err(format!("送る本文は {} KB までです", MAX_FETCH_BODY / 1024));
        }
        if net.fetches_left == 0 {
            return Err(format!(
                "1 回の実行で通信できるのは {MAX_FETCHES} 回までです"
            ));
        }
        net.fetches_left -= 1;
        let r = (net.fetcher)(&req);
        self.fetches.push(FetchRecord {
            method: req.method.clone(),
            url: req.url.clone(),
            status: r.as_ref().ok().map(|r| r.status),
        });
        let r = r?;
        Ok(serde_json::to_value(r).unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{ElementQuery, Op};
    use serde_json::json;

    #[test]
    fn manifest_validation() {
        let mut m: PluginManifest = serde_json::from_value(json!({
            "id": "tidy", "name": "整理", "permissions": ["board:read", "board:write"],
            "commands": [{"id": "run", "title": "実行"}]
        }))
        .unwrap();
        assert!(m.validate().is_ok());
        assert!(m.can(Permission::BoardWrite));
        m.id = "Bad Id".into();
        assert!(m.validate().is_err());
        m.id = "ok".into();
        m.abi = 9;
        assert!(m.validate().is_err());
        m.abi = ABI_VERSION;
        m.commands.clear();
        assert!(m.validate().is_err());
    }

    #[test]
    fn dispatch_respects_permissions_and_atomicity() {
        let mut ro = HostContext::new(Board::new(), true, false);
        assert!(
            ro.dispatch(HostCall::Query {
                query: ElementQuery::default()
            })
            .is_ok()
        );
        let create: Vec<Op> = serde_json::from_value(json!([
            {"op": "create", "element": {"type": "shape", "kind": "sticky", "text": "a"}}
        ]))
        .unwrap();
        assert!(
            ro.dispatch(HostCall::Apply {
                ops: create.clone()
            })
            .is_err()
        );

        let mut rw = HostContext::new(ro.board, true, true);
        let r = rw.dispatch(HostCall::Apply { ops: create }).unwrap();
        assert_eq!(r["created"].as_array().unwrap().len(), 1);
        // 2 つ目で失敗する一括操作は 1 つ目も残さない。
        let bad: Vec<Op> = serde_json::from_value(json!([
            {"op": "create", "element": {"type": "shape", "kind": "sticky"}},
            {"op": "delete", "id": "ffffff"}
        ]))
        .unwrap();
        assert!(rw.dispatch(HostCall::Apply { ops: bad }).is_err());
        rw.dispatch(HostCall::Log {
            message: "hi".into(),
        })
        .unwrap();
        assert_eq!(rw.logs, vec!["hi".to_string()]);
        assert_eq!(rw.ops_applied, 1);
        assert_eq!(rw.board.len(), 1);

        let mut none = HostContext::new(rw.board, false, false);
        assert!(none.dispatch(HostCall::Get { id: "1".into() }).is_err());
    }

    #[test]
    fn network_and_hosts() {
        let mut m: PluginManifest = serde_json::from_value(json!({
            "id": "net", "name": "通信", "permissions": ["network"],
            "commands": [{"id": "run", "title": "実行"}]
        }))
        .unwrap();
        // 接続先を書かずに network は求められない。
        assert!(m.validate().is_err());
        m.hosts = vec!["api.example.com".into(), "*.cdn.example.com".into()];
        assert!(m.validate().is_ok());
        m.hosts.push("https://evil.example".into());
        assert!(m.validate().is_err());

        assert!(host_matches("api.example.com", "api.example.com"));
        assert!(!host_matches("api.example.com", "api.example.com:8443"));
        assert!(!host_matches(
            "api.example.com",
            "api.example.com.evil.test"
        ));
        assert!(host_matches("*.example.com", "a.b.example.com"));
        assert!(!host_matches("*.example.com", "example.com"));
        assert!(!host_matches("*.example.com", "badexample.com"));
        assert!(host_matches("localhost:8080", "localhost:8080"));
        assert!(pattern_covered("*.example.com", "api.example.com"));
        assert!(pattern_covered("*.example.com", "*.a.example.com"));
        assert!(!pattern_covered("api.example.com", "*.example.com"));

        assert_eq!(
            url_target("HTTPS://API.Example.com:443/v1?q=1")
                .unwrap()
                .host,
            "api.example.com"
        );
        assert!(url_target("https://user@api.example.com/").is_err());
        assert!(url_target("file:///etc/passwd").is_err());
        assert!(url_target("https://[::1]/").is_err());

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = calls.clone();
        let fetcher: Fetcher = Box::new(move |req| {
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(FetchResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: format!("{} {}", req.method, req.url),
            })
        });
        let mut ctx = HostContext::new(Board::new(), false, false).with_network(Network::new(
            vec!["api.example.com".into(), "*.cdn.example.com".into()],
            fetcher,
        ));
        let fetch = |url: &str| HostCall::Fetch(FetchRequest::get(url));
        let r = ctx.dispatch(fetch("https://api.example.com/x")).unwrap();
        assert_eq!(r["body"], "GET https://api.example.com/x");
        assert!(
            ctx.dispatch(fetch("https://img.cdn.example.com/a.png"))
                .is_ok()
        );
        // 宣言していない先・http・おかしなヘッダーは送る前に断る。
        let e = ctx.dispatch(fetch("https://evil.test/")).unwrap_err();
        assert!(e.contains("evil.test"), "{e}");
        assert!(ctx.dispatch(fetch("http://api.example.com/")).is_err());
        let bad = FetchRequest::get("https://api.example.com/").header("Host", "evil.test");
        assert!(ctx.dispatch(HostCall::Fetch(bad)).is_err());
        let bad = FetchRequest::get("https://api.example.com/").header("x-a", "1\r\nx-b: 2");
        assert!(ctx.dispatch(HostCall::Fetch(bad)).is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(ctx.fetches.len(), 2);
        // 回数の上限。
        for _ in 0..MAX_FETCHES {
            let _ = ctx.dispatch(fetch("https://api.example.com/x"));
        }
        let e = ctx
            .dispatch(fetch("https://api.example.com/x"))
            .unwrap_err();
        assert!(e.contains("回まで"), "{e}");

        // 権限がなければ通信できない。
        let mut none = HostContext::new(Board::new(), true, true);
        let e = none
            .dispatch(fetch("https://api.example.com/"))
            .unwrap_err();
        assert!(e.contains("外部と通信する"), "{e}");
    }

    #[test]
    fn storage_limits() {
        let mut none = HostContext::new(Board::new(), true, true);
        assert!(none.dispatch(HostCall::StorageKeys).is_err());

        let mut ctx = HostContext::new(Board::new(), false, false)
            .with_storage(PluginStorage::new(BTreeMap::new()));
        ctx.dispatch(HostCall::StorageSet {
            key: "repo".into(),
            value: json!("a/b"),
        })
        .unwrap();
        assert_eq!(
            ctx.dispatch(HostCall::StorageGet { key: "repo".into() })
                .unwrap(),
            json!("a/b")
        );
        assert_eq!(
            ctx.dispatch(HostCall::StorageKeys).unwrap(),
            json!(["repo"])
        );
        let big = "x".repeat(MAX_STORAGE_BYTES);
        assert!(
            ctx.dispatch(HostCall::StorageSet {
                key: "big".into(),
                value: json!(big),
            })
            .is_err()
        );
        ctx.dispatch(HostCall::StorageDelete { key: "repo".into() })
            .unwrap();
        let st = ctx.storage.unwrap();
        assert!(st.changed);
        assert_eq!(st.used, 0);
        assert!(st.data.is_empty());
    }

    #[test]
    fn grants_and_pending() {
        let m: PluginManifest = serde_json::from_value(json!({
            "id": "net", "name": "通信", "permissions": ["board:read", "network", "storage"],
            "hosts": ["api.example.com", "b.example.com"],
            "commands": [{"id": "run", "title": "実行"}]
        }))
        .unwrap();
        let none = PolicyDenied::default();
        let g = PluginGrant::default();
        let p = g.pending(&m, &none);
        assert_eq!(
            p.permissions,
            vec![Permission::Network, Permission::Storage]
        );
        assert_eq!(p.hosts.len(), 2);
        assert_eq!(g.effective(&m, &none).0, vec![Permission::BoardRead]);

        let g = PluginGrant::decide(
            &m,
            &GrantRequest {
                allowed: vec![
                    Permission::BoardRead,
                    Permission::Network,
                    Permission::BoardWrite,
                ],
                hosts: vec!["api.example.com".into(), "other.test".into()],
            },
            1,
        );
        assert!(g.pending(&m, &none).is_empty());
        // 求めていない権限・接続先は許可しても入らない。
        assert_eq!(
            g.effective(&m, &none),
            (
                vec![Permission::BoardRead, Permission::Network],
                vec!["api.example.com".to_string()]
            )
        );
        // 新しい版で接続先が増えたら、また確認する。
        let mut m2 = m.clone();
        m2.hosts.push("c.example.com".into());
        assert_eq!(
            g.pending(&m2, &none).hosts,
            vec!["c.example.com".to_string()]
        );
        // 管理者が止めたものは確認せず、使えもしない。
        let policy = PolicyDenied {
            permissions: vec![Permission::Network],
            hosts: vec![],
        };
        assert!(PluginGrant::default().pending(&m, &policy).hosts.is_empty());
        assert_eq!(g.effective(&m, &policy).0, vec![Permission::BoardRead]);
    }
}
