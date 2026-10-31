//! ボードの情報(名前・オーナー・共有設定)と権限の判定。
//!
//! - ログインして作ったボードにはオーナーがいて、最初は本人しか開けない。
//!   メンバーとして招待するか、共有リンク(`?room=…&key=…`)を有効にすると他の人も開ける。
//! - 情報のないボード(ログインせずに `?room=` で作ったもの)は、従来どおり誰でも編集できる。
//!   サーバー設定 `ANONYMOUS_BOARDS=0` でこの種類のボードを禁止できる。
//! - チームのボードは、チームのメンバーならチームの既定の権限(`team_access`)で開ける。
//!   チームの管理者・オーナーはオーナー権限を持つ。

use std::collections::BTreeMap;

use board_core::api::{BoardInfo, LinkShare, Role, Sharing, TeamRole};
use serde::{Deserialize, Serialize};

use crate::auth::{now_ms, random_hex, random_id};
use crate::store::{BOARDS, Store, StoreError, TEAM_BOARDS, USER_BOARDS};
use crate::teams;

pub const MAX_TITLE_CHARS: usize = 80;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardMeta {
    pub id: String,
    pub title: String,
    pub owner: Option<String>,
    pub created: u64,
    pub updated: u64,
    #[serde(default)]
    pub members: BTreeMap<String, Role>,
    #[serde(default)]
    pub link: Option<LinkShare>,
    /// 埋め込み用の鍵(共有リンクとは別)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed: Option<LinkShare>,
    /// Slack 通知の送り先。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<crate::slack::SlackHook>,
    /// 削除済み。ID を使い回して誰でも開けるボードとして復活しないよう、印だけ残す。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    /// 属しているチーム。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// チームのメンバーに与える権限(省略時は編集可)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_access: Option<Role>,
}

impl BoardMeta {
    pub fn sharing(&self) -> Sharing {
        Sharing {
            owner: self.owner.clone(),
            members: self.members.clone(),
            link: self.link.clone(),
            embed: self.embed.clone(),
        }
    }

    pub fn info(&self, role: Role, key: Option<String>) -> BoardInfo {
        BoardInfo {
            id: self.id.clone(),
            title: self.title.clone(),
            owner: self.owner.clone(),
            role,
            created: self.created,
            updated: self.updated,
            key,
            team: self.team.clone(),
        }
    }

    /// チームのメンバーに与える権限。
    pub fn team_access(&self) -> Role {
        self.team_access.unwrap_or(Role::Editor).min(Role::Editor)
    }
}

/// ユーザーのボード一覧に載せる印(作った・招待された・開いた)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserBoard {
    /// 最後に開いた(または一覧に加わった)時刻。
    pub opened: u64,
    /// 共有リンクで開いたときの鍵。
    #[serde(default)]
    pub key: Option<String>,
}

/// 誰が、どの鍵で開こうとしているか。
#[derive(Clone, Copy, Debug, Default)]
pub struct Visitor<'a> {
    pub login: Option<&'a str>,
    pub key: Option<&'a str>,
}

/// 権限を決める。開けないなら `None`。
///
/// `team_role` は、ボードの属するチームでの訪問者の役割([`role_for`] が調べる)。
pub fn role_of(
    meta: Option<&BoardMeta>,
    who: Visitor<'_>,
    team_role: Option<TeamRole>,
    anonymous_boards: bool,
) -> Option<Role> {
    let Some(meta) = meta else {
        return anonymous_boards.then_some(Role::Editor);
    };
    if meta.deleted {
        return None;
    }
    let Some(owner) = &meta.owner else {
        return Some(Role::Editor);
    };
    let mut best: Option<Role> = None;
    if let Some(login) = who.login {
        if login == owner {
            return Some(Role::Owner);
        }
        best = meta.members.get(login).copied();
    }
    if meta.team.is_some() {
        let from_team = match team_role {
            Some(r) if r.is_admin() => Some(Role::Owner),
            Some(_) => Some(meta.team_access()),
            None => None,
        };
        best = best.max(from_team);
    }
    if let (Some(link), Some(key)) = (&meta.link, who.key)
        && constant_time_eq(link.key.as_bytes(), key.as_bytes())
    {
        best = best.max(Some(link.role));
    }
    if let (Some(embed), Some(key)) = (&meta.embed, who.key)
        && constant_time_eq(embed.key.as_bytes(), key.as_bytes())
    {
        best = best.max(Some(embed.role));
    }
    best
}

/// 保存先からチームの役割を引いて権限を決める。
pub fn role_for(
    store: &Store,
    meta: Option<&BoardMeta>,
    who: Visitor<'_>,
    anonymous_boards: bool,
) -> Result<Option<Role>, StoreError> {
    let team_role = match (meta.and_then(|m| m.team.as_deref()), who.login) {
        (Some(team), Some(login)) => teams::role_in(store, team, login)?,
        _ => None,
    };
    Ok(role_of(meta, who, team_role, anonymous_boards))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn clean_title(title: &str) -> String {
    let t: String = title
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE_CHARS)
        .collect();
    if t.is_empty() {
        "無題のボード".into()
    } else {
        t
    }
}

