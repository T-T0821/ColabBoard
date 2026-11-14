//! redb への保存。
//!
//! | テーブル | キー | 値 |
//! |----------|------|----|
//! | `rooms` | ボード ID | ボードの状態(yrs v1 更新) |
//! | `users` | ログイン名 | [`UserRecord`](crate::auth::UserRecord)(JSON) |
//! | `sessions` | セッショントークン | [`SessionRecord`](crate::auth::SessionRecord)(JSON) |
//! | `boards` | ボード ID | [`BoardMeta`](crate::boards::BoardMeta)(JSON) |
//! | `user_boards` | (ログイン名, ボード ID) | [`UserBoard`](crate::boards::UserBoard)(JSON) |
//! | `comments` | (ボード ID, コメント ID) | [`Comment`](board_core::api::Comment)(JSON) |
//! | `notifications` | (ログイン名, 通知 ID) | [`Notification`](board_core::api::Notification)(JSON) |
//! | `versions` | (ボード ID, バージョン ID) | その時点のボードの状態(yrs v1 更新) |
//! | `version_info` | (ボード ID, バージョン ID) | [`VersionInfo`](board_core::api::VersionInfo)(JSON) |
//! | `media` | (ボード ID, 動画 ID) | 動画メモ(WebM / MP4) |
//! | `media_info` | (ボード ID, 動画 ID) | [`MediaInfo`](crate::media::MediaInfo)(JSON) |
//! | `api_tokens` | トークン ID | [`TokenRecord`](crate::tokens::TokenRecord)(JSON) |
//! | `user_tokens` | (ログイン名, トークン ID) | なし(一覧用の索引) |
//! | `plugins` | (ログイン名, プラグイン ID) | [`PluginRecord`](crate::plugins::PluginRecord)(JSON) |
//! | `plugin_wasm` | (ログイン名, プラグイン ID) | プラグインの WebAssembly モジュール |
//! | `teams` | チーム ID | [`TeamRecord`](crate::teams::TeamRecord)(JSON) |
//! | `user_teams` | (ログイン名, チーム ID) | 入った時刻(JSON) |
//! | `team_boards` | (チーム ID, ボード ID) | 加わった時刻(JSON) |
//! | `sso_identities` | `{issuer}\n{sub}` | 結びついたログイン名(JSON) |
//! | `templates` | テンプレート ID | [`TemplateRecord`](crate::templates::TemplateRecord)(JSON) |
//! | `template_data` | テンプレート ID | テンプレートの中身(クリップボード形式の文字列、JSON) |
//! | `user_templates` | (ログイン名, テンプレート ID) | なし(自分のテンプレートの索引) |
//! | `team_templates` | (チーム ID, テンプレート ID) | なし(チームに公開されたテンプレートの索引) |
//! | `audit` | (`*` か `team:{id}`, 記録 ID) | [`AuditEvent`](board_core::api::AuditEvent)(JSON) |
//! | `due_comments` | (ボード ID, コメント ID) | 期限(UNIX ミリ秒、JSON)。期限の知らせを送る前のスレッドの索引 |
//! | `notify_settings` | ログイン名 | [`NotifyRecord`](crate::notify::NotifyRecord)(JSON) |
//! | `notify_verify` | 確認トークン | [`VerifyRecord`](crate::notify::VerifyRecord)(JSON) |
//! | `push_subs` | (ログイン名, 送り先 URL のハッシュ) | [`PushRecord`](crate::notify::PushRecord)(JSON) |
//! | `server_keys` | 名前(`vapid`) | サーバーが作った鍵(JSON) |

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use redb::backends::InMemoryBackend;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::Serialize;
use serde::de::DeserializeOwned;

const ROOMS: TableDefinition<&str, &[u8]> = TableDefinition::new("rooms");
pub(crate) const USERS: TableDefinition<&str, &[u8]> = TableDefinition::new("users");
pub(crate) const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");
pub(crate) const BOARDS: TableDefinition<&str, &[u8]> = TableDefinition::new("boards");
pub(crate) const USER_BOARDS: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("user_boards");
pub(crate) const COMMENTS: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("comments");
pub(crate) const NOTIFICATIONS: TableDefinition<(&str, u64), &[u8]> =
    TableDefinition::new("notifications");
