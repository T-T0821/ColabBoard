//! HTTP API(`/api/...`)。型と一覧は [`board_core::api`] を参照。

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use board_core::api::{
    ApiError, BoardInfo, Comment, EditComment, Login, MarkRead, Me, NewBoard, NewComment,
    NewVersion, Notification, Posted, RenameBoard, Role, SetLink, SetMember, Sharing, Signup,
    VersionInfo,
};
use board_core::protocol::{TOPIC_COMMENTS, TOPIC_VERSIONS};
use serde::Deserialize;

use crate::auth::{self, AuthError, SESSION_COOKIE, UserRecord};
use crate::boards::{self, BoardMeta, Visitor};
use crate::comments::{self, CommentError};
use crate::store::StoreError;
use crate::versions;
use crate::{AppState, valid_room_name};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/signup", post(signup))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route("/boards", get(list_boards).post(create_board))
        .route(
            "/boards/{id}",
            get(board_info).patch(rename_board).delete(delete_board),
        )
        .route("/boards/{id}/sharing", get(sharing))
        .route("/boards/{id}/members", put(set_member))
        .route("/boards/{id}/link", put(set_link))
        .route(
            "/boards/{id}/comments",
            get(list_comments).post(post_comment),
        )
        .route(
            "/boards/{id}/comments/{cid}",
            axum::routing::patch(edit_comment).delete(delete_comment),
        )
        .route(
            "/boards/{id}/versions",
            get(list_versions).post(create_version),
        )
        .route(
            "/boards/{id}/versions/{vid}",
            get(version_state).patch(rename_version),
        )
        .route("/boards/{id}/versions/{vid}/restore", post(restore_version))
        .route("/notifications", get(notifications))
        .route("/notifications/read", post(mark_read))
}

// ---- エラー ----

pub struct Error(StatusCode, String);

impl Error {
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self(status, msg.into())
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "ログインしてください")
    }
    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "この操作をする権限がありません")
    }
    pub fn status(&self) -> StatusCode {
        self.0
    }
    pub fn message(&self) -> &str {
        &self.1
    }
    pub fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "ボードが見つからないか、開く権限がありません",
        )
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiError { error: self.1 })).into_response()
    }
}

impl From<StoreError> for Error {
    fn from(e: StoreError) -> Self {
        tracing::error!("保存先のエラー: {e}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "サーバーでエラーが起きました",
        )
    }
}

impl From<AuthError> for Error {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Invalid(m) => Self::bad_request(m),
            AuthError::Internal(e) => e.into(),
        }
    }
}

impl From<CommentError> for Error {
    fn from(e: CommentError) -> Self {
        match e {
            CommentError::Invalid(m) => Self::bad_request(m),
            CommentError::NotFound => Self::new(StatusCode::NOT_FOUND, "コメントが見つかりません"),
            CommentError::Forbidden => Self::forbidden(),
            CommentError::Store(e) => e.into(),
        }
    }
}

type ApiResult<T> = Result<T, Error>;

// ---- ログイン状態 ----

fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|h| auth::cookie_value(h, SESSION_COOKIE))
}

/// Cookie からログイン中のユーザーを引く。
pub fn current_user(state: &AppState, headers: &HeaderMap) -> ApiResult<Option<UserRecord>> {
    match session_token(headers) {
        Some(t) => Ok(auth::session_user(state.store(), t)?),
        None => Ok(None),
    }
}

fn require_user(state: &AppState, headers: &HeaderMap) -> ApiResult<UserRecord> {
    current_user(state, headers)?.ok_or_else(Error::unauthorized)
}

/// ボードの情報と、このリクエストの権限。開けなければ 404。
pub fn board_access(
    state: &AppState,
    id: &str,
    login: Option<&str>,
    key: Option<&str>,
) -> ApiResult<(Option<BoardMeta>, Role)> {
    if !valid_room_name(id) {
        return Err(Error::not_found());
    }
    let meta = boards::get(state.store(), id)?;
    let role = boards::role_of(
        meta.as_ref(),
        Visitor { login, key },
        state.config.anonymous_boards,
    )
    .ok_or_else(Error::not_found)?;
    Ok((meta, role))
}