pub fn get(store: &Store, id: &str) -> Result<Option<BoardMeta>, StoreError> {
    store.get_json(BOARDS, id)
}

pub fn put(store: &Store, meta: &BoardMeta) -> Result<(), StoreError> {
    store.put_json(BOARDS, &meta.id, meta)
}

/// 新しいボードを作る。`team` を渡すとそのチームのボードにする(メンバーかは呼ぶ側で確かめる)。
pub fn create(
    store: &Store,
    owner: &str,
    title: &str,
    team: Option<&str>,
) -> Result<BoardMeta, StoreError> {
    let now = now_ms();
    loop {
        let meta = BoardMeta {
            id: random_id(12),
            title: clean_title(title),
            owner: Some(owner.to_string()),
            created: now,
            updated: now,
            members: BTreeMap::new(),
            link: None,
            embed: None,
            slack: None,
            deleted: false,
            team: team.map(str::to_string),
            team_access: None,
        };
        // ID がぶつかるのはまずないが、ぶつかったら作り直す。既存の(情報のない)ルームとも避ける。
        if store.load(&meta.id)?.is_some() {
            continue;
        }
        if store.insert_new_json(BOARDS, &meta.id, &meta)? {
            remember(store, owner, &meta.id, None)?;
            if let Some(team) = team {
                attach(store, team, &meta.id)?;
            }
            return Ok(meta);
        }
    }
}

/// ユーザーのボード一覧に載せる(すでにあれば開いた時刻を更新)。
pub fn remember(store: &Store, login: &str, id: &str, key: Option<&str>) -> Result<(), StoreError> {
    store.put_pair(
        USER_BOARDS,
        login,
        id,
        &UserBoard {
            opened: now_ms(),
            key: key.map(str::to_string),
        },
    )
}

pub fn forget(store: &Store, login: &str, id: &str) -> Result<(), StoreError> {
    store.remove_pair(USER_BOARDS, login, id)
}

/// チームのボード一覧に載せる。
pub fn attach(store: &Store, team: &str, id: &str) -> Result<(), StoreError> {
    store.put_pair(TEAM_BOARDS, team, id, &now_ms())
}

/// チームのボード一覧から外す。
pub fn detach(store: &Store, team: &str, id: &str) -> Result<(), StoreError> {
    store.remove_pair(TEAM_BOARDS, team, id)
}

