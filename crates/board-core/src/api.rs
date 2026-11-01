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
//! | `PUT` | `/api/boards/{id}/embed` | 埋め込み(iframe)の設定([`SetLink`] → [`Sharing`]、オーナーのみ) |
//! | `PUT` | `/api/boards/{id}/public` | Web への公開([`SetPublic`] → [`Sharing`]、オーナーのみ) |
//! | `GET` | `/api/boards/{id}/comments?key=` | コメント一覧([`Comment`] の配列、閲覧者以上) |
//! | `POST` | `/api/boards/{id}/comments?key=` | コメント・返信([`NewComment`] → [`Posted`]、コメント可以上・要ログイン) |
//! | `PATCH` | `/api/boards/{id}/comments/{cid}?key=` | 本文の編集(本人)・解決/再開(コメント可以上)([`EditComment`]) |
//! | `DELETE` | `/api/boards/{id}/comments/{cid}?key=` | 削除(本人かオーナー。スレッドの先頭なら返信ごと) |
//! | `GET` | `/api/notifications` | 自分宛ての通知([`Notification`] の配列、新しい順) |
//! | `POST` | `/api/notifications/read` | 既読にする([`MarkRead`]) |
//! | `GET` | `/api/notify/settings` | メール・プッシュ通知の設定([`NotifyStatus`]) |
//! | `PUT` | `/api/notify/settings` | メール・プッシュ通知の設定を変える([`SetNotify`] → [`NotifyStatus`]。メールアドレスを変えると確認のメールを送る) |
//! | `GET` | `/api/notify/verify?token=` | メールアドレスの確認(メールのリンク。終わるとボード一覧へ戻る) |
//! | `POST` | `/api/notify/push` | このブラウザでプッシュ通知を受け取る([`PushSubscribe`]) |
//! | `DELETE` | `/api/notify/push` | このブラウザのプッシュ通知をやめる([`PushUnsubscribe`]) |
//! | `POST` | `/api/notify/test` | 自分にテストの通知を送る(メールとプッシュ) |
//! | `GET` | `/api/boards/{id}/versions?key=` | バージョン履歴([`VersionInfo`] の配列、新しい順、閲覧者以上) |
//! | `POST` | `/api/boards/{id}/versions?key=` | 今の状態に名前を付けて保存([`NewVersion`] → [`VersionInfo`]、編集者以上) |
//! | `GET` | `/api/boards/{id}/versions/{vid}?key=` | そのバージョンの状態(yrs v1 更新のバイナリ) |
//! | `PATCH` | `/api/boards/{id}/versions/{vid}?key=` | 名前の変更([`NewVersion`]、編集者以上) |
//! | `POST` | `/api/boards/{id}/versions/{vid}/restore?key=` | そのバージョンに戻す(編集者以上。戻す前の状態も保存する) |
//! | `GET` | `/api/boards/{id}/slack` | Slack 通知の設定([`SlackSettings`]、オーナーのみ) |
//! | `PUT` | `/api/boards/{id}/slack` | Slack 通知の設定を変える([`SetSlack`] → [`SlackSettings`]、オーナーのみ) |
//! | `POST` | `/api/boards/{id}/slack/test` | Slack にテストのメッセージを送る(オーナーのみ) |
//! | `POST` | `/api/boards/{id}/issues?key=` | GitHub・Jira の課題の状態を引く([`IssueQuery`] → [`IssueLookup`] の配列、編集者以上) |
//! | `PUT` | `/api/boards/{id}/team` | チームへ移す・チームから外す([`SetBoardTeam`] → [`BoardInfo`]、オーナーのみ) |
//! | `GET` | `/api/teams` | 自分の入っているチーム([`TeamInfo`] の配列) |
//! | `POST` | `/api/teams` | チームを作る([`NewTeam`] → [`Team`]、作った人がオーナー) |
//! | `GET` | `/api/teams/{tid}` | チームの詳細とメンバー([`Team`]、メンバーのみ) |
//! | `PATCH` | `/api/teams/{tid}` | 名前の変更([`NewTeam`] → [`Team`]、管理者以上) |
//! | `DELETE` | `/api/teams/{tid}` | チームを消す(オーナーのみ。ボードは作った人の個人ボードに戻る) |
//! | `PUT` | `/api/teams/{tid}/members` | メンバーの追加・変更・削除([`SetTeamMember`] → [`Team`]) |
//! | `PUT` | `/api/teams/{tid}/sso` | SSO 自動参加ドメインの設定([`TeamSso`] → [`Team`]、サーバー管理者のみ) |
//! | `GET` | `/api/storage` | 選べるデータの保存場所([`StorageLocation`] の配列、要ログイン) |
//! | `PUT` | `/api/teams/{tid}/storage` | チームのボードの保存場所を変え、中身を移す([`SetTeamStorage`] → [`Team`]、チームのオーナーのみ) |
//! | `GET` | `/api/sso` | ログイン方法([`SsoStatus`]、ログイン不要) |
//! | `GET` | `/api/sso/oidc/start?next=` | OpenID Connect でログインを始める(IdP へリダイレクト) |
//! | `GET` | `/api/audit?team=&before=&limit=&action=` | 操作ログ([`AuditEvent`] の配列、新しい順。`team` 指定はチームの管理者、なしはサーバー管理者) |
//! | `GET` | `/api/audit/export?team=&format=csv\|jsonl` | 操作ログの書き出し(CSV か JSON Lines) |
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
    /// サーバー管理者(`ADMIN_LOGINS`)か。全体の操作ログを見られる。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub admin: bool,
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
    /// 属しているチームの ID。個人のボードなら `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NewBoard {
    pub title: String,
    /// チームのボードとして作るなら、そのチームの ID(メンバーである必要がある)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
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
    /// 他のサイトに埋め込む(iframe)ときの鍵と権限。共有リンクとは別に作り直せる。
    #[serde(default)]
    pub embed: Option<LinkShare>,
    /// Web に公開中なら、その公開ページ。
    #[serde(default)]
    pub public: Option<PublicShare>,
}