pub(crate) const VERSIONS: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("versions");
pub(crate) const VERSION_INFO: TableDefinition<(&str, u64), &[u8]> =
    TableDefinition::new("version_info");
pub(crate) const MEDIA: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("media");
pub(crate) const MEDIA_INFO: TableDefinition<(&str, u64), &[u8]> =
    TableDefinition::new("media_info");
pub(crate) const API_TOKENS: TableDefinition<&str, &[u8]> = TableDefinition::new("api_tokens");
pub(crate) const USER_TOKENS: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("user_tokens");
pub(crate) const PLUGINS: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("plugins");
pub(crate) const PLUGIN_WASM: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("plugin_wasm");
/// (ログイン名, プラグイン ID) → 実行する人が与えた許可。
pub(crate) const PLUGIN_GRANTS: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("plugin_grants");
/// (ログイン名, プラグイン ID) → プラグインの保存領域。
pub(crate) const PLUGIN_DATA: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("plugin_data");
pub(crate) const TEAMS: TableDefinition<&str, &[u8]> = TableDefinition::new("teams");
pub(crate) const USER_TEAMS: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("user_teams");
pub(crate) const TEAM_BOARDS: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("team_boards");
pub(crate) const SSO_IDENTITIES: TableDefinition<&str, &[u8]> =
    TableDefinition::new("sso_identities");
pub(crate) const AUDIT: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("audit");
pub(crate) const TEMPLATES: TableDefinition<&str, &[u8]> = TableDefinition::new("templates");
pub(crate) const TEMPLATE_DATA: TableDefinition<&str, &[u8]> =
    TableDefinition::new("template_data");
pub(crate) const USER_TEMPLATES: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("user_templates");
pub(crate) const TEAM_TEMPLATES: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("team_templates");
pub(crate) const DUE_COMMENTS: TableDefinition<(&str, u64), &[u8]> =
    TableDefinition::new("due_comments");
pub(crate) const NOTIFY_SETTINGS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("notify_settings");
pub(crate) const NOTIFY_VERIFY: TableDefinition<&str, &[u8]> =
    TableDefinition::new("notify_verify");
pub(crate) const PUSH_SUBS: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("push_subs");
pub(crate) const SERVER_KEYS: TableDefinition<&str, &[u8]> = TableDefinition::new("server_keys");

/// 時刻順に並ぶ一意な ID(UNIX ミリ秒 × 1000 + 連番)。再起動しても時刻が進んでいれば重ならない。
pub fn next_id() -> u64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = crate::auth::now_ms() * 1000;
    let mut prev = LAST.load(Ordering::Relaxed);
    loop {
        let id = now.max(prev + 1);
        match LAST.compare_exchange_weak(prev, id, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return id,
            Err(p) => prev = p,
        }
    }
}

pub type StoreError = Box<dyn std::error::Error + Send + Sync>;

pub struct Store {
    db: Database,
    persistent: bool,
}

