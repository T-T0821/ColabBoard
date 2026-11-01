//! グラボード の REST API(`/api/v1`)を Rust から使うためのクライアント。
//!
//! ```no_run
//! use graboard_sdk::{Batch, Client, EndpointData, NewElement};
//!
//! # async fn run() -> Result<(), graboard_sdk::Error> {
//! let client = Client::new("https://board.example.com", "gbt_…")?;
//! let board = client.create_board("API で作ったボード").await?;
//! let b = client.board(&board.id);
//!
//! // 付箋を 1 枚置く。
//! let note = b.create(NewElement::sticky(0.0, 0.0, "こんにちは")).await?;
//!
//! // まとめて作ってつなぐ(1 つでも失敗したら何も変わらない)。
//! let mut batch = Batch::new();
//! let a = batch.create(NewElement::shape("rect", 0.0, 400.0, 160.0, 100.0));
//! let c = batch.create(NewElement::shape("rect", 400.0, 400.0, 160.0, 100.0));
//! batch.create(NewElement::connector(a.endpoint(), c.endpoint()));
//! batch.delete(&note.id);
//! b.apply(batch).await?;
//! # Ok(()) }
//! ```
//!
//! 型は [`board_core::ops`] と [`board_core::api`] のものをそのまま使う。
//! API の詳細は `docs/api.md` を参照。

use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use board_core::access::{TokenInfo, TokenScope, Whoami};
pub use board_core::api::{BoardInfo, Comment, NewBoard, NewComment, Posted, Role};
pub use board_core::ops::{
    ConnectorStyle, ElementBody, ElementData, ElementPatch, ElementQuery, EndpointData, MetaPatch,
    NewElement, Op, OpsRequest, OpsResult, ShapeStyle,
};

/// SDK のエラー。
#[derive(Debug)]
pub enum Error {
    /// サーバーがエラーを返した(ステータスとメッセージ)。
    Api { status: u16, message: String },
    /// 通信や JSON の読み書きの失敗。
    Http(reqwest::Error),
    /// URL などの設定の誤り。
    Config(String),
}

impl Error {
    /// サーバーが返した HTTP ステータス(通信の失敗なら `None`)。
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Api { status, message } => write!(f, "API エラー ({status}): {message}"),
            Error::Http(e) => write!(f, "通信エラー: {e}"),
            Error::Config(m) => write!(f, "設定の誤り: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// API クライアント。複製しても接続は共有する。
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Client {
    /// `base_url` はサーバーの URL(例: `https://board.example.com`)。`token` は `gbt_…`。
    pub fn new(base_url: &str, token: impl Into<String>) -> Result<Self> {
        let mut c = Self::anonymous(base_url)?;
        c.token = Some(token.into());
        Ok(c)
    }

    /// トークンなし(共有リンクの鍵や、誰でも編集できるボード用)。
    pub fn anonymous(base_url: &str) -> Result<Self> {
        let base = base_url.trim_end_matches('/');
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(Error::Config(format!(
                "URL は http:// か https:// で始めてください: {base_url}"
            )));
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("graboard-sdk/", env!("CARGO_PKG_VERSION")))
                .build()?,
            base: base.to_string(),
            token: None,
        })
    }

    /// 環境変数 `GRABOARD_URL` と `GRABOARD_TOKEN` から作る。
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("GRABOARD_URL")
            .map_err(|_| Error::Config("GRABOARD_URL を設定してください".into()))?;
        match std::env::var("GRABOARD_TOKEN") {
            Ok(t) => Self::new(&url, t),
            Err(_) => Self::anonymous(&url),
        }
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<Option<T>> {
        let mut req = self
            .http
            .request(method, format!("{}/api/v1{path}", self.base))
            .query(query);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = req.send().await?;
        let status = res.status();
        if status == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or(text);
            return Err(Error::Api {
                status: status.as_u16(),
                message,
            });
        }
        Ok(Some(res.json().await?))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        self.send(Method::GET, path, query, None::<&()>)
            .await?
            .ok_or_else(|| Error::Config("空の応答".into()))
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        body: &(impl Serialize + ?Sized),
    ) -> Result<T> {
        self.send(Method::POST, path, query, Some(body))
            .await?
            .ok_or_else(|| Error::Config("空の応答".into()))
    }

    /// 自分(トークンで呼んだときはトークンの情報も)。
    pub async fn me(&self) -> Result<Whoami> {
        self.get("/me", &[]).await
    }

    /// 自分のボード一覧(最近開いた順)。
    pub async fn boards(&self) -> Result<Vec<BoardInfo>> {
        self.get("/boards", &[]).await
    }

    /// ボードを作る(ボードを限らない `write` のトークンが必要)。
    pub async fn create_board(&self, title: &str) -> Result<BoardInfo> {
        self.post(
            "/boards",
            &[],
            &NewBoard {
                title: title.to_string(),
                team: None,
                e2e: None,
            },
        )
        .await
    }

    /// チームのボードとして作る(自分がそのチームのメンバーである必要がある)。
    pub async fn create_team_board(&self, title: &str, team: &str) -> Result<BoardInfo> {
        self.post(
            "/boards",
            &[],
            &NewBoard {
                title: title.to_string(),
                team: Some(team.to_string()),
                e2e: None,
            },
        )
        .await
    }

    /// 1 枚のボードを操作するためのハンドル(通信はしない)。
    pub fn board(&self, id: &str) -> Board {
        Board {
            client: self.clone(),
            id: id.to_string(),
            key: None,
        }
    }
}

