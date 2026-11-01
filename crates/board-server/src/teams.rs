//! チーム(ワークスペース)。
//!
//! チームにはメンバーと役割([`TeamRole`])があり、チームに属するボードはメンバー全員が開ける。
//! 権限の計算は [`boards::role_for`](crate::boards::role_for) がチームの役割も見て行う。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, put};
use board_core::api::{
    NewTeam, SetTeamMember, SetTeamStorage, StorageLocation, Team, TeamInfo, TeamRole, TeamSso,
};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::{Error, require_user};
use crate::audit::{self, Entry};
use crate::auth::{self, now_ms, random_id};
use crate::boards;
use crate::store::{Store, StoreError, TEAMS, USER_TEAMS};

pub const MAX_TEAM_NAME_CHARS: usize = 60;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TeamRecord {
    pub id: String,
    pub name: String,
    pub created: u64,
    pub members: BTreeMap<String, TeamRole>,
    /// SSO 自動参加ドメイン(サーバー管理者が設定する)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sso_domains: Vec<String>,
    /// ボードの中身を置く場所([`crate::storage`])。`None` はこのサーバー。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    /// チームのボードで直接接続(P2P)を止めているか([`crate::p2p`])。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub p2p_disabled: bool,
}

impl TeamRecord {
    pub fn view(&self, role: TeamRole) -> Team {
        Team {
            id: self.id.clone(),
            name: self.name.clone(),
            role,
            members: self.members.clone(),
            created: self.created,
            sso_domains: self.sso_domains.clone(),
            storage: self.storage.clone(),
            p2p_disabled: self.p2p_disabled,
        }
    }

    fn owners(&self) -> usize {
        self.members
            .values()
            .filter(|r| **r == TeamRole::Owner)
            .count()
    }
}

/// チーム ID として受け付ける形か(英数字と `-` `_`)。
pub fn valid_team_id(id: &str) -> bool {
    crate::valid_room_name(id)
}

pub fn clean_team_name(name: &str) -> Option<String> {
    let n: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEAM_NAME_CHARS)
        .collect();
    (!n.is_empty()).then_some(n)
}

pub fn get_team(store: &Store, id: &str) -> Result<Option<TeamRecord>, StoreError> {
    if !valid_team_id(id) {
        return Ok(None);
    }
    store.get_json(TEAMS, id)
}

pub fn put_team(store: &Store, team: &TeamRecord) -> Result<(), StoreError> {
    store.put_json(TEAMS, &team.id, team)
}

/// チームでの役割。メンバーでなければ `None`。
pub fn role_in(store: &Store, team: &str, login: &str) -> Result<Option<TeamRole>, StoreError> {
    Ok(get_team(store, team)?.and_then(|t| t.members.get(login).copied()))
}

/// すべてのチーム。
pub fn all(store: &Store) -> Result<Vec<TeamRecord>, StoreError> {
    store.scan_all_json(TEAMS)
}

