//! HTTP API(`/api/...`)でやり取りする JSON の型。クライアントとサーバーで共有する。
//!
//! | メソッド | パス | 内容 |
//! |----------|------|------|
//! | `POST` | `/api/signup` | アカウント作成([`Signup`] → [`Me`]、ログイン状態になる) |
//! | `POST` | `/api/login` | ログイン([`Login`] → [`Me`]) |
//! | `POST` | `/api/logout` | ログアウト |
//! | `GET` | `/api/me` | 自分([`Me`]、未ログインなら 401) |
//! | `GET` | `/api/boards` | 自分のボード一覧([`BoardInfo`] の配列) |
//! | `POST` | `/api/boards` | ボード作成([`NewBoard`] → [`BoardInfo`]) |
//! | `GET` | `/api/boards/{id}?key=` | ボードの情報と自分の権限([`BoardInfo`]) |
//! | `PATCH` | `/api/boards/{id}` | 名前の変更([`RenameBoard`]、編集者以上) |
//! | `DELETE` | `/api/boards/{id}` | 削除(オーナーのみ) |
//! | `GET` | `/api/boards/{id}/sharing` | 共有設定([`Sharing`]、オーナーのみ) |
//! | `PUT` | `/api/boards/{id}/members` | メンバーの追加・変更・削除([`SetMember`]、オーナーのみ) |
//! | `PUT` | `/api/boards/{id}/link` | 共有リンクの設定([`SetLink`] → [`Sharing`]、オーナーのみ) |
//! | `GET` | `/api/boards/{id}/comments?key=` | コメント一覧([`Comment`] の配列、閲覧者以上) |
//! | `POST` | `/api/boards/{id}/comments?key=` | コメント・返信([`NewComment`] → [`Posted`]、コメント可以上・要ログイン) |
//! | `PATCH` | `/api/boards/{id}/comments/{cid}?key=` | 本文の編集(本人)・解決/再開(コメント可以上)([`EditComment`]) |
//! | `DELETE` | `/api/boards/{id}/comments/{cid}?key=` | 削除(本人かオーナー。スレッドの先頭なら返信ごと) |
//! | `GET` | `/api/notifications` | 自分宛ての通知([`Notification`] の配列、新しい順) |
//! | `POST` | `/api/notifications/read` | 既読にする([`MarkRead`]) |
//!
//! 失敗したときは HTTP ステータスと [`ApiError`] を返す。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// ボードに対する権限。下ほど強い(`Ord` で比べられる)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// 見るだけ。
    Viewer,
    /// 見る + コメント。
    Commenter,
    /// 編集できる。
    Editor,
    /// 共有設定・削除もできる。
    Owner,
}

impl Role {
    pub fn can_comment(self) -> bool {
        self >= Role::Commenter
    }

