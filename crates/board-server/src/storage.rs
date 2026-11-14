//! バックアップの置き場所(S3 互換ストレージ・ディレクトリ)。
//!
//! `BACKUP_LOCATION` の URL をここで読む([`crate::backup`])。
//!
//! - `s3://バケット/接頭辞`: S3 互換ストレージ(AWS S3・Cloudflare R2・MinIO・Backblaze B2・Wasabi など)。
//!   `endpoint` を省くと AWS(`https://s3.{region}.amazonaws.com`)。`style=virtual` で仮想ホスト形式。
//!   署名は AWS Signature V4。
//! - `file:///パス`: サーバーから見えるディレクトリ(別ディスクや NAS のマウント先)。

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::store::StoreError;

/// 中身を置く場所。キーは `boards/{id}.ybin` のような `/` 区切りの文字列。
pub trait Blob: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StoreError>;
    fn delete(&self, key: &str) -> Result<(), StoreError>;
}

/// 管理者が用意した保存場所。
pub struct Location {
    pub id: String,
    pub label: String,
    /// 種類と所在の説明(画面に出す。認証情報は含めない)。
    pub description: String,
    pub blob: Arc<dyn Blob>,
}

pub fn board_key(board: &str) -> String {
    format!("boards/{board}.ybin")
}

pub fn version_key(board: &str, id: u64) -> String {
    format!("boards/{board}/versions/{id}.ybin")
}

/// 保存場所の ID として受け付けるか(英小文字・数字・`-`・`_`)。
pub fn valid_location_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && id != "local"
}

// ---- ディレクトリ ----

pub struct DirBlob {
    root: PathBuf,
}

impl DirBlob {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, key: &str) -> Result<PathBuf, StoreError> {
        // キーはこのモジュールで作るが、念のため外へ出られないようにする。
        if key
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(format!("使えないキーです: {key}").into());
        }
        Ok(self.root.join(key))
    }
}

impl Blob for DirBlob {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match std::fs::read(self.path(key)?) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.path(key)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 途中で落ちても壊れたファイルが残らないよう、書いてから名前を変える。
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), StoreError> {
        match std::fs::remove_file(self.path(key)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

// ---- S3 互換 ----

#[derive(Clone, Debug)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    /// `https://バケット.ホスト/キー` の形にするか(既定は `https://ホスト/バケット/キー`)。
    pub virtual_host: bool,
}

pub struct S3Blob {
    config: S3Config,
    http: reqwest::Client,
}

/// 同期の呼び出し元(redb と同じ扱いの保存処理)から非同期の HTTP を使うための専用ランタイム。
/// 終わらないスレッドで動かすので、どのスレッドから呼んでも(非同期の中からでも)使える。
fn io_runtime() -> &'static tokio::runtime::Handle {
    static HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("storage-io".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("保存用のランタイム");
                let _ = tx.send(rt.handle().clone());
                rt.block_on(std::future::pending::<()>());
            })
            .expect("保存用のスレッド");
        rx.recv().expect("保存用のランタイム")
    })
}

/// 非同期の処理を専用ランタイムで動かし、終わるまで待つ。
fn run_blocking<T: Send + 'static>(fut: impl Future<Output = T> + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    io_runtime().spawn(async move {
        let _ = tx.send(fut.await);
    });
    let wait = move || rx.recv().expect("保存の処理が途中で止まりました");
    // 複数スレッドのランタイムの中なら、ほかの処理を止めないよう知らせてから待つ。
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(wait)
        }
        _ => wait(),
    }
}

impl S3Blob {
    pub fn new(config: S3Config) -> Self {
        Self {
            config,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("HTTP クライアント"),
        }
    }

    /// (URL, Host ヘッダー, 署名に使うパス)。
    fn target(&self, key: &str) -> (String, String, String) {
        let c = &self.config;
        let (scheme, host) = c
            .endpoint
            .split_once("://")
            .unwrap_or(("https", c.endpoint.as_str()));
        let host = host.trim_end_matches('/');
        let object = format!("{}{key}", c.prefix);
        let encoded: String = object
            .split('/')
            .map(uri_encode)
            .collect::<Vec<_>>()
            .join("/");
        if c.virtual_host {
            let host = format!("{}.{host}", c.bucket);
            let path = format!("/{encoded}");
            (format!("{scheme}://{host}{path}"), host, path)
        } else {
            let path = format!("/{}/{encoded}", uri_encode(&c.bucket));
            (format!("{scheme}://{host}{path}"), host.to_string(), path)
        }
    }

