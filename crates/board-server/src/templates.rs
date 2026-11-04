//! 自作テンプレートとテンプレートギャラリー。
//!
//! 選んだ要素を名前を付けて保存し、自分だけで使うか、チーム(組織)に公開する。
//! チームに公開したテンプレートはメンバー全員のギャラリーに出て、誰でもボードに置ける。
//! 変更・公開先の切り替え・削除は作った人と、そのチームの管理者・オーナーができる。
//!
//! 中身はクリップボードと同じ形式([`board_core::arrange::copy`])で、縮小図は保存時に作る。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{self, patch, post};
use board_core::api::{EditUserTemplate, NewUserTemplate, UserTemplate, UserTemplateData};
use board_core::gallery::{self, MAX_CATEGORY_CHARS, MAX_DESCRIPTION_CHARS, MAX_TITLE_CHARS};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::{Error, require_user};
use crate::audit::{self, Entry};
use crate::auth::{self, now_ms, random_id};
use crate::store::{Store, StoreError, TEAM_TEMPLATES, TEMPLATE_DATA, TEMPLATES, USER_TEMPLATES};
use crate::teams;

/// 1 人が持てるテンプレートの数。
pub const MAX_TEMPLATES_PER_USER: usize = 200;

/// 保存するテンプレートの情報(中身は `template_data` に分けて持つ)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemplateRecord {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub created: u64,
    pub updated: u64,
    #[serde(default)]
    pub uses: u64,
    #[serde(default)]
    pub elements: usize,
    #[serde(default)]
    pub preview: String,
}

/// 使われた回数などの読み書きが重ならないようにする。
static WRITE: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    WRITE.lock().unwrap_or_else(|e| e.into_inner())
}

fn valid_id(id: &str) -> bool {
    crate::valid_room_name(id)
}

pub fn get(store: &Store, id: &str) -> Result<Option<TemplateRecord>, StoreError> {
    if !valid_id(id) {
        return Ok(None);
    }
    store.get_json(TEMPLATES, id)
}

fn put(store: &Store, t: &TemplateRecord) -> Result<(), StoreError> {
    store.put_json(TEMPLATES, &t.id, t)
}

fn data_of(store: &Store, id: &str) -> Result<Option<String>, StoreError> {
    store.get_json(TEMPLATE_DATA, id)
}

/// 公開先を `team` に付け替える(索引も合わせる)。
fn set_team(store: &Store, t: &mut TemplateRecord, team: Option<String>) -> Result<(), StoreError> {
    if let Some(old) = &t.team {
        store.remove_pair(TEAM_TEMPLATES, old, &t.id)?;
    }
    if let Some(new) = &team {
        store.put_pair(TEAM_TEMPLATES, new, &t.id, &())?;
    }
    t.team = team;
    Ok(())
}

/// テンプレートを保存する。
pub fn create(
    store: &Store,
    owner: &str,
    req: &NewUserTemplate,
) -> Result<TemplateRecord, CreateError> {
    let title = gallery::clean_line(&req.title, MAX_TITLE_CHARS);
    if title.is_empty() {
        return Err(CreateError::Invalid(
            "テンプレートの名前を入れてください".into(),
        ));
    }
    let elements = gallery::parse(&req.data).map_err(CreateError::Invalid)?;
    if store.scan_pair::<()>(USER_TEMPLATES, owner)?.len() >= MAX_TEMPLATES_PER_USER {
        return Err(CreateError::Invalid(format!(
            "テンプレートは 1 人 {MAX_TEMPLATES_PER_USER} 個までです。使わないものを消してください"
        )));
    }
    let now = now_ms();
    let _g = lock();
    loop {
        let mut t = TemplateRecord {
            id: random_id(12),
            title: title.clone(),
            description: gallery::clean_line(&req.description, MAX_DESCRIPTION_CHARS),
            category: gallery::clean_line(&req.category, MAX_CATEGORY_CHARS),
            owner: owner.to_string(),
            team: None,
            created: now,
            updated: now,
            uses: 0,
            elements: elements.len(),
            preview: gallery::preview_svg(&elements),
        };
        if store.insert_new_json(TEMPLATES, &t.id, &t)? {
            store.put_json(TEMPLATE_DATA, &t.id, &req.data)?;
            store.put_pair(USER_TEMPLATES, owner, &t.id, &())?;
            if req.team.is_some() {
                set_team(store, &mut t, req.team.clone())?;
                put(store, &t)?;
            }
            return Ok(t);
        }
    }
}

