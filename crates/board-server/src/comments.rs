//! コメント・返信・メンションと通知。
//!
//! コメントはボードの中身(yrs ドキュメント)とは別に redb に持つ。権限(コメント可以上)を
//! サーバーで確かめるためと、通知を送るため。変わったらルームの参加者に
//! [`Message::Refresh`](board_core::protocol::Message::Refresh) を送り、取り直してもらう。

use std::collections::BTreeSet;

use board_core::api::{
    Comment, EditComment, NewComment, Notification, NotificationKind, Posted, Role, mentions_in,
};

use crate::auth::{UserRecord, get_user, now_ms};
use crate::boards::{self, BoardMeta, UserBoard, Visitor};
use crate::store::{COMMENTS, NOTIFICATIONS, Store, StoreError, USER_BOARDS, next_id};

pub const MAX_BODY_CHARS: usize = 4000;
const EXCERPT_CHARS: usize = 80;
/// 1 人あたりに残す通知の数(古いものから消す)。
const MAX_NOTIFICATIONS: usize = 200;

#[derive(Debug)]
pub enum CommentError {
    Invalid(&'static str),
    NotFound,
    Forbidden,
    Store(StoreError),
}

impl From<StoreError> for CommentError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

pub fn list(store: &Store, board: &str) -> Result<Vec<Comment>, StoreError> {
    Ok(store
        .scan_seq::<Comment>(COMMENTS, board)?
        .into_iter()
        .map(|(_, c)| c)
        .collect())
}

fn clean_body(body: &str) -> Result<String, CommentError> {
    let body: String = body
        .trim()
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .take(MAX_BODY_CHARS)
        .collect();
    if body.is_empty() {
        return Err(CommentError::Invalid("コメントが空です"));
    }
    Ok(body)
}

fn excerpt(body: &str) -> String {
    let mut s: String = body.chars().take(EXCERPT_CHARS).collect();
    if body.chars().count() > EXCERPT_CHARS {
        s.push('…');
    }
    s.replace('\n', " ")
}

/// そのユーザーがボードを開けるか(一覧に残っている共有リンクの鍵も使う)。
pub fn user_role(
    store: &Store,
    meta: Option<&BoardMeta>,
    board: &str,
    login: &str,
    anonymous_boards: bool,
) -> Result<(Option<Role>, Option<String>), StoreError> {
    let key = store
        .scan_pair::<UserBoard>(USER_BOARDS, login)?
        .into_iter()
        .find(|(id, _)| id == board)
        .and_then(|(_, ub)| ub.key);
    let role = boards::role_of(
        meta,
        Visitor {
            login: Some(login),
            key: key.as_deref(),
        },
        anonymous_boards,
    );
    Ok((role, key))
}

/// コメントを書く。メンションと返信の通知も送る。
pub fn post(
    store: &Store,
    meta: Option<&BoardMeta>,
    board: &str,
    author: &UserRecord,
    req: NewComment,
    anonymous_boards: bool,
) -> Result<Posted, CommentError> {
    let body = clean_body(&req.body)?;
    // 返信なら、スレッドの先頭を探す。
    let (thread, root) = match req.parent {
        Some(parent) => {
            let p: Comment = store
                .get_seq(COMMENTS, board, parent)?
                .ok_or(CommentError::NotFound)?;
            let root_id = p.thread.unwrap_or(p.id);
            let root: Comment = if root_id == p.id {
                p
            } else {
                store
                    .get_seq(COMMENTS, board, root_id)?
                    .ok_or(CommentError::NotFound)?
            };
            (Some(root_id), Some(root))
        }
        None => (None, None),
    };
    let (x, y, anchor) = match &root {
        Some(r) => (r.x, r.y, None),
        None => {
            if !(req.x.is_finite() && req.y.is_finite()) {
                return Err(CommentError::Invalid("位置が正しくありません"));
            }
            let anchor = req.anchor.filter(|a| {
                a.element.len() <= 16
                    && a.element.bytes().all(|b| b.is_ascii_hexdigit())
                    && a.dx.is_finite()
                    && a.dy.is_finite()
            });
            (req.x, req.y, anchor)
        }
    };

    // メンション: 存在して、ボードを開ける人だけに通知する。
    let mut mentioned = Vec::new();
    let mut unreachable = Vec::new();
    for login in mentions_in(&body) {
        if login == author.login || get_user(store, &login)?.is_none() {
            continue;
        }
        let (role, _) = user_role(store, meta, board, &login, anonymous_boards)?;
        if role.is_some() {
            mentioned.push(login);
        } else {
            unreachable.push(login);
        }
    }

    let comment = Comment {
        id: next_id(),
        thread,
        author: author.login.clone(),
        author_name: author.name.clone(),
        body,
        x,
        y,
        anchor,
        created: now_ms(),
        edited: None,
        resolved: false,
        mentions: mentioned.clone(),
    };
    store.put_seq(COMMENTS, board, comment.id, &comment)?;

    // 返信: スレッドの参加者(先頭を書いた人と返信した人)に知らせる。
    let mut repliers = BTreeSet::new();
    if let Some(root) = &root {
        repliers.insert(root.author.clone());
        for c in list(store, board)? {
            if c.thread == Some(root.id) {
                repliers.insert(c.author);
            }
        }
    }
    let title = meta.map_or_else(|| board.to_string(), |m| m.title.clone());
    let thread_id = thread.unwrap_or(comment.id);
    let targets = mentioned
        .iter()
        .map(|l| (l.clone(), NotificationKind::Mention))
        .chain(
            repliers
                .into_iter()
                .filter(|l| l != &author.login && !mentioned.contains(l))
                .map(|l| (l, NotificationKind::Reply)),
        );
    for (login, kind) in targets {
        let (role, key) = user_role(store, meta, board, &login, anonymous_boards)?;
        if role.is_none() {
            continue;
        }
        notify(
            store,
            &login,
            Notification {
                id: next_id(),
                kind,
                board: board.to_string(),
                board_title: title.clone(),
                comment: comment.id,
                thread: thread_id,
                from: author.login.clone(),
                from_name: author.name.clone(),
                excerpt: excerpt(&comment.body),
                created: comment.created,
                read: false,
                key,
            },
        )?;
    }
    Ok(Posted {
        comment,
        unreachable,
    })
}

fn notify(store: &Store, login: &str, n: Notification) -> Result<(), StoreError> {
    store.put_seq(NOTIFICATIONS, login, n.id, &n)?;
    let all = store.scan_seq::<Notification>(NOTIFICATIONS, login)?;
    if all.len() > MAX_NOTIFICATIONS {
        let old: Vec<u64> = all[..all.len() - MAX_NOTIFICATIONS]
            .iter()
            .map(|(id, _)| *id)
            .collect();
        store.remove_seq(NOTIFICATIONS, login, &old)?;
    }
    Ok(())
}

/// 本文の編集(本人のみ)と、解決・再開(コメント可以上)。
pub fn edit(
    store: &Store,
    board: &str,
    id: u64,
    user: &UserRecord,
    role: Role,
    req: EditComment,
) -> Result<Comment, CommentError> {
    let mut c: Comment = store
        .get_seq(COMMENTS, board, id)?
        .ok_or(CommentError::NotFound)?;
    if let Some(body) = req.body {
        if c.author != user.login {
            return Err(CommentError::Forbidden);
        }
        c.body = clean_body(&body)?;
        c.edited = Some(now_ms());
    }
    if let Some(resolved) = req.resolved {
        if !role.can_comment() {
            return Err(CommentError::Forbidden);
        }
        if c.thread.is_some() {
            return Err(CommentError::Invalid(
                "解決にできるのはスレッドの先頭だけです",
            ));
        }
        c.resolved = resolved;
    }
    store.put_seq(COMMENTS, board, id, &c)?;
    Ok(c)
}

/// 削除(本人かオーナー)。スレッドの先頭なら返信ごと消す。
pub fn delete(
    store: &Store,
    board: &str,
    id: u64,
    user: &UserRecord,
    role: Role,
) -> Result<(), CommentError> {
    let c: Comment = store
        .get_seq(COMMENTS, board, id)?
        .ok_or(CommentError::NotFound)?;
    if c.author != user.login && role != Role::Owner {
        return Err(CommentError::Forbidden);
    }
    let mut ids = vec![id];
    if c.thread.is_none() {
        ids.extend(
            list(store, board)?
                .into_iter()
                .filter(|r| r.thread == Some(id))
                .map(|r| r.id),
        );
    }
    store.remove_seq(COMMENTS, board, &ids)?;
    Ok(())
}

pub fn delete_all(store: &Store, board: &str) -> Result<(), StoreError> {
    store.clear_seq(COMMENTS, board)
}

pub fn notifications(store: &Store, login: &str) -> Result<Vec<Notification>, StoreError> {
    let mut all: Vec<Notification> = store
        .scan_seq::<Notification>(NOTIFICATIONS, login)?
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    all.reverse();
    Ok(all)
}

pub fn mark_read(store: &Store, login: &str, ids: Option<&[u64]>) -> Result<(), StoreError> {
    for (id, mut n) in store.scan_seq::<Notification>(NOTIFICATIONS, login)? {
        if !n.read && ids.is_none_or(|ids| ids.contains(&id)) {
            n.read = true;
            store.put_seq(NOTIFICATIONS, login, id, &n)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth;

    fn user(store: &Store, login: &str) -> UserRecord {
        auth::signup(store, login, &login.to_uppercase(), "password123").unwrap()
    }

    fn new(body: &str, parent: Option<u64>) -> NewComment {
        NewComment {
            body: body.into(),
            x: 1.0,
            y: 2.0,
            anchor: None,
            parent,
        }
    }

    #[test]
    fn threads_mentions_and_notifications() {
        let store = Store::memory();
        let alice = user(&store, "alice");
        let bob = user(&store, "bob");
        let carol = user(&store, "carol");
        let meta = boards::create(&store, "alice", "企画").unwrap();
        let id = meta.id.clone();
        // bob はメンバー、carol は開けない。
        let mut m = boards::get(&store, &id).unwrap().unwrap();
        m.members.insert("bob".into(), Role::Commenter);
        boards::put(&store, &m).unwrap();

        let posted = post(
            &store,
            Some(&m),
            &id,
            &alice,
            new("@bob と @carol 見て", None),
            true,
        )
        .unwrap();
        assert_eq!(posted.comment.mentions, vec!["bob".to_string()]);
        assert_eq!(posted.unreachable, vec!["carol".to_string()]);
        let root = posted.comment.id;
        let n = notifications(&store, "bob").unwrap();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].kind, NotificationKind::Mention);
        assert_eq!(n[0].thread, root);
        assert!(notifications(&store, "carol").unwrap().is_empty());

        // bob の返信 → alice に「返信」の通知。返信への返信でも同じスレッド。
        let r1 = post(&store, Some(&m), &id, &bob, new("了解", Some(root)), true).unwrap();
        assert_eq!(r1.comment.thread, Some(root));
        assert_eq!(
            notifications(&store, "alice").unwrap()[0].kind,
            NotificationKind::Reply
        );
        let r2 = post(
            &store,
            Some(&m),
            &id,
            &alice,
            new("ありがとう", Some(r1.comment.id)),
            true,
        )
        .unwrap();
        assert_eq!(r2.comment.thread, Some(root));
        assert_eq!(notifications(&store, "bob").unwrap().len(), 2);

        mark_read(&store, "bob", None).unwrap();
        assert!(notifications(&store, "bob").unwrap().iter().all(|n| n.read));

        // 編集は本人だけ、解決はコメント可以上、削除は本人かオーナー。
        assert!(matches!(
            edit(
                &store,
                &id,
                root,
                &bob,
                Role::Commenter,
                EditComment {
                    body: Some("x".into()),
                    resolved: None
                }
            ),
            Err(CommentError::Forbidden)
        ));
        let c = edit(
            &store,
            &id,
            root,
            &bob,
            Role::Commenter,
            EditComment {
                body: None,
                resolved: Some(true),
            },
        )
        .unwrap();
        assert!(c.resolved);
        assert!(matches!(
            delete(&store, &id, root, &carol, Role::Viewer),
            Err(CommentError::Forbidden)
        ));
        delete(&store, &id, root, &alice, Role::Owner).unwrap();
        assert!(list(&store, &id).unwrap().is_empty(), "返信ごと消える");
    }

    #[test]
    fn rejects_empty_and_bad_positions() {
        let store = Store::memory();
        let alice = user(&store, "alice");
        assert!(matches!(
            post(&store, None, "free", &alice, new("   ", None), true),
            Err(CommentError::Invalid(_))
        ));
        let mut bad = new("x", None);
        bad.x = f32::NAN;
        assert!(matches!(
            post(&store, None, "free", &alice, bad, true),
            Err(CommentError::Invalid(_))
        ));
    }
}