    fn request(
        &self,
        method: &'static str,
        key: &str,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), StoreError> {
        let (url, host, path) = self.target(key);
        let now = amz_now();
        let payload_hash = hex(&Sha256::digest(&body));
        let headers = vec![
            ("host".to_string(), host),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), now.clone()),
        ];
        let auth = sigv4_authorization(&SigV4 {
            method,
            path: &path,
            query: "",
            headers: &headers,
            payload_hash: &payload_hash,
            amz_date: &now,
            region: &self.config.region,
            service: "s3",
            access_key: &self.config.access_key,
            secret_key: &self.config.secret_key,
        });
        let http = self.http.clone();
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("HTTP メソッド");
        run_blocking(async move {
            let mut req = http
                .request(m, &url)
                .header("x-amz-content-sha256", payload_hash)
                .header("x-amz-date", now)
                .header("authorization", auth);
            if !body.is_empty() || method == "PUT" {
                req = req.body(body);
            }
            let res = req.send().await.map_err(|e| e.to_string())?;
            let status = res.status().as_u16();
            let bytes = res.bytes().await.map_err(|e| e.to_string())?;
            Ok::<_, String>((status, bytes.to_vec()))
        })
        .map_err(Into::into)
    }
}

fn s3_error(op: &str, status: u16, body: &[u8]) -> StoreError {
    let text = String::from_utf8_lossy(body);
    // <Code>…</Code> だけ拾う(本文に秘密は入らない)。
    let code = text
        .split_once("<Code>")
        .and_then(|(_, r)| r.split_once("</Code>"))
        .map(|(c, _)| c.to_string())
        .unwrap_or_default();
    format!("S3 の{op}に失敗しました(HTTP {status} {code})").into()
}

impl Blob for S3Blob {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match self.request("GET", key, Vec::new())? {
            (200, b) => Ok(Some(b)),
            (404, _) => Ok(None),
            (s, b) => Err(s3_error("読み出し", s, &b)),
        }
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        match self.request("PUT", key, bytes.to_vec())? {
            (200..=299, _) => Ok(()),
            (s, b) => Err(s3_error("書き込み", s, &b)),
        }
    }

    fn delete(&self, key: &str) -> Result<(), StoreError> {
        match self.request("DELETE", key, Vec::new())? {
            (200..=299, _) | (404, _) => Ok(()),
            (s, b) => Err(s3_error("削除", s, &b)),
        }
    }
}

// ---- AWS Signature V4 ----

pub struct SigV4<'a> {
    pub method: &'a str,
    /// URI 符号化済みのパス。
    pub path: &'a str,
    /// 正規化済みのクエリ(なければ空)。
    pub query: &'a str,
    /// 署名するヘッダー(名前は小文字)。
    pub headers: &'a [(String, String)],
    pub payload_hash: &'a str,
    /// `20150830T123600Z` の形。
    pub amz_date: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
}

/// `Authorization` ヘッダーの値。
pub fn sigv4_authorization(s: &SigV4<'_>) -> String {
    let mut headers: Vec<(String, String)> = s
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{}",
        s.method, s.path, s.query, s.payload_hash
    );
    let date = &s.amz_date[..8];
    let scope = format!("{date}/{}/{}/aws4_request", s.region, s.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        s.amz_date,
        hex(&Sha256::digest(canonical_request.as_bytes()))
    );
    let k_date = hmac_sha256(format!("AWS4{}", s.secret_key).as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, s.region.as_bytes());
    let k_service = hmac_sha256(&k_region, s.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        s.access_key
    )
}

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// S3 のパスの 1 区切りを符号化する(`A-Za-z0-9-_.~` 以外を `%XX`)。
pub fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn amz_now() -> String {
    let iso = crate::audit::iso8601(crate::auth::now_ms());
    // 2026-10-31T12:34:56.789Z → 20261031T123456Z
    format!(
        "{}{}{}T{}{}{}Z",
        &iso[0..4],
        &iso[5..7],
        &iso[8..10],
        &iso[11..13],
        &iso[14..16],
        &iso[17..19]
    )
}

// ---- 設定 ----