#[derive(Debug)]
pub enum CreateError {
    Invalid(String),
    Store(StoreError),
}

impl From<StoreError> for CreateError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<CreateError> for Error {
    fn from(e: CreateError) -> Self {
        match e {
            CreateError::Invalid(m) => Error::bad_request(m),
            CreateError::Store(e) => e.into(),
        }
    }
}

/// テンプレートを消す。
pub fn delete(store: &Store, t: &TemplateRecord) -> Result<(), StoreError> {
    let _g = lock();
    if let Some(team) = &t.team {
        store.remove_pair(TEAM_TEMPLATES, team, &t.id)?;
    }
    store.remove_pair(USER_TEMPLATES, &t.owner, &t.id)?;
    store.remove(TEMPLATE_DATA, &t.id)?;
    store.remove(TEMPLATES, &t.id)?;
    Ok(())
}

/// チームがなくなったとき([`teams::delete`])、公開していたテンプレートを作った人だけのものに戻す。
pub fn detach_team(store: &Store, team: &str) -> Result<usize, StoreError> {
    let _g = lock();
    let ids = store.scan_pair::<()>(TEAM_TEMPLATES, team)?;
    for (id, ()) in &ids {
        if let Some(mut t) = get(store, id)? {
            t.team = None;
            put(store, &t)?;
        }
        store.remove_pair(TEAM_TEMPLATES, team, id)?;
    }
    Ok(ids.len())
}

/// `login` が見られるか。
fn can_view(store: &Store, t: &TemplateRecord, login: &str) -> Result<bool, StoreError> {
    if t.owner == login {
        return Ok(true);
    }
    match &t.team {
        Some(team) => Ok(teams::role_in(store, team, login)?.is_some()),
        None => Ok(false),
    }
}

/// `login` が変更・削除できるか(作った人か、公開先のチームの管理者以上)。
fn can_edit(store: &Store, t: &TemplateRecord, login: &str) -> Result<bool, StoreError> {
    if t.owner == login {
        return Ok(true);
    }
    match &t.team {
        Some(team) => Ok(teams::role_in(store, team, login)?.is_some_and(|r| r.is_admin())),
        None => Ok(false),
    }
}

/// 画面に出す形にする。
struct Viewer<'a> {
    store: &'a Store,
    login: &'a str,
    names: BTreeMap<String, String>,
    team_names: BTreeMap<String, String>,
}

impl<'a> Viewer<'a> {
    fn new(store: &'a Store, login: &'a str) -> Self {
        Self {
            store,
            login,
            names: BTreeMap::new(),
            team_names: BTreeMap::new(),
        }
    }

    fn view(&mut self, t: &TemplateRecord) -> Result<UserTemplate, StoreError> {
        let store = self.store;
        let owner_name = match self.names.get(&t.owner) {
            Some(n) => n.clone(),
            None => {
                let n = auth::get_user(store, &t.owner)?
                    .map(|u| u.name)
                    .unwrap_or_else(|| t.owner.clone());
                self.names.insert(t.owner.clone(), n.clone());
                n
            }
        };
        let team_name = match &t.team {
            Some(id) => Some(match self.team_names.get(id) {
                Some(n) => n.clone(),
                None => {
                    let n = teams::get_team(store, id)?
                        .map(|t| t.name)
                        .unwrap_or_else(|| id.clone());
                    self.team_names.insert(id.clone(), n.clone());
                    n
                }
            }),
            None => None,
        };
        Ok(UserTemplate {
            id: t.id.clone(),
            title: t.title.clone(),
            description: t.description.clone(),
            category: t.category.clone(),
            owner: t.owner.clone(),
            owner_name,
            team: t.team.clone(),
            team_name,
            created: t.created,
            updated: t.updated,
            uses: t.uses,
            elements: t.elements,
            preview: t.preview.clone(),
            can_edit: can_edit(store, t, self.login)?,
        })
    }
}

/// `login` が使えるテンプレート(自分のものと、入っているチームに公開されたもの)。
/// よく使われる順、同じなら新しい順。
pub fn list_for(store: &Store, login: &str) -> Result<Vec<UserTemplate>, StoreError> {
    let mut ids: Vec<String> = store
        .scan_pair::<()>(USER_TEMPLATES, login)?
        .into_iter()
        .map(|(id, ())| id)
        .collect();
    for team in teams::team_ids_of(store, login)? {
        for (id, ()) in store.scan_pair::<()>(TEAM_TEMPLATES, &team)? {
            ids.push(id);
        }
    }
    ids.sort();
    ids.dedup();
    let mut v = Viewer::new(store, login);
    let mut out = Vec::new();
    for id in ids {
        if let Some(t) = get(store, &id)?
            && can_view(store, &t, login)?
        {
            out.push(v.view(&t)?);
        }
    }
    out.sort_by(|a, b| {
        b.uses
            .cmp(&a.uses)
            .then(b.updated.cmp(&a.updated))
            .then(a.id.cmp(&b.id))
    });
    Ok(out)
}