/// Web への公開。`/p/{board}/{slug}` を知っている人は誰でも(ログインなしで)見られる。
/// 見るだけで、編集やコメントはできない。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicShare {
    pub slug: String,
    /// 公開した時刻(ミリ秒)。
    pub since: u64,
}

impl PublicShare {
    /// 公開ページのパス(`/p/{board}/{slug}`)。
    pub fn path(&self, board: &str) -> String {
        format!("/p/{board}/{}", self.slug)
    }
}

/// `enabled` が false なら公開をやめる。`regenerate` で URL を作り直す(古い URL は使えなくなる)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetPublic {
    pub enabled: bool,
    #[serde(default)]
    pub regenerate: bool,
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

/// チーム(ワークスペース)での役割。下ほど強い。
///
/// - メンバー: チームのボードをチームの既定の権限で開ける。チームのボードを作れる。
/// - 管理者: チームのボードすべてにオーナー権限を持ち、メンバーを追加・削除できる。
/// - オーナー: 管理者の権限に加えて、管理者の任命・チームの削除ができる。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamRole {
    Member,
    Admin,
    Owner,
}

impl TeamRole {
    pub fn is_admin(self) -> bool {
        self >= TeamRole::Admin
    }

    pub fn label(self) -> &'static str {
        match self {
            TeamRole::Member => "メンバー",
            TeamRole::Admin => "管理者",
            TeamRole::Owner => "オーナー",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TeamRole::Member => "member",
            TeamRole::Admin => "admin",
            TeamRole::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "member" => TeamRole::Member,
            "admin" => TeamRole::Admin,
            "owner" => TeamRole::Owner,
            _ => return None,
        })
    }
}

/// 一覧に出すチームの概要。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TeamInfo {
    pub id: String,
    pub name: String,
    /// 自分の役割。
    pub role: TeamRole,
    pub members: usize,
    pub created: u64,
}

/// チームの詳細。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub role: TeamRole,
    /// ログイン名 → 役割。
    pub members: BTreeMap<String, TeamRole>,
    pub created: u64,
    /// SSO でログインした人のメールがこのドメインなら、自動でメンバーにする。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sso_domains: Vec<String>,
    /// チームのボードの中身を置く場所の ID([`StorageLocation`])。`None` はこのサーバー。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
}

/// サーバーの管理者が用意したデータの保存場所。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageLocation {
    /// `local` はこのサーバー。
    pub id: String,
    pub label: String,
    /// 種類と所在(`S3 互換(ap-northeast-1)` など)。
    pub description: String,
}

/// チームの保存場所を変える。`location` が `None` か `local` ならこのサーバーに戻す。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SetTeamStorage {
    #[serde(default)]
    pub location: Option<String>,
}

/// SSO 自動参加ドメインの設定。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TeamSso {
    pub domains: Vec<String>,
}

/// サーバーが受け付けるログイン方法。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoStatus {
    /// パスワードでのログイン・アカウント作成ができるか。
    pub password_login: bool,
    /// OpenID Connect が使えるなら、ボタンに出す名前。
    #[serde(default)]
    pub oidc: Option<String>,
}

