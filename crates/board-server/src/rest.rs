//! 外部からボードを操作する REST API(`/api/v1/...`)。
//!
//! Web クライアント用の `/api/...`(Cookie のログイン)と違い、`Authorization: Bearer <token>`
//! の API トークン([`crate::tokens`])でも呼べる。トークンの権限はトークンのスコープと
//! 持ち主の権限の弱い方。Cookie のログインでも呼べる(その場合は本人の権限)。
//! 共有リンクの鍵(`?key=`)や、誰でも編集できるボードは、これまでどおりログインなしでも使える。
//!
//! 要素の形と操作は [`board_core::ops`] を参照。

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use board_core::access::{CreatedToken, NewToken, TokenInfo, Whoami};
use board_core::api::{BoardInfo, Comment, NewBoard, NewComment, Posted, Role};
use board_core::ops::{
    self, ElementData, ElementPatch, ElementQuery, NewElement, Op, OpError, OpsRequest, OpsResult,
};
use board_core::protocol::TOPIC_COMMENTS;
use serde::Deserialize;

use crate::AppState;
use crate::api::{self, Error};
use crate::auth::UserRecord;
use crate::boards::{self, BoardMeta};
use crate::comments;
use crate::store::StoreError;
use crate::tokens::{self, TokenRecord};

/// `/api/v1` の下のルート。
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me", get(whoami))
        .route("/boards", get(list_boards).post(create_board))
        .route("/boards/{id}", get(board_info))
        .route(
            "/boards/{id}/elements",
            get(list_elements).post(create_element),
        )
        .route(
            "/boards/{id}/elements/{eid}",
            get(get_element)
                .patch(update_element)
                .delete(delete_element),
        )
        .route("/boards/{id}/ops", post(apply_ops))
        .route(
            "/boards/{id}/comments",
            get(list_comments).post(post_comment),
        )
}

/// トークンの管理(`/api/tokens`)。Web クライアントから Cookie のログインで使う。
/// トークンでトークンを作ることはできない。
pub fn token_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/tokens", get(list_tokens).post(create_token))
        .route("/tokens/{tid}", delete(revoke_token))
}

type ApiResult<T> = Result<T, Error>;

/// 誰が呼んでいるか。
pub enum Actor {
    /// 未ログイン(共有リンクの鍵か、誰でも編集できるボードだけ)。
    Anonymous,
    /// Cookie でログイン中。
    Session(UserRecord),
    /// API トークン。
    Token(UserRecord, TokenRecord),
}

impl Actor {
    pub fn user(&self) -> Option<&UserRecord> {
        match self {
            Actor::Anonymous => None,
            Actor::Session(u) | Actor::Token(u, _) => Some(u),
        }
    }

    pub fn login(&self) -> Option<&str> {
        self.user().map(|u| u.login.as_str())
    }

    pub fn token(&self) -> Option<&TokenRecord> {
        match self {
            Actor::Token(_, t) => Some(t),
            _ => None,
        }
    }

    /// このボードで使える権限の上限(トークンで限られる)。使えなければ `None`。
    pub fn cap(&self, board: &str) -> Option<Role> {
        match self {
            Actor::Token(_, t) if !t.allows_board(board) => None,
            Actor::Token(_, t) => Some(t.scope.max_role()),
            _ => Some(Role::Owner),
        }
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// リクエストから呼び手を決める。トークンが付いていて正しくなければ 401。
pub fn actor(state: &AppState, headers: &HeaderMap) -> ApiResult<Actor> {
    if let Some(token) = bearer(headers) {
        let rec = tokens::verify(state.store(), token)?.ok_or_else(invalid_token)?;
        let user = crate::auth::get_user(state.store(), &rec.login)?.ok_or_else(invalid_token)?;
        return Ok(Actor::Token(user, rec));
    }
    Ok(match api::current_user(state, headers)? {
        Some(u) => Actor::Session(u),
        None => Actor::Anonymous,
    })
}

fn invalid_token() -> Error {
    Error::new(
        StatusCode::UNAUTHORIZED,
        "API トークンが正しくないか、期限が切れています",
    )
}

fn require_user(actor: &Actor) -> ApiResult<&UserRecord> {
    actor.user().ok_or_else(Error::unauthorized)
}

/// ボードを開けるか確かめ、使える権限を返す。
pub fn access(
    state: &AppState,
    actor: &Actor,
    id: &str,
    key: Option<&str>,
) -> ApiResult<(Option<BoardMeta>, Role)> {
    let cap = actor.cap(id).ok_or_else(Error::not_found)?;
    let (meta, role) = api::board_access(state, id, actor.login(), key)?;
    Ok((meta, role.min(cap)))
}

fn need(role: Role, want: Role) -> ApiResult<()> {
    if role >= want {
        Ok(())
    } else {
        Err(Error::forbidden())
    }
}

fn op_error(e: OpError) -> Error {
    Error::bad_request(e.to_string())
}

/// 重い処理(ボードの読み込み)を別スレッドで。
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::from(StoreError::from(e.to_string())))?
}