// ---- HTTP ----

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/templates",
            routing::get(list_templates)
                .post(create_template)
                .layer(DefaultBodyLimit::max(gallery::MAX_DATA_BYTES + 64 * 1024)),
        )
        .route(
            "/templates/{id}",
            patch(edit_template).delete(delete_template),
        )
        .route("/templates/{id}/use", post(use_template))
}

fn not_found() -> Error {
    Error::new(StatusCode::NOT_FOUND, "テンプレートが見つかりません")
}

/// 公開先として指定されたチームのメンバーか確かめる。
fn check_team(store: &Store, team: &str, login: &str) -> Result<(), Error> {
    if teams::role_in(store, team, login)?.is_none() {
        return Err(Error::bad_request(
            "公開先のチームが見つからないか、メンバーではありません",
        ));
    }
    Ok(())
}

async fn list_templates(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<UserTemplate>>, Error> {
    let user = require_user(&state, &headers)?;
    Ok(Json(list_for(state.store(), &user.login)?))
}

async fn create_template(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(mut req): Json<NewUserTemplate>,
) -> Result<(StatusCode, Json<UserTemplate>), Error> {
    let user = require_user(&state, &headers)?;
    req.team = req.team.filter(|t| !t.is_empty());
    if let Some(team) = &req.team {
        check_team(state.store(), team, &user.login)?;
    }
    let store = state.store().clone();
    let login = user.login.clone();
    // 中身を読んで縮小図を作るのは重いことがあるので、別のスレッドで。
    let t = tokio::task::spawn_blocking(move || create(&store, &login, &req))
        .await
        .map_err(|e| StoreError::from(e.to_string()))??;
    tracing::info!(template = %t.id, owner = %user.login, team = ?t.team, "テンプレート作成");
    audit::record(
        &state,
        &headers,
        Entry::new("template.create")
            .actor(&user.login)
            .team(t.team.as_deref())
            .target(&t.id)
            .detail(&t.title),
    );
    let view = Viewer::new(state.store(), &user.login).view(&t)?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn edit_template(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<EditUserTemplate>,
) -> Result<Json<UserTemplate>, Error> {
    let user = require_user(&state, &headers)?;
    let store = state.store();
    let mut t = get(store, &id)?.ok_or_else(not_found)?;
    if !can_view(store, &t, &user.login)? {
        return Err(not_found());
    }
    if !can_edit(store, &t, &user.login)? {
        return Err(Error::forbidden());
    }
    let mut changes = Vec::new();
    if let Some(title) = &req.title {
        let title = gallery::clean_line(title, MAX_TITLE_CHARS);
        if title.is_empty() {
            return Err(Error::bad_request("テンプレートの名前を入れてください"));
        }
        if title != t.title {
            changes.push(format!("名前: {title}"));
            t.title = title;
        }
    }
    if let Some(d) = &req.description {
        t.description = gallery::clean_line(d, MAX_DESCRIPTION_CHARS);
    }
    if let Some(c) = &req.category {
        t.category = gallery::clean_line(c, MAX_CATEGORY_CHARS);
    }
    let _g = lock();
    // 書き込みの前に読み直す(使われた回数を消さないように)。
    let fresh = get(store, &id)?.ok_or_else(not_found)?;
    t.uses = fresh.uses;
    if let Some(team) = &req.team {
        let team = Some(team.clone()).filter(|t| !t.is_empty());
        if team != t.team {
            // 別のチームに公開し直すのは、作った人だけ(管理者は自分のチームから外すことだけできる)。
            if let Some(new) = &team {
                if t.owner != user.login {
                    return Err(Error::forbidden());
                }
                check_team(store, new, &user.login)?;
            }
            changes.push(match &team {
                Some(new) => format!("公開先: チーム {new}"),
                None => "公開先: 作った人だけ".into(),
            });
            let old = t.team.clone();
            set_team(store, &mut t, team)?;
            // 外したチームの操作ログにも残す。
            if let Some(old) = old {
                audit::record(
                    &state,
                    &headers,
                    Entry::new("template.update")
                        .actor(&user.login)
                        .team(Some(&old))
                        .target(&t.id)
                        .detail(format!("{}(このチームへの公開をやめた)", t.title)),
                );
            }
        }
    }
    t.updated = now_ms();
    put(store, &t)?;
    drop(_g);
    if !changes.is_empty() {
        audit::record(
            &state,
            &headers,
            Entry::new("template.update")
                .actor(&user.login)
                .team(t.team.as_deref())
                .target(&t.id)
                .detail(changes.join("、")),
        );
    }
    Ok(Json(Viewer::new(store, &user.login).view(&t)?))
}

async fn delete_template(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, Error> {
    let user = require_user(&state, &headers)?;
    let store = state.store();
    let t = get(store, &id)?.ok_or_else(not_found)?;
    if !can_view(store, &t, &user.login)? {
        return Err(not_found());
    }
    if !can_edit(store, &t, &user.login)? {
        return Err(Error::forbidden());
    }
    delete(store, &t)?;
    audit::record(
        &state,
        &headers,
        Entry::new("template.delete")
            .actor(&user.login)
            .team(t.team.as_deref())
            .target(&t.id)
            .detail(&t.title),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// 中身を返し、使われた回数を 1 つ増やす。
async fn use_template(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<UserTemplateData>, Error> {
    let user = require_user(&state, &headers)?;
    let store = state.store();
    let t = {
        let _g = lock();
        let mut t = get(store, &id)?.ok_or_else(not_found)?;
        if !can_view(store, &t, &user.login)? {
            return Err(not_found());
        }
        t.uses += 1;
        put(store, &t)?;
        t
    };
    let data = data_of(store, &id)?.ok_or_else(not_found)?;
    Ok(Json(UserTemplateData {
        template: Viewer::new(store, &user.login).view(&t)?,
        data,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::api::TeamRole;
    use board_core::templates::Template;
    use board_core::{Board, Vec2, arrange};

    fn data() -> String {
        let mut b = Board::new();
        let ids = board_core::templates::insert(&mut b, Template::Kpt, Vec2::ZERO);
        arrange::copy(&b, &ids).unwrap()
    }

    fn new(title: &str, team: Option<&str>) -> NewUserTemplate {
        NewUserTemplate {
            title: title.into(),
            description: "説明".into(),
            category: "振り返り".into(),
            team: team.map(str::to_string),
            data: data(),
        }
    }

    #[test]
    fn personal_and_team_visibility() {
        let store = Store::memory();
        let team = teams::create(&store, "alice", "開発部").unwrap();
        let mut t2 = team.clone();
        teams::change_member(&mut t2, "alice", "bob", Some(TeamRole::Member)).unwrap();
        teams::save_member_change(&store, &t2, "bob").unwrap();

        let mine = create(&store, "alice", &new("  自分用  ", None)).unwrap();
        assert_eq!(mine.title, "自分用");
        assert!(mine.preview.starts_with("<svg"));
        let shared = create(&store, "alice", &new("チームの KPT", Some(&team.id))).unwrap();

        // alice には両方、bob にはチームのものだけ、carol には何も出ない。
        assert_eq!(list_for(&store, "alice").unwrap().len(), 2);
        let bobs = list_for(&store, "bob").unwrap();
        assert_eq!(bobs.len(), 1);
        assert_eq!(bobs[0].id, shared.id);
        assert_eq!(bobs[0].team_name.as_deref(), Some("開発部"));
        assert!(!bobs[0].can_edit);
        assert!(list_for(&store, "carol").unwrap().is_empty());

        // チームがなくなると作った人だけのものに戻る。
        let t = teams::get_team(&store, &team.id).unwrap().unwrap();
        teams::delete(&store, &t).unwrap();
        assert!(list_for(&store, "bob").unwrap().is_empty());
        let alices = list_for(&store, "alice").unwrap();
        assert_eq!(alices.len(), 2);
        assert!(alices.iter().all(|t| t.team.is_none()));

        delete(&store, &mine).unwrap();
        assert_eq!(list_for(&store, "alice").unwrap().len(), 1);
        assert!(data_of(&store, &mine.id).unwrap().is_none());
    }

    #[test]
    fn rejects_empty_title_and_bad_data() {
        let store = Store::memory();
        assert!(matches!(
            create(&store, "alice", &new("  ", None)),
            Err(CreateError::Invalid(_))
        ));
        let mut bad = new("x", None);
        bad.data = "not a template".into();
        assert!(matches!(
            create(&store, "alice", &bad),
            Err(CreateError::Invalid(_))
        ));
    }
}