/// チームに属するボードの ID。
pub fn team_board_ids(store: &Store, team: &str) -> Result<Vec<String>, StoreError> {
    Ok(store
        .scan_pair::<u64>(TEAM_BOARDS, team)?
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

/// ユーザーのボード一覧(最近開いた順)。開けなくなったものは除く。
/// 自分の入っているチームのボードも含める(開いたことがなければ更新時刻の順)。
pub fn list_for(
    store: &Store,
    login: &str,
    anonymous_boards: bool,
) -> Result<Vec<BoardInfo>, StoreError> {
    let mut out: Vec<(u64, BoardInfo)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (id, ub) in store.scan_pair::<UserBoard>(USER_BOARDS, login)? {
        let meta = get(store, &id)?;
        let who = Visitor {
            login: Some(login),
            key: ub.key.as_deref(),
        };
        let Some(role) = role_for(store, meta.as_ref(), who, anonymous_boards)? else {
            continue;
        };
        let info = match meta {
            Some(m) => {
                // 鍵が不要になった(メンバーになった)なら URL に付けない。
                let need_key = role_for(
                    store,
                    Some(&m),
                    Visitor {
                        login: Some(login),
                        key: None,
                    },
                    anonymous_boards,
                )?
                .is_none_or(|r| r < role);
                m.info(role, if need_key { ub.key.clone() } else { None })
            }
            // 情報のないボードは名前を ID にしておく。
            None => {
                if store.load(&id)?.is_none() {
                    continue;
                }
                BoardInfo {
                    id: id.clone(),
                    title: id.clone(),
                    owner: None,
                    role,
                    created: ub.opened,
                    updated: ub.opened,
                    key: None,
                    team: None,
                }
            }
        };
        seen.insert(id);
        out.push((ub.opened, info));
    }
    for team in teams::team_ids_of(store, login)? {
        for id in team_board_ids(store, &team)? {
            if seen.contains(&id) {
                continue;
            }
            let Some(m) = get(store, &id)? else { continue };
            let who = Visitor {
                login: Some(login),
                key: None,
            };
            let Some(role) = role_for(store, Some(&m), who, anonymous_boards)? else {
                continue;
            };
            seen.insert(id);
            out.push((m.updated, m.info(role, None)));
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.0));
    Ok(out.into_iter().map(|(_, i)| i).collect())
}

/// 共有リンクを設定する。
pub fn set_link(meta: &mut BoardMeta, role: Option<Role>, regenerate: bool) {
    meta.link = next_share(meta.link.take(), role, regenerate);
}

/// 埋め込み用の鍵を設定する。
pub fn set_embed(meta: &mut BoardMeta, role: Option<Role>, regenerate: bool) {
    meta.embed = next_share(meta.embed.take(), role, regenerate);
}

fn next_share(old: Option<LinkShare>, role: Option<Role>, regenerate: bool) -> Option<LinkShare> {
    match (role, old) {
        (None, _) => None,
        // オーナー権限はリンクでは渡さない。
        (Some(r), old) => Some(LinkShare {
            key: match old {
                Some(l) if !regenerate => l.key,
                _ => random_hex(16),
            },
            role: r.min(Role::Editor),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> BoardMeta {
        BoardMeta {
            id: "b".into(),
            title: "t".into(),
            owner: Some("alice".into()),
            created: 0,
            updated: 0,
            members: BTreeMap::from([("bob".to_string(), Role::Viewer)]),
            link: Some(LinkShare {
                key: "secret".into(),
                role: Role::Commenter,
            }),
            embed: None,
            slack: None,
            deleted: false,
            team: None,
            team_access: None,
        }
    }

    fn who<'a>(login: Option<&'a str>, key: Option<&'a str>) -> Visitor<'a> {
        Visitor { login, key }
    }

    #[test]
    fn roles() {
        let m = meta();
        assert_eq!(
            role_of(Some(&m), who(Some("alice"), None), None, true),
            Some(Role::Owner)
        );
        assert_eq!(
            role_of(Some(&m), who(Some("bob"), None), None, true),
            Some(Role::Viewer)
        );
        // リンクの方が強ければそちら。
        assert_eq!(
            role_of(Some(&m), who(Some("bob"), Some("secret")), None, true),
            Some(Role::Commenter)
        );
        assert_eq!(
            role_of(Some(&m), who(None, Some("secret")), None, true),
            Some(Role::Commenter)
        );
        assert_eq!(
            role_of(Some(&m), who(None, Some("wrong")), None, true),
            None
        );
        assert_eq!(
            role_of(Some(&m), who(Some("carol"), None), None, true),
            None
        );
        // 情報のないボード。
        assert_eq!(
            role_of(None, who(None, None), None, true),
            Some(Role::Editor)
        );
        assert_eq!(role_of(None, who(None, None), None, false), None);
        let gone = BoardMeta { deleted: true, ..m };
        assert_eq!(
            role_of(Some(&gone), who(Some("alice"), None), None, true),
            None
        );
    }

    #[test]
    fn embed_key_is_separate() {
        let mut m = meta();
        set_embed(&mut m, Some(Role::Viewer), false);
        let ek = m.embed.clone().unwrap().key;
        assert_ne!(ek, "secret");
        assert_eq!(
            role_of(Some(&m), who(None, Some(&ek)), None, true),
            Some(Role::Viewer)
        );
        // 共有リンクを止めても埋め込みは使える。
        set_link(&mut m, None, false);
        assert_eq!(
            role_of(Some(&m), who(None, Some(&ek)), None, true),
            Some(Role::Viewer)
        );
        set_embed(&mut m, None, false);
        assert_eq!(role_of(Some(&m), who(None, Some(&ek)), None, true), None);
    }

    #[test]
    fn links() {
        let mut m = meta();
        m.link = None;
        set_link(&mut m, Some(Role::Owner), false);
        let first = m.link.clone().unwrap();
        assert_eq!(first.role, Role::Editor);
        set_link(&mut m, Some(Role::Viewer), false);
        assert_eq!(m.link.as_ref().unwrap().key, first.key);
        set_link(&mut m, Some(Role::Viewer), true);
        assert_ne!(m.link.as_ref().unwrap().key, first.key);
        set_link(&mut m, None, false);
        assert!(m.link.is_none());
    }

    #[test]
    fn listing() {
        let store = Store::memory();
        let a = create(&store, "alice", "  企画  ", None).unwrap();
        assert_eq!(a.title, "企画");
        let b = create(&store, "alice", "", None).unwrap();
        assert_eq!(b.title, "無題のボード");
        let list = list_for(&store, "alice", true).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|i| i.role == Role::Owner));

        // bob はリンクで開いた。リンクが消えたら一覧からも消える。
        let mut m = get(&store, &a.id).unwrap().unwrap();
        set_link(&mut m, Some(Role::Viewer), false);
        put(&store, &m).unwrap();
        let key = m.link.clone().unwrap().key;
        remember(&store, "bob", &a.id, Some(&key)).unwrap();
        let list = list_for(&store, "bob", true).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].key.as_deref(), Some(key.as_str()));
        set_link(&mut m, None, false);
        put(&store, &m).unwrap();
        assert!(list_for(&store, "bob", true).unwrap().is_empty());
    }
}