/// オーナーのいるボードで、指定の権限以上が必要な操作。
fn owned_board(
    state: &AppState,
    id: &str,
    user: &UserRecord,
    key: Option<&str>,
    need: Role,
) -> ApiResult<BoardMeta> {
    let (meta, role) = board_access(state, id, Some(&user.login), key)?;
    let meta =
        meta.ok_or_else(|| Error::bad_request("誰でも編集できるボードには共有設定がありません"))?;
    if role < need {
        return Err(Error::forbidden());
    }
    Ok(meta)
}

#[derive(Deserialize)]
pub struct KeyQuery {
    pub key: Option<String>,
}

fn with_cookie<T: IntoResponse>(cookie: String, body: T) -> Response {
    let mut res = body.into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        res.headers_mut().insert(SET_COOKIE, v);
    }
    res
}

/// 重い処理(パスワードのハッシュ)を別スレッドで。
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, AuthError> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::from(StoreError::from(e.to_string())))?
        .map_err(Error::from)
}

// ---- アカウント ----

async fn signup(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Signup>,
) -> ApiResult<Response> {
    let store = state.store().clone();
    let user = blocking(move || auth::signup(&store, &req.login, &req.name, &req.password)).await?;
    tracing::info!(login = %user.login, "アカウント作成");
    let token = auth::create_session(state.store(), &user.login)?;
    Ok(with_cookie(
        auth::session_cookie(&token, state.config.secure_cookies),
        Json(user.me()),
    ))
}

async fn login(State(state): State<Arc<AppState>>, Json(req): Json<Login>) -> ApiResult<Response> {
    let store = state.store().clone();
    let user = blocking(move || auth::check_login(&store, &req.login, &req.password)).await?;
    let token = auth::create_session(state.store(), &user.login)?;
    Ok(with_cookie(
        auth::session_cookie(&token, state.config.secure_cookies),
        Json(user.me()),
    ))
}

async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(t) = session_token(&headers) {
        auth::delete_session(state.store(), t)?;
    }
    Ok(with_cookie(
        auth::clear_cookie(state.config.secure_cookies),
        StatusCode::NO_CONTENT,
    ))
}