impl Store {
    /// ファイルに保存する。
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Database::create(path)?, true)
    }

    /// メモリ上だけに持つ(`DATA_DIR=` やテスト用)。プロセスが終われば消える。
    pub fn memory() -> Self {
        let db = Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .expect("メモリ上のデータベース");
        Self::init(db, false).expect("テーブルの作成")
    }

    fn init(db: Database, persistent: bool) -> Result<Self, StoreError> {
        // 読み出し時に「テーブルがない」を気にしなくて済むよう、先に作っておく。
        let txn = db.begin_write()?;
        txn.open_table(ROOMS)?;
        txn.open_table(USERS)?;
        txn.open_table(SESSIONS)?;
        txn.open_table(BOARDS)?;
        txn.open_table(USER_BOARDS)?;
        txn.open_table(COMMENTS)?;
        txn.open_table(NOTIFICATIONS)?;
        txn.open_table(VERSIONS)?;
        txn.open_table(VERSION_INFO)?;
        txn.open_table(MEDIA)?;
        txn.open_table(MEDIA_INFO)?;
        txn.open_table(API_TOKENS)?;
        txn.open_table(USER_TOKENS)?;
        txn.open_table(PLUGINS)?;
        txn.open_table(PLUGIN_WASM)?;
        txn.open_table(PLUGIN_GRANTS)?;
        txn.open_table(PLUGIN_DATA)?;
        txn.open_table(TEAMS)?;
        txn.open_table(USER_TEAMS)?;
        txn.open_table(TEAM_BOARDS)?;
        txn.open_table(AUDIT)?;
        txn.open_table(SSO_IDENTITIES)?;
        txn.open_table(TEMPLATES)?;
        txn.open_table(TEMPLATE_DATA)?;
        txn.open_table(USER_TEMPLATES)?;
        txn.open_table(TEAM_TEMPLATES)?;
        txn.open_table(DUE_COMMENTS)?;
        txn.open_table(NOTIFY_SETTINGS)?;
        txn.open_table(NOTIFY_VERIFY)?;
        txn.open_table(PUSH_SUBS)?;
        txn.open_table(SERVER_KEYS)?;
        txn.commit()?;
        Ok(Self { db, persistent })
    }

    /// redb そのもの(バックアップで全テーブルを読み書きする)。
    pub(crate) fn database(&self) -> &Database {
        &self.db
    }

    /// ディスクに保存しているか。
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    // ---- ボードの中身 ----

    /// ボードの中身を読む。
    pub fn load(&self, room: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.load_local(room)
    }

    pub fn save(&self, room: &str, state: &[u8]) -> Result<(), StoreError> {
        self.save_local(room, state)
    }

    pub fn delete_room(&self, room: &str) -> Result<(), StoreError> {
        self.delete_local_room(room)
    }

    // ---- バージョンの中身 ----

    pub fn version_put(&self, room: &str, id: u64, state: &[u8]) -> Result<(), StoreError> {
        self.put_seq_raw(VERSIONS, room, id, state)
    }

    pub fn version_get(&self, room: &str, id: u64) -> Result<Option<Vec<u8>>, StoreError> {
        self.get_seq_raw(VERSIONS, room, id)
    }

    pub fn version_remove(&self, room: &str, ids: &[u64]) -> Result<(), StoreError> {
        self.remove_seq(VERSIONS, room, ids)
    }

    /// 1 つ目のキーが `a` のものの ID(ID 順)。
    pub(crate) fn seq_ids(&self, table: SeqTable, a: &str) -> Result<Vec<u64>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(table)?;
        let mut out = Vec::new();
        for entry in t.range((a, 0)..=(a, u64::MAX))? {
            let (k, _) = entry?;
            out.push(k.value().1);
        }
        Ok(out)
    }

    // ---- このサーバー(redb)に置く中身 ----

    fn load_local(&self, room: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(ROOMS)?;
        Ok(table.get(room)?.map(|v| v.value().to_vec()))
    }

    fn save_local(&self, room: &str, state: &[u8]) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(ROOMS)?;
            table.insert(room, state)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn delete_local_room(&self, room: &str) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(ROOMS)?;
            table.remove(room)?;
        }
        txn.commit()?;
        Ok(())
    }

    // ---- JSON で持つレコード ----

    pub(crate) fn get_json<T: DeserializeOwned>(
        &self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(table)?;
        match t.get(key)? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    }

    pub(crate) fn put_json<T: Serialize>(
        &self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec(value)?;
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(table)?;
            t.insert(key, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// テーブルの値を全部読む。
    pub(crate) fn scan_all_json<T: DeserializeOwned>(
        &self,
        table: TableDefinition<&str, &[u8]>,
    ) -> Result<Vec<T>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(table)?;
        let mut out = Vec::new();
        for entry in t.iter()? {
            let (_, v) = entry?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    /// まだなければ書く。書いたら true(同じキーの同時作成を防ぐ)。
    pub(crate) fn insert_new_json<T: Serialize>(
        &self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
        value: &T,
    ) -> Result<bool, StoreError> {
        let bytes = serde_json::to_vec(value)?;
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(table)?;
            if t.get(key)?.is_some() {
                return Ok(false);
            }
            t.insert(key, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(true)
    }

    /// 表の件数。
    pub(crate) fn count(&self, table: TableDefinition<&str, &[u8]>) -> Result<u64, StoreError> {
        let txn = self.db.begin_read()?;
        match txn.open_table(table) {
            Ok(t) => Ok(t.len()?),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn remove(
        &self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
    ) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(table)?;
            t.remove(key)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// 2 つの文字列をキーにするテーブルへ書く。
    pub(crate) fn put_pair<T: Serialize>(
        &self,
        table: TableDefinition<(&str, &str), &[u8]>,
        a: &str,
        b: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec(value)?;
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(table)?;
            t.insert((a, b), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub(crate) fn remove_pair(
        &self,
        table: TableDefinition<(&str, &str), &[u8]>,
        a: &str,
        b: &str,
    ) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(table)?;
            t.remove((a, b))?;
        }
        txn.commit()?;
        Ok(())
    }

    /// 2 つの文字列をキーにするテーブルへ、バイト列をそのまま書く。
    pub(crate) fn put_pair_bytes(
        &self,
        table: TableDefinition<(&str, &str), &[u8]>,
        a: &str,
        b: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(table)?;
            t.insert((a, b), bytes)?;
        }
        txn.commit()?;
        Ok(())
    }

    pub(crate) fn get_pair_bytes(
        &self,
        table: TableDefinition<(&str, &str), &[u8]>,
        a: &str,
        b: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(table)?;
        Ok(t.get((a, b))?.map(|v| v.value().to_vec()))
    }

    /// 1 つ目のキーが `a` のものを全部読む(2 つ目のキー順)。
    pub(crate) fn scan_pair<T: DeserializeOwned>(
        &self,
        table: TableDefinition<(&str, &str), &[u8]>,
        a: &str,
    ) -> Result<Vec<(String, T)>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(table)?;
        let mut out = Vec::new();
        for entry in t.range((a, "")..)? {
            let (k, v) = entry?;
            let (ka, kb) = k.value();
            if ka != a {
                break;
            }
            out.push((kb.to_string(), serde_json::from_slice(v.value())?));
        }
        Ok(out)
    }
}

/// (文字列, 連番)をキーにするテーブル(コメント・通知・バージョン)。
pub(crate) type SeqTable = TableDefinition<'static, (&'static str, u64), &'static [u8]>;

impl Store {
    pub(crate) fn put_seq<T: Serialize>(
        &self,
        table: SeqTable,
        a: &str,
        id: u64,
        value: &T,
    ) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec(value)?;
        self.put_seq_raw(table, a, id, &bytes)
    }

    pub(crate) fn put_seq_raw(
        &self,
        table: SeqTable,
        a: &str,
        id: u64,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(table)?;
            t.insert((a, id), bytes)?;
        }
        txn.commit()?;
        Ok(())
    }

    pub(crate) fn get_seq<T: DeserializeOwned>(
        &self,
        table: SeqTable,
        a: &str,
        id: u64,
    ) -> Result<Option<T>, StoreError> {
        match self.get_seq_raw(table, a, id)? {
            Some(v) => Ok(Some(serde_json::from_slice(&v)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn get_seq_raw(
        &self,
        table: SeqTable,
        a: &str,
        id: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(table)?;
        Ok(t.get((a, id))?.map(|v| v.value().to_vec()))
    }

    /// 1 つ目のキーが `a` のものを ID 順に全部読む。
    pub(crate) fn scan_seq<T: DeserializeOwned>(
        &self,
        table: SeqTable,
        a: &str,
    ) -> Result<Vec<(u64, T)>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(table)?;
        let mut out = Vec::new();
        for entry in t.range((a, 0)..=(a, u64::MAX))? {
            let (k, v) = entry?;
            out.push((k.value().1, serde_json::from_slice(v.value())?));
        }
        Ok(out)
    }

    /// テーブルの中身を(1 つ目のキーにかかわらず)全部読む。
    pub(crate) fn scan_seq_all<T: DeserializeOwned>(
        &self,
        table: SeqTable,
    ) -> Result<Vec<(String, u64, T)>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(table)?;
        let mut out = Vec::new();
        for entry in t.iter()? {
            let (k, v) = entry?;
            let (a, id) = k.value();
            out.push((a.to_string(), id, serde_json::from_slice(v.value())?));
        }
        Ok(out)
    }

    /// 指定の ID を消す。
    pub(crate) fn remove_seq(
        &self,
        table: SeqTable,
        a: &str,
        ids: &[u64],
    ) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(table)?;
            for id in ids {
                t.remove((a, *id))?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// 1 つ目のキーが `a` で ID が `upper` 以下のものを、新しい順に `limit` 件まで読む(`keep` で絞る)。
    pub(crate) fn scan_seq_rev<T: DeserializeOwned>(
        &self,
        table: SeqTable,
        a: &str,
        upper: u64,
        limit: usize,
        keep: impl Fn(&T) -> bool,
    ) -> Result<Vec<T>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(table)?;
        let mut out = Vec::new();
        for entry in t.range((a, 0)..=(a, upper))?.rev() {
            if out.len() >= limit {
                break;
            }
            let (_, v) = entry?;
            let item: T = serde_json::from_slice(v.value())?;
            if keep(&item) {
                out.push(item);
            }
        }
        Ok(out)
    }

    /// ID が `min_id` より小さいものを(1 つ目のキーにかかわらず)全部消す。消した件数を返す。
    pub(crate) fn retain_seq_newer(&self, table: SeqTable, min_id: u64) -> Result<u64, StoreError> {
        let txn = self.db.begin_write()?;
        let mut removed = 0;
        {
            let mut t = txn.open_table(table)?;
            t.retain(|k, _| {
                let keep = k.1 >= min_id;
                if !keep {
                    removed += 1;
                }
                keep
            })?;
        }
        txn.commit()?;
        Ok(removed)
    }

    /// 1 つ目のキーが `a` のものを全部消す。
    pub(crate) fn clear_seq(&self, table: SeqTable, a: &str) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(table)?;
            t.retain_in((a, 0)..=(a, u64::MAX), |_, _| false)?;
        }
        txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_tables() {
        let store = Store::memory();
        store.put_seq(COMMENTS, "b", 2, &"two").unwrap();
        store.put_seq(COMMENTS, "b", 1, &"one").unwrap();
        store.put_seq(COMMENTS, "c", 1, &"other").unwrap();
        let got: Vec<(u64, String)> = store.scan_seq(COMMENTS, "b").unwrap();
        assert_eq!(got, vec![(1, "one".into()), (2, "two".into())]);
        store.remove_seq(COMMENTS, "b", &[1]).unwrap();
        assert_eq!(store.scan_seq::<String>(COMMENTS, "b").unwrap().len(), 1);
        store.clear_seq(COMMENTS, "b").unwrap();
        assert!(store.scan_seq::<String>(COMMENTS, "b").unwrap().is_empty());
        assert_eq!(
            store
                .get_seq::<String>(COMMENTS, "c", 1)
                .unwrap()
                .as_deref(),
            Some("other")
        );
        assert!(next_id() < next_id());
    }

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/board.redb");
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.load("a").unwrap(), None);
            store.save("a", &[1, 2, 3]).unwrap();
            store.save("a", &[4, 5]).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.load("a").unwrap(), Some(vec![4, 5]));
        assert_eq!(store.load("b").unwrap(), None);
    }

    #[test]
    fn json_and_pairs() {
        let store = Store::memory();
        assert!(store.insert_new_json(USERS, "a", &1u32).unwrap());
        assert!(!store.insert_new_json(USERS, "a", &2u32).unwrap());
        assert_eq!(store.get_json::<u32>(USERS, "a").unwrap(), Some(1));
        store.put_pair(USER_BOARDS, "a", "x", &1u32).unwrap();
        store.put_pair(USER_BOARDS, "a", "y", &2u32).unwrap();
        store.put_pair(USER_BOARDS, "ab", "z", &3u32).unwrap();
        store.put_pair(USER_BOARDS, "", "w", &4u32).unwrap();
        let got: Vec<(String, u32)> = store.scan_pair(USER_BOARDS, "a").unwrap();
        assert_eq!(got, vec![("x".into(), 1), ("y".into(), 2)]);
        store.remove_pair(USER_BOARDS, "a", "x").unwrap();
        assert_eq!(store.scan_pair::<u32>(USER_BOARDS, "a").unwrap().len(), 1);
    }
}