impl Team {
    pub fn info(&self) -> TeamInfo {
        TeamInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            role: self.role,
            members: self.members.len(),
            created: self.created,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewTeam {
    pub name: String,
}

/// `role` が `None` ならチームから外す(自分を外すと退出)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetTeamMember {
    pub login: String,
    pub role: Option<TeamRole>,
}

/// `team` が `None` ならチームから外して個人のボードに戻す。
/// `access` はチームのメンバーに与える権限(省略時は編集可)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetBoardTeam {
    pub team: Option<String>,
    #[serde(default)]
    pub access: Option<Role>,
}

/// 操作ログの 1 件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: u64,
    /// 時刻(UNIX ミリ秒)。
    pub at: u64,
    /// 操作の種類(`board.create`・`team.member` など)。
    pub action: String,
    /// 操作した人のログイン名(ログイン前の操作なら `None`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// 操作の対象(メンバーのログイン名など)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// 補足(変更後の権限・名前など)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 接続元の IP アドレス。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
}

impl AuditEvent {
    /// 画面に出す操作の名前。
    pub fn label(&self) -> &str {
        match self.action.as_str() {
            "auth.signup" => "アカウント作成",
            "auth.login" => "ログイン",
            "auth.login_failed" => "ログイン失敗",
            "auth.logout" => "ログアウト",
            "auth.sso" => "SSO でログイン",
            "board.create" => "ボード作成",
            "board.rename" => "ボード名の変更",
            "board.delete" => "ボード削除",
            "board.member" => "ボードのメンバー変更",
            "board.link" => "共有リンクの変更",
            "board.team" => "ボードのチーム変更",
            "board.restore" => "バージョンの復元",
            "team.create" => "チーム作成",
            "team.rename" => "チーム名の変更",
            "team.delete" => "チーム削除",
            "team.member" => "チームのメンバー変更",
            "team.settings" => "チームの設定変更",
            "team.storage" => "保存場所の変更",
            "token.create" => "API トークン作成",
            "token.revoke" => "API トークン失効",
            other => other,
        }
    }
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
    /// 期限(UNIX ミリ秒。スレッドの先頭だけが持つ)。近づくと・過ぎるとメンションした人と書いた人に知らせる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<u64>,
    /// 期限の知らせをどこまで送ったか(サーバーが使う。0: まだ、1: 近づいた、2: 過ぎた)。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub due_stage: u8,
}

fn is_zero(v: &u8) -> bool {
    *v == 0
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
    /// 期限(UNIX ミリ秒)。新しいスレッドのときだけ使う。
    #[serde(default)]
    pub due: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EditComment {
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub resolved: Option<bool>,
    /// 期限を変える(UNIX ミリ秒、`0` で外す。スレッドの先頭だけ、コメント可以上)。
    #[serde(default)]
    pub due: Option<u64>,
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
    /// 担当している(メンションされた・書いた)スレッドの期限が近づいた。
    Due,
    /// 担当しているスレッドの期限が過ぎた(まだ解決していない)。
    Overdue,
}

impl NotificationKind {
    /// メール・プッシュ通知の設定でどの種類に入るか。
    pub fn group(self) -> NotifyGroup {
        match self {
            Self::Mention => NotifyGroup::Mention,
            Self::Reply => NotifyGroup::Reply,
            Self::Due | Self::Overdue => NotifyGroup::Due,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyGroup {
    Mention,
    Reply,
    Due,
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
    /// 期限の通知なら、その期限(UNIX ミリ秒)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<u64>,
}

impl Notification {
    /// 一文での説明(「〜が〜しました」)。画面・メール・プッシュ通知で使う。
    pub fn headline(&self) -> String {
        match self.kind {
            NotificationKind::Mention => format!("{} があなたをメンションしました", self.from_name),
            NotificationKind::Reply => format!("{} がスレッドに返信しました", self.from_name),
            NotificationKind::Due => "コメントの期限が近づいています".to_string(),
            NotificationKind::Overdue => "コメントの期限を過ぎました".to_string(),
        }
    }
}

/// メール・プッシュ通知で知らせる種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyKinds {
    /// メンションされた。
    #[serde(default)]
    pub mention: bool,
    /// 参加しているスレッドに返信があった。
    #[serde(default)]
    pub reply: bool,
    /// 期限が近づいた・過ぎた。
    #[serde(default)]
    pub due: bool,
}

impl NotifyKinds {
    pub const ALL: Self = Self {
        mention: true,
        reply: true,
        due: true,
    };

    pub fn allows(self, kind: NotificationKind) -> bool {
        match kind.group() {
            NotifyGroup::Mention => self.mention,
            NotifyGroup::Reply => self.reply,
            NotifyGroup::Due => self.due,
        }
    }
}

/// `PUT /api/notify/settings` で送る、メール・プッシュ通知の設定。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetNotify {
    /// 送り先のメールアドレス(空で外す)。変えると確認のメールを送り、リンクを開くまで使わない。
    #[serde(default)]
    pub email: String,
    pub email_on: NotifyKinds,
    pub push_on: NotifyKinds,
}

/// `GET /api/notify/settings` の結果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotifyStatus {
    pub email: String,
    /// 確認のリンクを開いたか(開くまでメールは送らない)。
    pub email_verified: bool,
    pub email_on: NotifyKinds,
    pub push_on: NotifyKinds,
    /// サーバーでメールを送れるか(`SMTP_URL` が設定されているか)。
    pub email_available: bool,
    /// プッシュ通知の購読に使う公開鍵(VAPID、base64url)。`None` ならプッシュ通知は使えない。
    #[serde(default)]
    pub push_key: Option<String>,
    /// プッシュ通知を受け取る端末(ブラウザ)の数。
    pub push_devices: u32,
}

/// ブラウザの `PushSubscription.toJSON()` の形。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PushSubscribe {
    pub endpoint: String,
    #[serde(default)]
    pub keys: Option<PushKeys>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PushKeys {
    #[serde(default)]
    pub p256dh: String,
    #[serde(default)]
    pub auth: String,
}

/// `DELETE /api/notify/push` で送る。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PushUnsubscribe {
    pub endpoint: String,
}