/// 1 枚のボード。
#[derive(Clone, Debug)]
pub struct Board {
    client: Client,
    id: String,
    key: Option<String>,
}

impl Board {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 共有リンクの鍵を付ける。
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    fn path(&self, rest: &str) -> String {
        format!("/boards/{}{rest}", self.id)
    }

    fn query(&self) -> Vec<(&'static str, String)> {
        self.key.iter().map(|k| ("key", k.clone())).collect()
    }

    /// ボードの情報と自分の権限。
    pub async fn info(&self) -> Result<BoardInfo> {
        self.client.get(&self.path(""), &self.query()).await
    }

    /// 要素の一覧(奥から順)。
    pub async fn elements(&self, q: &ElementQuery) -> Result<Vec<ElementData>> {
        let mut query = self.query();
        if let Some(t) = &q.element_type {
            query.push(("type", t.clone()));
        }
        if let Some(k) = &q.kind {
            query.push(("kind", k.clone()));
        }
        if let Some([a, b, c, d]) = q.bbox {
            query.push(("bbox", format!("{a},{b},{c},{d}")));
        }
        if let Some(t) = &q.text {
            query.push(("text", t.clone()));
        }
        if let Some(ids) = &q.ids {
            query.push(("ids", ids.join(",")));
        }
        if let Some(l) = q.limit {
            query.push(("limit", l.to_string()));
        }
        self.client.get(&self.path("/elements"), &query).await
    }

    /// 付箋だけを取る(よく使うので近道)。
    pub async fn stickies(&self) -> Result<Vec<ElementData>> {
        self.elements(&ElementQuery {
            kind: Some("sticky".into()),
            ..Default::default()
        })
        .await
    }

    /// 要素を 1 つ取る。
    pub async fn element(&self, id: &str) -> Result<ElementData> {
        self.client
            .get(&self.path(&format!("/elements/{id}")), &self.query())
            .await
    }

    /// 要素を 1 つ作る。
    pub async fn create(&self, element: NewElement) -> Result<ElementData> {
        self.client
            .post(&self.path("/elements"), &self.query(), &element)
            .await
    }

    /// 要素を変える(指定したところだけ)。
    pub async fn update(&self, id: &str, set: ElementPatch) -> Result<ElementData> {
        self.client
            .send(
                Method::PATCH,
                &self.path(&format!("/elements/{id}")),
                &self.query(),
                Some(&set),
            )
            .await?
            .ok_or_else(|| Error::Config("空の応答".into()))
    }

    /// 要素の文字を変える(近道)。
    pub async fn set_text(&self, id: &str, text: &str) -> Result<ElementData> {
        self.update(
            id,
            ElementPatch {
                shape: ShapeStyle {
                    text: Some(text.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
    }

    /// 要素を動かす(近道。図形と手書きの線)。
    pub async fn move_to(&self, id: &str, x: f32, y: f32) -> Result<ElementData> {
        self.update(
            id,
            ElementPatch {
                x: Some(x),
                y: Some(y),
                ..Default::default()
            },
        )
        .await
    }

    /// 要素を消す。
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.client
            .send::<serde_json::Value>(
                Method::DELETE,
                &self.path(&format!("/elements/{id}")),
                &self.query(),
                None::<&()>,
            )
            .await?;
        Ok(())
    }

    /// まとめて適用する。1 つでも失敗したら何も変わらない。
    pub async fn apply(&self, ops: impl Into<OpsRequest>) -> Result<OpsResult> {
        self.client
            .post(&self.path("/ops"), &self.query(), &ops.into())
            .await
    }

    /// コメント一覧。
    pub async fn comments(&self) -> Result<Vec<Comment>> {
        self.client
            .get(&self.path("/comments"), &self.query())
            .await
    }

    /// ボードの位置 (x, y) にコメントを書く。
    pub async fn comment(&self, body: &str, x: f32, y: f32) -> Result<Posted> {
        let c: NewComment = serde_json::from_value(serde_json::json!({
            "body": body, "x": x, "y": y,
        }))
        .map_err(|e| Error::Config(e.to_string()))?;
        self.client
            .post(&self.path("/comments"), &self.query(), &c)
            .await
    }
}

/// 一括操作を組み立てる。
#[derive(Clone, Debug, Default)]
pub struct Batch {
    ops: Vec<Op>,
    created: usize,
}

/// 同じ一括操作で作る要素への参照(`$0`, `$1`, …)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ref(String);

impl Ref {
    /// ID として使う文字列(`$N`)。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// コネクタの端にする。
    pub fn endpoint(&self) -> EndpointData {
        EndpointData::element(self.0.clone())
    }
}

impl Batch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// 作る。返り値で、同じ一括操作の中から参照できる。
    pub fn create(&mut self, element: NewElement) -> Ref {
        self.ops.push(Op::Create { element });
        self.created += 1;
        Ref(format!("${}", self.created - 1))
    }

    /// 変える。`id` は既存の要素の ID か [`Ref::as_str`]。
    pub fn update(&mut self, id: &str, set: ElementPatch) -> &mut Self {
        self.ops.push(Op::Update {
            id: id.to_string(),
            set,
        });
        self
    }

    pub fn delete(&mut self, id: &str) -> &mut Self {
        self.ops.push(Op::Delete { id: id.to_string() });
        self
    }

    pub fn into_ops(self) -> Vec<Op> {
        self.ops
    }
}

impl From<Batch> for OpsRequest {
    fn from(b: Batch) -> Self {
        OpsRequest { ops: b.ops }
    }
}