impl From<String> for Error {
    fn from(e: String) -> Self {
        tracing::error!("ボードを読めません: {e}");
        Error::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ボードを読めませんでした",
        )
    }
}

#[derive(Deserialize)]
pub struct KeyQuery {
    pub key: Option<String>,
}

// ---- 自分・トークン ----

async fn whoami(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Whoami>> {
    let actor = actor(&state, &headers)?;
    let user = require_user(&actor)?;
    Ok(Json(Whoami {
        login: user.login.clone(),
        name: user.name.clone(),
        token: actor.token().map(TokenRecord::info),
    }))
}

/// トークンの管理は Cookie のログインだけで受け付ける。
fn session_only(state: &AppState, headers: &HeaderMap) -> ApiResult<UserRecord> {
    if bearer(headers).is_some() {
        return Err(Error::forbidden());
    }
    api::current_user(state, headers)?.ok_or_else(Error::unauthorized)
}

async fn list_tokens(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<TokenInfo>>> {
    let user = session_only(&state, &headers)?;
    Ok(Json(
        tokens::list(state.store(), &user.login)?
            .iter()
            .map(TokenRecord::info)
            .collect(),
    ))
}

async fn create_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<NewToken>,
) -> ApiResult<(StatusCode, Json<CreatedToken>)> {
    let user = session_only(&state, &headers)?;
    let (token, rec) = tokens::create(state.store(), &user.login, &req)?;
    tracing::info!(login = %user.login, token = %rec.id, scope = ?rec.scope, "API トークン作成");
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            token,
            info: rec.info(),
        }),
    ))
}

async fn revoke_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(tid): Path<String>,
) -> ApiResult<StatusCode> {
    let user = session_only(&state, &headers)?;
    if tokens::revoke(state.store(), &user.login, &tid)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Error::new(
            StatusCode::NOT_FOUND,
            "トークンが見つかりません",
        ))
    }
}

// ---- ボード ----

async fn list_boards(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<BoardInfo>>> {
    let actor = actor(&state, &headers)?;
    let user = require_user(&actor)?;
    let list = boards::list_for(state.store(), &user.login, state.config.anonymous_boards)?;
    Ok(Json(
        list.into_iter()
            .filter_map(|mut b| {
                b.role = b.role.min(actor.cap(&b.id)?);
                Some(b)
            })
            .collect(),
    ))
}

async fn create_board(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<NewBoard>,
) -> ApiResult<(StatusCode, Json<BoardInfo>)> {
    let actor = actor(&state, &headers)?;
    let user = require_user(&actor)?;
    // ボードを限ったトークンや読むだけのトークンでは作れない。
    if let Some(t) = actor.token()
        && (t.boards.is_some() || t.scope.max_role() < Role::Editor)
    {
        return Err(Error::forbidden());
    }
    let meta = boards::create(state.store(), &user.login, &req.title)?;
    tracing::info!(board = %meta.id, owner = %user.login, "ボード作成(API)");
    let role = Role::Owner.min(actor.cap(&meta.id).unwrap_or(Role::Viewer));
    Ok((StatusCode::CREATED, Json(meta.info(role, None))))
}

async fn board_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<BoardInfo>> {
    let actor = actor(&state, &headers)?;
    let (meta, role) = access(&state, &actor, &id, q.key.as_deref())?;
    Ok(Json(match meta {
        Some(m) => m.info(role, None),
        None => BoardInfo {
            id: id.clone(),
            title: id,
            owner: None,
            role,
            created: 0,
            updated: 0,
            key: None,
        },
    }))
}

// ---- 要素 ----

/// `GET /elements` の絞り込み(クエリ文字列)。
#[derive(Deserialize)]
pub struct ElementsQuery {
    pub key: Option<String>,
    #[serde(rename = "type")]
    pub element_type: Option<String>,
    pub kind: Option<String>,
    /// `x0,y0,x1,y1`
    pub bbox: Option<String>,
    pub text: Option<String>,
    /// カンマ区切りの ID。
    pub ids: Option<String>,
    pub limit: Option<usize>,
}

impl ElementsQuery {
    fn to_query(&self) -> ApiResult<ElementQuery> {
        let bbox = match &self.bbox {
            None => None,
            Some(s) => {
                let v: Vec<f32> = s
                    .split(',')
                    .map(|p| p.trim().parse::<f32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| Error::bad_request("bbox は x0,y0,x1,y1 で指定してください"))?;
                match v.as_slice() {
                    [a, b, c, d] if v.iter().all(|f| f.is_finite()) => Some([*a, *b, *c, *d]),
                    _ => return Err(Error::bad_request("bbox は x0,y0,x1,y1 で指定してください")),
                }
            }
        };
        Ok(ElementQuery {
            element_type: self.element_type.clone(),
            kind: self.kind.clone(),
            bbox,
            text: self.text.clone(),
            ids: self
                .ids
                .as_ref()
                .map(|s| s.split(',').map(|p| p.trim().to_string()).collect()),
            limit: self.limit,
        })
    }
}

async fn list_elements(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ElementsQuery>,
) -> ApiResult<Json<Vec<ElementData>>> {
    let actor = actor(&state, &headers)?;
    access(&state, &actor, &id, q.key.as_deref())?;
    let query = q.to_query()?;
    let hub = state.hub.clone();
    blocking(move || Ok(hub.read(&id, |b| ops::query(b, &query))?))
        .await
        .map(Json)
}

async fn get_element(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, eid)): Path<(String, String)>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<ElementData>> {
    let actor = actor(&state, &headers)?;
    access(&state, &actor, &id, q.key.as_deref())?;
    let hub = state.hub.clone();
    blocking(move || {
        hub.read(&id, |b| ops::get(b, &eid))?
            .ok_or_else(element_not_found)
    })
    .await
    .map(Json)
}