/// `ids` が `None` ならすべて既読にする。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MarkRead {
    #[serde(default)]
    pub ids: Option<Vec<u64>>,
}

/// バージョン(ある時点のボードの状態)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub id: u64,
    /// 作った時刻(UNIX ミリ秒)。
    pub created: u64,
    /// 名前。自動保存なら `None`。
    #[serde(default)]
    pub label: Option<String>,
    /// 保存した人(自動保存なら `None`)。
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub author_name: Option<String>,
    /// 要素の数。
    pub elements: usize,
    /// 状態のバイト数。
    pub bytes: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NewVersion {
    #[serde(default)]
    pub label: Option<String>,
}

/// Slack に知らせる出来事。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackEvents {
    /// すべてのコメントと返信。
    #[serde(default)]
    pub comments: bool,
    /// メンションを含むコメント(`comments` がオフでも送る)。
    #[serde(default)]
    pub mentions: bool,
    /// 名前を付けたバージョンの保存と、バージョンの復元。
    #[serde(default)]
    pub versions: bool,
}

impl Default for SlackEvents {
    fn default() -> Self {
        Self {
            comments: true,
            mentions: true,
            versions: true,
        }
    }
}

/// ボードの Slack 通知の設定。Webhook URL は秘密なので末尾だけ返す。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SlackSettings {
    /// Webhook URL が設定されているか。
    pub enabled: bool,
    /// 設定済みの Webhook URL の見分け用(末尾の数文字)。
    #[serde(default)]
    pub hint: Option<String>,
    pub events: SlackEvents,
}

/// `webhook` が `None` なら URL はそのまま、空文字なら通知をやめる。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetSlack {
    #[serde(default)]
    pub webhook: Option<String>,
    #[serde(default)]
    pub events: SlackEvents,
}

/// 課題の置き場所。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueProvider {
    Github,
    Jira,
}

impl IssueProvider {
    pub fn label(self) -> &'static str {
        match self {
            IssueProvider::Github => "GitHub",
            IssueProvider::Jira => "Jira",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            IssueProvider::Github => "github",
            IssueProvider::Jira => "jira",
        }
    }
}

/// 課題の状態を大まかに分けたもの(カードの色に使う)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    /// 未着手・オープン。
    Open,
    /// 進行中(Jira の「進行中」カテゴリ、GitHub の下書き PR)。
    InProgress,
    /// 完了(クローズ・マージ・Jira の完了カテゴリ)。
    Done,
    /// 対応しないで閉じた。
    Closed,
}

/// 課題 1 件の情報。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IssueInfo {
    pub provider: IssueProvider,
    /// 表示用の番号(`owner/repo#12`、`PROJ-34`)。
    pub key: String,
    pub title: String,
    pub state: IssueState,
    /// サービス上の状態の名前(`Open`、`Merged`、`進行中` など)。
    pub status: String,
    /// 担当者(いなければ `None`)。
    #[serde(default)]
    pub assignee: Option<String>,
    /// プルリクエストか。
    #[serde(default)]
    pub pull: bool,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IssueQuery {
    pub urls: Vec<String>,
}

/// 1 件分の結果。取れなければ `error`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IssueLookup {
    pub url: String,
    #[serde(default)]
    pub issue: Option<IssueInfo>,
    #[serde(default)]
    pub error: Option<String>,
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