/// 入っているチームの ID。
pub fn team_ids_of(store: &Store, login: &str) -> Result<Vec<String>, StoreError> {
    Ok(store
        .scan_pair::<u64>(USER_TEAMS, login)?
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

/// 入っているチームの一覧(名前順)。
pub fn list_for(store: &Store, login: &str) -> Result<Vec<TeamInfo>, StoreError> {
    let mut out = Vec::new();
    for id in team_ids_of(store, login)? {
        if let Some(t) = get_team(store, &id)?
            && let Some(role) = t.members.get(login)
        {
            out.push(t.view(*role).info());
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(out)
}

/// チームを作る。作った人がオーナーになる。
pub fn create(store: &Store, owner: &str, name: &str) -> Result<TeamRecord, StoreError> {
    let name = clean_team_name(name).unwrap_or_else(|| "無題のチーム".into());
    loop {
        let team = TeamRecord {
            id: random_id(12),
            name: name.clone(),
            created: now_ms(),
            members: BTreeMap::from([(owner.to_string(), TeamRole::Owner)]),
            sso_domains: Vec::new(),
            storage: None,
            p2p_disabled: false,
        };
        if store.insert_new_json(TEAMS, &team.id, &team)? {
            store.put_pair(USER_TEAMS, owner, &team.id, &now_ms())?;
            return Ok(team);
        }
    }
}

/// メンバーの変更でできないこと。
#[derive(Debug, PartialEq)]
pub enum MemberError {
    Forbidden,
    Invalid(String),
}

/// `actor` が `login` の役割を `role` にする(`None` なら外す)。成功したら変更後のチームを返す。
///
/// - 管理者はメンバーの追加・削除ができる。管理者・オーナーの任命と解任はオーナーだけ。
/// - 自分を外す(退出)のは誰でもできる。
/// - オーナーが 1 人もいなくなる変更はできない。
pub fn change_member(
    team: &mut TeamRecord,
    actor: &str,
    login: &str,
    role: Option<TeamRole>,
) -> Result<(), MemberError> {
    let actor_role = team
        .members
        .get(actor)
        .copied()
        .ok_or(MemberError::Forbidden)?;
    let current = team.members.get(login).copied();
    let leaving_self = actor == login && role.is_none();
    if !leaving_self {
        if !actor_role.is_admin() {
            return Err(MemberError::Forbidden);
        }
        // 管理者以上が関わる変更はオーナーだけ。
        let touches_admin =
            current.is_some_and(TeamRole::is_admin) || role.is_some_and(TeamRole::is_admin);
        if touches_admin && actor_role != TeamRole::Owner {
            return Err(MemberError::Forbidden);
        }
    }
    if current == Some(TeamRole::Owner) && role != Some(TeamRole::Owner) && team.owners() <= 1 {
        return Err(MemberError::Invalid(
            "オーナーが 1 人もいなくなります。先にほかの人をオーナーにしてください".into(),
        ));
    }
    match role {
        Some(r) => {
            team.members.insert(login.to_string(), r);
        }
        None => {
            team.members.remove(login);
        }
    }
    Ok(())
}

/// メンバーの変更を保存する(一覧の索引も合わせる)。
pub fn save_member_change(store: &Store, team: &TeamRecord, login: &str) -> Result<(), StoreError> {
    put_team(store, team)?;
    if team.members.contains_key(login) {
        store.put_pair(USER_TEAMS, login, &team.id, &now_ms())?;
    } else {
        store.remove_pair(USER_TEAMS, login, &team.id)?;
    }
    Ok(())
}

/// チームを消す。ボードはチームから外れ、作った人の個人ボードに戻る。
pub fn delete(store: &Store, team: &TeamRecord) -> Result<Vec<String>, StoreError> {
    let ids = boards::team_board_ids(store, &team.id)?;
    for id in &ids {
        if let Some(mut meta) = boards::get(store, id)?
            && meta.team.as_deref() == Some(team.id.as_str())
        {
            meta.team = None;
            meta.team_access = None;
            boards::put(store, &meta)?;
        }
        boards::detach(store, &team.id, id)?;
    }
    for login in team.members.keys() {
        store.remove_pair(USER_TEAMS, login, &team.id)?;
    }
    store.remove(TEAMS, &team.id)?;
    Ok(ids)
}

// ---- HTTP ----

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/teams", get(list_teams).post(create_team))
        .route(
            "/teams/{tid}",
            get(team_detail).patch(rename_team).delete(delete_team),
        )
        .route("/teams/{tid}/members", put(set_member))
        .route("/teams/{tid}/sso", put(set_sso))
        .route("/teams/{tid}/storage", put(set_storage))
        .route("/storage", get(list_locations))
}

fn team_not_found() -> Error {
    Error::new(
        StatusCode::NOT_FOUND,
        "チームが見つからないか、メンバーではありません",
    )
}

/// メンバーとしてチームを引く。
pub fn member_team(store: &Store, tid: &str, login: &str) -> Result<(TeamRecord, TeamRole), Error> {
    let team = get_team(store, tid)?.ok_or_else(team_not_found)?;
    let role = team
        .members
        .get(login)
        .copied()
        .ok_or_else(team_not_found)?;
    Ok((team, role))
}

/// チームのボードにつないでいる人をつなぎ直させる(権限を確かめ直す)。
fn kick_team_boards(state: &AppState, tid: &str) -> Result<(), StoreError> {
    for id in boards::team_board_ids(state.store(), tid)? {
        state.hub.kick(&id);
    }
    Ok(())
}

async fn list_teams(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<TeamInfo>>, Error> {
    let user = require_user(&state, &headers)?;
    Ok(Json(list_for(state.store(), &user.login)?))
}

async fn create_team(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<NewTeam>,
) -> Result<(StatusCode, Json<Team>), Error> {
    let user = require_user(&state, &headers)?;
    let team = create(state.store(), &user.login, &req.name)?;
    tracing::info!(team = %team.id, owner = %user.login, "チーム作成");
    audit::record(
        &state,
        &headers,
        Entry::new("team.create")
            .actor(&user.login)
            .team(Some(&team.id))
            .detail(&team.name),
    );
    Ok((StatusCode::CREATED, Json(team.view(TeamRole::Owner))))
}

async fn team_detail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(tid): Path<String>,
) -> Result<Json<Team>, Error> {
    let user = require_user(&state, &headers)?;
    let (team, role) = member_team(state.store(), &tid, &user.login)?;
    Ok(Json(team.view(role)))
}

async fn rename_team(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(tid): Path<String>,
    Json(req): Json<NewTeam>,
) -> Result<Json<Team>, Error> {
    let user = require_user(&state, &headers)?;
    let (mut team, role) = member_team(state.store(), &tid, &user.login)?;
    if !role.is_admin() {
        return Err(Error::forbidden());
    }
    team.name = clean_team_name(&req.name)
        .ok_or_else(|| Error::bad_request("チームの名前を入れてください"))?;
    put_team(state.store(), &team)?;
    audit::record(
        &state,
        &headers,
        Entry::new("team.rename")
            .actor(&user.login)
            .team(Some(&team.id))
            .detail(&team.name),
    );
    Ok(Json(team.view(role)))
}

async fn delete_team(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(tid): Path<String>,
) -> Result<StatusCode, Error> {
    let user = require_user(&state, &headers)?;
    let (team, role) = member_team(state.store(), &tid, &user.login)?;
    if role != TeamRole::Owner {
        return Err(Error::forbidden());
    }
    // ほかの場所に置いた中身は、個人ボードに戻る前にこのサーバーへ移す。
    if team.storage.is_some() {
        migrate(&state, &tid, None).await?;
    }
    audit::record(
        &state,
        &headers,
        Entry::new("team.delete")
            .actor(&user.login)
            .team(Some(&team.id))
            .detail(&team.name),
    );
    for id in delete(state.store(), &team)? {
        state.hub.kick(&id);
    }
    tracing::info!(team = %tid, "チーム削除");
    Ok(StatusCode::NO_CONTENT)
}

async fn set_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(tid): Path<String>,
    Json(req): Json<SetTeamMember>,
) -> Result<Json<Team>, Error> {
    let user = require_user(&state, &headers)?;
    let (mut team, _) = member_team(state.store(), &tid, &user.login)?;
    let login = req
        .login
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase();
    if req.role.is_some() && auth::get_user(state.store(), &login)?.is_none() {
        return Err(Error::bad_request(format!(
            "「{login}」というユーザーはいません"
        )));
    }
    change_member(&mut team, &user.login, &login, req.role).map_err(|e| match e {
        MemberError::Forbidden => Error::forbidden(),
        MemberError::Invalid(m) => Error::bad_request(m),
    })?;
    save_member_change(state.store(), &team, &login)?;
    audit::record(
        &state,
        &headers,
        Entry::new("team.member")
            .actor(&user.login)
            .team(Some(&tid))
            .target(&login)
            .detail(req.role.map_or("removed", TeamRole::as_str)),
    );
    kick_team_boards(&state, &tid)?;
    // 自分が抜けたあとは自分の役割がない。
    let my_role = team
        .members
        .get(&user.login)
        .copied()
        .unwrap_or(TeamRole::Member);
    Ok(Json(team.view(my_role)))
}

/// SSO 自動参加ドメイン。会社のドメインを勝手に名乗れないよう、サーバー管理者だけが設定できる。
async fn set_sso(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(tid): Path<String>,
    Json(req): Json<TeamSso>,
) -> Result<Json<Team>, Error> {
    let user = require_user(&state, &headers)?;
    if !state.config.is_admin(&user.login) {
        return Err(Error::forbidden());
    }
    let mut team = get_team(state.store(), &tid)?.ok_or_else(team_not_found)?;
    team.sso_domains = crate::oidc::split_domains(&req.domains.join(","));
    team.sso_domains.dedup();
    put_team(state.store(), &team)?;
    audit::record(
        &state,
        &headers,
        Entry::new("team.settings")
            .actor(&user.login)
            .team(Some(&tid))
            .detail(format!("SSO 自動参加: {}", team.sso_domains.join(", "))),
    );
    let role = team
        .members
        .get(&user.login)
        .copied()
        .unwrap_or(TeamRole::Member);
    Ok(Json(team.view(role)))
}

/// 選べる保存場所。先頭は「このサーバー」。
pub fn location_list(store: &Store) -> Vec<StorageLocation> {
    let mut out = vec![StorageLocation {
        id: "local".into(),
        label: "このサーバー".into(),
        description: "グラボードのサーバーのディスク".into(),
    }];
    out.extend(store.locations().iter().map(|l| StorageLocation {
        id: l.id.clone(),
        label: l.label.clone(),
        description: l.description.clone(),
    }));
    out
}

async fn list_locations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<StorageLocation>>, Error> {
    require_user(&state, &headers)?;
    Ok(Json(location_list(state.store())))
}

/// チームのボードの中身を `to` へ移し、チームの保存場所を切り替える。
///
/// 1. すべてのボード(とバージョン)を新しい場所へ写す
/// 2. チームの設定を切り替える(以後の保存は新しい場所へ)
/// 3. メモリ上の変更を書き出す(編集中のボードの最新の状態が新しい場所に入る)
/// 4. 古い場所から消す
pub async fn migrate(state: &AppState, tid: &str, to: Option<String>) -> Result<usize, Error> {
    let store = state.store().clone();
    let hub = state.hub.clone();
    let tid = tid.to_string();
    let result = tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
        let mut team = get_team(&store, &tid)?.ok_or("チームが見つかりません")?;
        let from = match &team.storage {
            Some(id) => Some(
                store
                    .location(id)
                    .ok_or_else(|| format!("保存場所「{id}」が設定されていません"))?,
            ),
            None => None,
        };
        let dest = match &to {
            Some(id) => Some(
                store
                    .location(id)
                    .ok_or_else(|| format!("保存場所「{id}」はありません"))?,
            ),
            None => None,
        };
        if team.storage == to {
            return Ok(0);
        }
        // 今の変更を古い場所に書き出してから写す。
        hub.flush();
        let ids = boards::team_board_ids(&store, &tid)?;
        for id in &ids {
            if let Some(b) = store.load_at(from.as_deref(), id)? {
                store.save_at(dest.as_deref(), id, &b)?;
            }
            for vid in store.seq_ids(crate::store::VERSION_INFO, id)? {
                if let Some(v) = store.version_get_at(from.as_deref(), id, vid)? {
                    store.version_put_at(dest.as_deref(), id, vid, &v)?;
                }
            }
        }
        team.storage = to.clone();
        put_team(&store, &team)?;
        hub.flush();
        for id in &ids {
            store.delete_at(from.as_deref(), id)?;
            for vid in store.seq_ids(crate::store::VERSION_INFO, id)? {
                store.version_delete_at(from.as_deref(), id, vid)?;
            }
        }
        Ok(ids.len())
    })
    .await
    .map_err(|e| StoreError::from(e.to_string()))?;
    result.map_err(|e| {
        tracing::error!("保存場所の移動に失敗: {e}");
        Error::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("保存場所を移せませんでした: {e}"),
        )
    })
}