/// `id=URL;…` の形の置き場所の一覧を読む(`BACKUP_LOCATION` は `backup=URL` として渡す)。`env` は環境変数を引く関数(テストで差し替える)。
pub fn parse_locations(
    spec: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Location>, String> {
    let mut out: Vec<Location> = Vec::new();
    for entry in spec.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        let (id, url) = entry
            .split_once('=')
            .ok_or_else(|| format!("保存場所の書き方が違います: {entry}"))?;
        let id = id.trim().to_ascii_lowercase();
        if !valid_location_id(&id) {
            return Err(format!(
                "保存場所の ID「{id}」は英小文字・数字・-・_ にしてください(local は使えません)"
            ));
        }
        if out.iter().any(|l| l.id == id) {
            return Err(format!("保存場所の ID「{id}」が重なっています"));
        }
        let url = url.trim();
        let (base, query) = url.split_once('?').unwrap_or((url, ""));
        let params: HashMap<String, String> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), percent_decode(v)))
            .collect();
        let label = params.get("label").cloned().unwrap_or_else(|| id.clone());
        let upper = id.to_ascii_uppercase().replace('-', "_");
        let (description, blob): (String, Arc<dyn Blob>) = if let Some(path) =
            base.strip_prefix("file://")
        {
            if !path.starts_with('/') {
                return Err(format!("{id}: file:// のあとは絶対パスにしてください"));
            }
            (
                "サーバーのディレクトリ".to_string(),
                Arc::new(DirBlob::new(PathBuf::from(path))),
            )
        } else if let Some(rest) = base.strip_prefix("s3://") {
            let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
            if bucket.is_empty() {
                return Err(format!("{id}: バケット名がありません"));
            }
            let region = params
                .get("region")
                .cloned()
                .unwrap_or_else(|| "us-east-1".into());
            let endpoint = params
                .get("endpoint")
                .cloned()
                .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
            let key = |name: &str| {
                env(&format!("STORAGE_{upper}_{name}")).or_else(|| env(&format!("AWS_{name}")))
            };
            let access_key = key("ACCESS_KEY_ID")
                .ok_or_else(|| format!("{id}: STORAGE_{upper}_ACCESS_KEY_ID がありません"))?;
            let secret_key = key("SECRET_ACCESS_KEY")
                .ok_or_else(|| format!("{id}: STORAGE_{upper}_SECRET_ACCESS_KEY がありません"))?;
            let prefix = match prefix.trim_matches('/') {
                "" => String::new(),
                p => format!("{p}/"),
            };
            (
                format!("S3 互換({region})"),
                Arc::new(S3Blob::new(S3Config {
                    endpoint: endpoint.trim_end_matches('/').to_string(),
                    bucket: bucket.to_string(),
                    prefix,
                    region,
                    access_key,
                    secret_key,
                    virtual_host: params.get("style").map(String::as_str) == Some("virtual"),
                })),
            )
        } else {
            return Err(format!("{id}: s3:// か file:// で始めてください({base})"));
        };
        out.push(Location {
            id,
            label,
            description,
            blob,
        });
    }
    Ok(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigv4_matches_aws_test_suite() {
        // AWS SigV4 テストスイートの get-vanilla。
        let headers = vec![
            ("Host".to_string(), "example.amazonaws.com".to_string()),
            ("X-Amz-Date".to_string(), "20150830T123600Z".to_string()),
        ];
        let auth = sigv4_authorization(&SigV4 {
            method: "GET",
            path: "/",
            query: "",
            headers: &headers,
            payload_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            amz_date: "20150830T123600Z",
            region: "us-east-1",
            service: "service",
            access_key: "AKIDEXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        });
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn hmac_rfc4231() {
        // RFC 4231 テストケース 2。
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn parses_locations() {
        let env = |k: &str| match k {
            "STORAGE_TOKYO_ACCESS_KEY_ID" => Some("AK".to_string()),
            "AWS_SECRET_ACCESS_KEY" => Some("SK".to_string()),
            _ => None,
        };
        let locs = parse_locations(
            "tokyo=s3://bucket/boards/?region=ap-northeast-1&label=%E6%9D%B1%E4%BA%AC; nas=file:///tmp/x?label=NAS",
            env,
        )
        .unwrap();
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0].label, "東京");
        assert_eq!(locs[0].description, "S3 互換(ap-northeast-1)");
        assert_eq!(locs[1].label, "NAS");
        assert!(parse_locations("local=file:///tmp", env).is_err());
        assert!(parse_locations("x=s3://b", |_| None).is_err());
        assert!(parse_locations("x=http://b", env).is_err());
        assert!(parse_locations("x=file://relative", env).is_err());
    }

    #[test]
    fn s3_urls() {
        let mk = |virtual_host| {
            S3Blob::new(S3Config {
                endpoint: "https://s3.ap-northeast-1.amazonaws.com".into(),
                bucket: "my-bucket".into(),
                prefix: "gb/".into(),
                region: "ap-northeast-1".into(),
                access_key: "a".into(),
                secret_key: "b".into(),
                virtual_host,
            })
        };
        let (url, host, path) = mk(false).target("boards/abc.ybin");
        assert_eq!(
            url,
            "https://s3.ap-northeast-1.amazonaws.com/my-bucket/gb/boards/abc.ybin"
        );
        assert_eq!(host, "s3.ap-northeast-1.amazonaws.com");
        assert_eq!(path, "/my-bucket/gb/boards/abc.ybin");
        let (url, host, _) = mk(true).target("boards/abc.ybin");
        assert_eq!(
            url,
            "https://my-bucket.s3.ap-northeast-1.amazonaws.com/gb/boards/abc.ybin"
        );
        assert_eq!(host, "my-bucket.s3.ap-northeast-1.amazonaws.com");
    }

    #[test]
    fn dir_blob() {
        let dir = tempfile::tempdir().unwrap();
        let b = DirBlob::new(dir.path().to_path_buf());
        assert_eq!(b.get("boards/a.ybin").unwrap(), None);
        b.put("boards/a.ybin", &[1, 2]).unwrap();
        b.put("boards/a/versions/1.ybin", &[3]).unwrap();
        assert_eq!(b.get("boards/a.ybin").unwrap(), Some(vec![1, 2]));
        b.delete("boards/a.ybin").unwrap();
        b.delete("boards/a.ybin").unwrap();
        assert_eq!(b.get("boards/a.ybin").unwrap(), None);
        assert!(b.get("../etc/passwd").is_err());
        assert_eq!(amz_now().len(), 16);
    }
}