fn element_not_found() -> Error {
    Error::new(StatusCode::NOT_FOUND, "要素が見つかりません")
}

/// 編集者以上で、ボードに操作を適用する。
async fn run_ops(
    state: &AppState,
    actor: &Actor,
    id: String,
    key: Option<&str>,
    ops: Vec<Op>,
) -> ApiResult<OpsResult> {
    let (_, role) = access(state, actor, &id, key)?;
    need(role, Role::Editor)?;
    let hub = state.hub.clone();
    blocking(move || hub.edit(&id, |b| ops::apply(b, &ops).map_err(op_error))).await
}

async fn create_element(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(element): Json<NewElement>,
) -> ApiResult<(StatusCode, Json<ElementData>)> {
    let actor = actor(&state, &headers)?;
    let r = run_ops(
        &state,
        &actor,
        id,
        q.key.as_deref(),
        vec![Op::Create { element }],
    )
    .await?;
    let e = r
        .elements
        .into_iter()
        .next()
        .ok_or_else(element_not_found)?;
    Ok((StatusCode::CREATED, Json(e)))
}

async fn update_element(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, eid)): Path<(String, String)>,
    Query(q): Query<KeyQuery>,
    Json(set): Json<ElementPatch>,
) -> ApiResult<Json<ElementData>> {
    let actor = actor(&state, &headers)?;
    if ops::parse_id(&eid).is_none() {
        return Err(element_not_found());
    }
    let r = run_ops(
        &state,
        &actor,
        id,
        q.key.as_deref(),
        vec![Op::Update { id: eid, set }],
    )
    .await
    .map_err(missing_to_404)?;
    r.elements
        .into_iter()
        .next()
        .map(Json)
        .ok_or_else(element_not_found)
}

async fn delete_element(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, eid)): Path<(String, String)>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<StatusCode> {
    let actor = actor(&state, &headers)?;
    if ops::parse_id(&eid).is_none() {
        return Err(element_not_found());
    }
    run_ops(
        &state,
        &actor,
        id,
        q.key.as_deref(),
        vec![Op::Delete { id: eid }],
    )
    .await
    .map_err(missing_to_404)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 1 要素の操作で、対象がないときは 404 にする。
fn missing_to_404(e: Error) -> Error {
    if e.status() == StatusCode::BAD_REQUEST && e.message().contains("はありません") {
        element_not_found()
    } else {
        e
    }
}

async fn apply_ops(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<OpsRequest>,
) -> ApiResult<Json<OpsResult>> {
    let actor = actor(&state, &headers)?;
    run_ops(&state, &actor, id, q.key.as_deref(), req.ops)
        .await
        .map(Json)
}

// ---- コメント ----

async fn list_comments(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<Vec<Comment>>> {
    let actor = actor(&state, &headers)?;
    access(&state, &actor, &id, q.key.as_deref())?;
    Ok(Json(comments::list(state.store(), &id)?))
}

async fn post_comment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<NewComment>,
) -> ApiResult<(StatusCode, Json<Posted>)> {
    let actor = actor(&state, &headers)?;
    let user = require_user(&actor)?.clone();
    let (meta, role) = access(&state, &actor, &id, q.key.as_deref())?;
    // 読むだけのトークンでもコメントはできない(閲覧者扱い)。
    need(role, Role::Commenter)?;
    let posted = comments::post(
        state.store(),
        meta.as_ref(),
        &id,
        &user,
        req,
        state.config.anonymous_boards,
    )?;
    state.hub.refresh(&id, TOPIC_COMMENTS);
    Ok((StatusCode::CREATED, Json(posted)))
}