/// チームのボードの保存場所を変える(チームのオーナーのみ)。
async fn set_storage(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(tid): Path<String>,
    Json(req): Json<SetTeamStorage>,
) -> Result<Json<Team>, Error> {
    let user = require_user(&state, &headers)?;
    let (team, role) = member_team(state.store(), &tid, &user.login)?;
    if role != TeamRole::Owner {
        return Err(Error::forbidden());
    }
    let to = req.location.filter(|l| l != "local");
    if let Some(id) = &to
        && state.store().location(id).is_none()
    {
        return Err(Error::bad_request(format!("保存場所「{id}」はありません")));
    }
    let from = team.storage.clone();
    let moved = migrate(&state, &tid, to.clone()).await?;
    if from != to {
        tracing::info!(team = %tid, ?from, ?to, boards = moved, "保存場所を変更");
        audit::record(
            &state,
            &headers,
            Entry::new("team.storage")
                .actor(&user.login)
                .team(Some(&tid))
                .detail(format!(
                    "{} → {}(ボード {moved} 枚)",
                    from.as_deref().unwrap_or("local"),
                    to.as_deref().unwrap_or("local")
                )),
        );
    }
    let (team, role) = member_team(state.store(), &tid, &user.login)?;
    Ok(Json(team.view(role)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_rules() {
        let store = Store::memory();
        let mut t = create(&store, "alice", "  開発部 ").unwrap();
        assert_eq!(t.name, "開発部");
        assert_eq!(t.members["alice"], TeamRole::Owner);

        // オーナーは管理者を任命できる。
        change_member(&mut t, "alice", "bob", Some(TeamRole::Admin)).unwrap();
        // 管理者はメンバーを足せるが、管理者は任命できない。
        change_member(&mut t, "bob", "carol", Some(TeamRole::Member)).unwrap();
        assert_eq!(
            change_member(&mut t, "bob", "dave", Some(TeamRole::Admin)),
            Err(MemberError::Forbidden)
        );
        assert_eq!(
            change_member(&mut t, "bob", "alice", None),
            Err(MemberError::Forbidden)
        );
        // メンバーは人を足せないが、自分は抜けられる。
        assert_eq!(
            change_member(&mut t, "carol", "dave", Some(TeamRole::Member)),
            Err(MemberError::Forbidden)
        );
        change_member(&mut t, "carol", "carol", None).unwrap();
        assert!(!t.members.contains_key("carol"));
        // 最後のオーナーは抜けられない。
        assert!(matches!(
            change_member(&mut t, "alice", "alice", None),
            Err(MemberError::Invalid(_))
        ));
        change_member(&mut t, "alice", "bob", Some(TeamRole::Owner)).unwrap();
        change_member(&mut t, "alice", "alice", None).unwrap();
        assert_eq!(t.owners(), 1);
        // 外の人は何もできない。
        assert_eq!(
            change_member(&mut t, "mallory", "mallory", Some(TeamRole::Owner)),
            Err(MemberError::Forbidden)
        );
    }

    #[test]
    fn listing_and_delete() {
        let store = Store::memory();
        let t = create(&store, "alice", "営業").unwrap();
        let mut t2 = t.clone();
        change_member(&mut t2, "alice", "bob", Some(TeamRole::Member)).unwrap();
        save_member_change(&store, &t2, "bob").unwrap();
        assert_eq!(list_for(&store, "bob").unwrap()[0].members, 2);
        assert_eq!(
            role_in(&store, &t.id, "bob").unwrap(),
            Some(TeamRole::Member)
        );

        let b = boards::create(&store, "alice", "計画", Some(&t.id)).unwrap();
        // bob はチームのボードとして一覧で見える(既定は編集可)。
        let list = boards::list_for(&store, "bob", true).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, b.id);
        assert_eq!(list[0].role, board_core::api::Role::Editor);
        assert_eq!(list[0].team.as_deref(), Some(t.id.as_str()));

        delete(&store, &t2).unwrap();
        assert!(list_for(&store, "bob").unwrap().is_empty());
        assert!(boards::list_for(&store, "bob", true).unwrap().is_empty());
        assert_eq!(boards::get(&store, &b.id).unwrap().unwrap().team, None);
        // alice には個人ボードとして残る。
        assert_eq!(boards::list_for(&store, "alice", true).unwrap().len(), 1);
    }
}