    pub fn can_edit(self) -> bool {
        self >= Role::Editor
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "閲覧のみ",
            Role::Commenter => "コメント可",
            Role::Editor => "編集可",
            Role::Owner => "オーナー",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Commenter => "commenter",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "viewer" => Role::Viewer,
            "commenter" => Role::Commenter,
            "editor" => Role::Editor,
            "owner" => Role::Owner,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    /// ログイン名(英小文字・数字・`-`・`_`)。メンションにも使う。
    pub login: String,
    /// 表示名。
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signup {
    pub login: String,
    pub name: String,
    pub password: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Login {
    pub login: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardInfo {
    /// ボード ID(URL の `?room=`)。
    pub id: String,
    pub title: String,
    /// オーナーのログイン名。誰でも編集できる(オーナーのいない)ボードは `None`。
    pub owner: Option<String>,
    /// 自分の権限。
    pub role: Role,
    /// 作成・更新時刻(UNIX ミリ秒)。
    pub created: u64,
    pub updated: u64,
    /// 共有リンクで開いたときの鍵(開き直すときに URL に付ける)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewBoard {
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenameBoard {
    pub title: String,
}

/// 共有リンク。URL に `&key=…` を付けて開いた人に `role` を与える。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkShare {
    pub key: String,
    pub role: Role,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sharing {
    pub owner: Option<String>,
    /// ログイン名 → 権限。
    pub members: BTreeMap<String, Role>,
    pub link: Option<LinkShare>,
}

/// `role` が `None` ならメンバーから外す。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetMember {
    pub login: String,
    pub role: Option<Role>,
}

/// `role` が `None` なら共有リンクを無効にする。`regenerate` で鍵を作り直す(古いリンクは使えなくなる)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetLink {
    pub role: Option<Role>,
    #[serde(default)]
    pub regenerate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

/// コメントを要素に結びつける。要素が動くとコメントの位置も付いていく。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    /// 要素 ID(16 進)。
    pub element: String,
    /// 要素の左上からのずれ(ワールド座標)。
    pub dx: f32,
    pub dy: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    /// 返信なら、スレッドの先頭のコメント ID。
    #[serde(default)]
    pub thread: Option<u64>,
    pub author: String,
    pub author_name: String,
    pub body: String,
    /// 置いた位置(ワールド座標)。要素に結びついていれば、要素がないときだけ使う。
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub anchor: Option<Anchor>,
    pub created: u64,
    #[serde(default)]
    pub edited: Option<u64>,
    /// 解決済み(スレッドの先頭だけが持つ)。
    #[serde(default)]
    pub resolved: bool,
    /// 通知したメンション(ログイン名)。
    #[serde(default)]
    pub mentions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewComment {
    pub body: String,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default)]
    pub anchor: Option<Anchor>,
    /// 返信先(スレッドの先頭でも返信でもよい)。
    #[serde(default)]
    pub parent: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EditComment {
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub resolved: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Posted {
    pub comment: Comment,
    /// メンションしたが、ボードを開けないので通知しなかった人。
    pub unreachable: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationKind {
    /// `@ログイン名` でメンションされた。
    Mention,
    /// 参加しているスレッドに返信があった。
    Reply,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: u64,
    pub kind: NotificationKind,
    pub board: String,
    pub board_title: String,
    pub comment: u64,
    /// スレッドの先頭のコメント ID(開くときに使う)。
    pub thread: u64,
    pub from: String,
    pub from_name: String,
    /// 本文の冒頭。
    pub excerpt: String,
    pub created: u64,
    #[serde(default)]
    pub read: bool,
    /// 共有リンクで開いているボードなら、その鍵。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// `ids` が `None` ならすべて既読にする。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MarkRead {
    #[serde(default)]
    pub ids: Option<Vec<u64>>,
}

/// 本文から `@ログイン名` を拾う(重複なし、出てきた順)。
pub fn mentions_in(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in body.char_indices() {
        // メールアドレスの @ は拾わない。
        if c == '@' && !prev.is_some_and(|p| p.is_ascii_alphanumeric()) {
            let rest = &body[i + 1..];
            let len = rest
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
                .count();
            let login = rest[..len].to_ascii_lowercase();
            if valid_login(&login) && !out.contains(&login) {
                out.push(login);
            }
        }
        prev = Some(c);
    }
    out
}

/// ログイン名として使えるか(3〜32 文字の英小文字・数字・`-`・`_`)。
pub fn valid_login(s: &str) -> bool {
    (3..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_ordered() {
        assert!(Role::Viewer < Role::Commenter);
        assert!(Role::Editor < Role::Owner);
        assert!(!Role::Commenter.can_edit());
        assert!(Role::Commenter.can_comment());
        for r in [Role::Viewer, Role::Commenter, Role::Editor, Role::Owner] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
    }

    #[test]
    fn mentions() {
        assert_eq!(
            mentions_in("@Alice と @bob_2、@alice もう一度。a@example.com は違う @x"),
            vec!["alice".to_string(), "bob_2".to_string()]
        );
        assert!(mentions_in("メンションなし").is_empty());
        assert_eq!(mentions_in("(@carol)"), vec!["carol".to_string()]);
    }

    #[test]
    fn logins() {
        assert!(valid_login("tsuno_mon-1"));
        assert!(!valid_login("ab"));
        assert!(!valid_login("Abc"));
        assert!(!valid_login("a b c"));
        assert!(!valid_login(&"a".repeat(33)));
    }
}