async fn me(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Me>> {
    Ok(Json(require_user(&state, &headers)?.me()))
}

// ---- ボード ----

async fn list_boards(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<BoardInfo>>> {
    let user = require_user(&state, &headers)?;
    Ok(Json(boards::list_for(
        state.store(),
        &user.login,
        state.config.anonymous_boards,
    )?))
}

async fn create_board(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<NewBoard>,
) -> ApiResult<(StatusCode, Json<BoardInfo>)> {
    let user = require_user(&state, &headers)?;
    let meta = boards::create(state.store(), &user.login, &req.title)?;
    tracing::info!(board = %meta.id, owner = %user.login, "ボード作成");
    Ok((StatusCode::CREATED, Json(meta.info(Role::Owner, None))))
}

async fn board_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<BoardInfo>> {
    let user = current_user(&state, &headers)?;
    let login = user.as_ref().map(|u| u.login.as_str());
    let (meta, role) = board_access(&state, &id, login, q.key.as_deref())?;
    // 開いたボードは一覧に載せる。
    if let Some(login) = login {
        boards::remember(state.store(), login, &id, q.key.as_deref())?;
    }
    Ok(Json(match meta {
        Some(m) => m.info(role, q.key.filter(|_| role < Role::Owner)),
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

async fn rename_board(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<RenameBoard>,
) -> ApiResult<Json<BoardInfo>> {
    let user = require_user(&state, &headers)?;
    let (meta, role) = board_access(&state, &id, Some(&user.login), q.key.as_deref())?;
    let Some(mut meta) = meta else {
        return Err(Error::bad_request(
            "誰でも編集できるボードの名前は変えられません",
        ));
    };
    if !role.can_edit() {
        return Err(Error::forbidden());
    }
    meta.title = boards::clean_title(&req.title);
    meta.updated = auth::now_ms();
    boards::put(state.store(), &meta)?;
    Ok(Json(meta.info(role, q.key)))
}

async fn delete_board(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let user = require_user(&state, &headers)?;
    let mut meta = owned_board(&state, &id, &user, None, Role::Owner)?;
    meta.deleted = true;
    meta.members.clear();
    meta.link = None;
    meta.updated = auth::now_ms();
    boards::put(state.store(), &meta)?;
    boards::forget(state.store(), &user.login, &id)?;
    comments::delete_all(state.store(), &id)?;
    versions::delete_all(state.store(), &id)?;
    state.hub.delete(&id)?;
    tracing::info!(board = %id, "ボード削除");
    Ok(StatusCode::NO_CONTENT)
}

async fn sharing(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Sharing>> {
    let user = require_user(&state, &headers)?;
    Ok(Json(
        owned_board(&state, &id, &user, None, Role::Owner)?.sharing(),
    ))
}

async fn set_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SetMember>,
) -> ApiResult<Json<Sharing>> {
    let user = require_user(&state, &headers)?;
    let mut meta = owned_board(&state, &id, &user, None, Role::Owner)?;
    let login = req
        .login
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase();
    if Some(&login) == meta.owner.as_ref() {
        return Err(Error::bad_request("オーナーの権限は変えられません"));
    }
    match req.role {
        Some(role) => {
            if auth::get_user(state.store(), &login)?.is_none() {
                return Err(Error::bad_request(format!(
                    "「{login}」というユーザーはいません"
                )));
            }
            meta.members.insert(login.clone(), role.min(Role::Editor));
            boards::remember(state.store(), &login, &id, None)?;
        }
        None => {
            meta.members.remove(&login);
        }
    }
    meta.updated = auth::now_ms();
    boards::put(state.store(), &meta)?;
    // 権限が下がった人がいるかもしれないので、つなぎ直させて確かめ直す。
    state.hub.kick(&id);
    Ok(Json(meta.sharing()))
}

async fn set_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SetLink>,
) -> ApiResult<Json<Sharing>> {
    let user = require_user(&state, &headers)?;
    let mut meta = owned_board(&state, &id, &user, None, Role::Owner)?;
    boards::set_link(&mut meta, req.role, req.regenerate);
    meta.updated = auth::now_ms();
    boards::put(state.store(), &meta)?;
    state.hub.kick(&id);
    Ok(Json(meta.sharing()))
}

// ---- コメント ----

async fn list_comments(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<Vec<Comment>>> {
    let user = current_user(&state, &headers)?;
    board_access(
        &state,
        &id,
        user.as_ref().map(|u| u.login.as_str()),
        q.key.as_deref(),
    )?;
    Ok(Json(comments::list(state.store(), &id)?))
}

async fn post_comment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<NewComment>,
) -> ApiResult<(StatusCode, Json<Posted>)> {
    let user = require_user(&state, &headers)?;
    let (meta, role) = board_access(&state, &id, Some(&user.login), q.key.as_deref())?;
    if !role.can_comment() {
        return Err(Error::forbidden());
    }
    let posted = comments::post(
        state.store(),
        meta.as_ref(),
        &id,
        &user,
        req,
        state.config.anonymous_boards,
    )?;
    // コメントしたボードは一覧に載せる。
    boards::remember(state.store(), &user.login, &id, q.key.as_deref())?;
    state.hub.refresh(&id, TOPIC_COMMENTS);
    Ok((StatusCode::CREATED, Json(posted)))
}

async fn edit_comment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, cid)): Path<(String, u64)>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<EditComment>,
) -> ApiResult<Json<Comment>> {
    let user = require_user(&state, &headers)?;
    let (_, role) = board_access(&state, &id, Some(&user.login), q.key.as_deref())?;
    if !role.can_comment() {
        return Err(Error::forbidden());
    }
    let c = comments::edit(state.store(), &id, cid, &user, role, req)?;
    state.hub.refresh(&id, TOPIC_COMMENTS);
    Ok(Json(c))
}

async fn delete_comment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, cid)): Path<(String, u64)>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<StatusCode> {
    let user = require_user(&state, &headers)?;
    let (_, role) = board_access(&state, &id, Some(&user.login), q.key.as_deref())?;
    comments::delete(state.store(), &id, cid, &user, role)?;
    state.hub.refresh(&id, TOPIC_COMMENTS);
    Ok(StatusCode::NO_CONTENT)
}

async fn notifications(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<Notification>>> {
    let user = require_user(&state, &headers)?;
    Ok(Json(comments::notifications(state.store(), &user.login)?))
}

async fn mark_read(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<MarkRead>,
) -> ApiResult<StatusCode> {
    let user = require_user(&state, &headers)?;
    comments::mark_read(state.store(), &user.login, req.ids.as_deref())?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- バージョン履歴 ----

fn version_not_found() -> Error {
    Error::new(StatusCode::NOT_FOUND, "バージョンが見つかりません")
}

/// 編集者以上か確かめる。
fn require_editor(
    state: &AppState,
    id: &str,
    user: Option<&UserRecord>,
    key: Option<&str>,
) -> ApiResult<()> {
    let (_, role) = board_access(state, id, user.map(|u| u.login.as_str()), key)?;
    if !role.can_edit() {
        return Err(Error::forbidden());
    }
    Ok(())
}

async fn list_versions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<Vec<VersionInfo>>> {
    let user = current_user(&state, &headers)?;
    board_access(
        &state,
        &id,
        user.as_ref().map(|u| u.login.as_str()),
        q.key.as_deref(),
    )?;
    Ok(Json(versions::list(state.store(), &id)?))
}

async fn create_version(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<NewVersion>,
) -> ApiResult<(StatusCode, Json<VersionInfo>)> {
    let user = current_user(&state, &headers)?;
    require_editor(&state, &id, user.as_ref(), q.key.as_deref())?;
    let current = state
        .hub
        .current_state(&id)?
        .ok_or_else(|| Error::bad_request("まだ中身のないボードです"))?;
    let label = req.label.unwrap_or_else(|| "名前のないバージョン".into());
    let info = versions::create(state.store(), &id, &current, Some(&label), user.as_ref())?;
    state.hub.refresh(&id, TOPIC_VERSIONS);
    Ok((StatusCode::CREATED, Json(info)))
}

async fn version_state(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, vid)): Path<(String, u64)>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Response> {
    let user = current_user(&state, &headers)?;
    board_access(
        &state,
        &id,
        user.as_ref().map(|u| u.login.as_str()),
        q.key.as_deref(),
    )?;
    let bytes = versions::state(state.store(), &id, vid)?.ok_or_else(version_not_found)?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    )
        .into_response())
}

async fn rename_version(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, vid)): Path<(String, u64)>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<NewVersion>,
) -> ApiResult<Json<VersionInfo>> {
    let user = current_user(&state, &headers)?;
    require_editor(&state, &id, user.as_ref(), q.key.as_deref())?;
    let info = versions::rename(state.store(), &id, vid, req.label.as_deref())?
        .ok_or_else(version_not_found)?;
    state.hub.refresh(&id, TOPIC_VERSIONS);
    Ok(Json(info))
}

async fn restore_version(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, vid)): Path<(String, u64)>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<VersionInfo>> {
    let user = current_user(&state, &headers)?;
    require_editor(&state, &id, user.as_ref(), q.key.as_deref())?;
    let target = versions::get_info(state.store(), &id, vid)?.ok_or_else(version_not_found)?;
    let old = versions::state(state.store(), &id, vid)?.ok_or_else(version_not_found)?;
    // 戻す前の状態も残しておく(戻したことを取り消せるように)。
    let backup = match state.hub.current_state(&id)? {
        Some(current) => {
            let label = format!(
                "「{}」に戻す前",
                target.label.as_deref().unwrap_or("自動保存")
            );
            Some(versions::create(
                state.store(),
                &id,
                &current,
                Some(&label),
                user.as_ref(),
            )?)
        }
        None => None,
    };
    let hub = state.hub.clone();
    let room = id.clone();
    tokio::task::spawn_blocking(move || hub.restore(&room, &old))
        .await
        .map_err(|e| Error::from(StoreError::from(e.to_string())))?
        .map_err(|e| Error::bad_request(format!("戻せませんでした: {e}")))?;
    tracing::info!(board = %id, version = vid, "バージョンを復元");
    state.hub.refresh(&id, TOPIC_VERSIONS);
    Ok(Json(backup.unwrap_or(target)))
}
