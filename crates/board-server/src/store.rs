//! redb への保存。
//!
//! | テーブル | キー | 値 |
//! |----------|------|----|
//! | `rooms` | ボード ID | ボードの状態(yrs v1 更新) |
//! | `users` | ログイン名 | [`UserRecord`](crate::auth::UserRecord)(JSON) |
//! | `sessions` | セッショントークン | [`SessionRecord`](crate::auth::SessionRecord)(JSON) |
//! | `boards` | ボード ID | [`BoardMeta`](crate::boards::BoardMeta)(JSON) |
//! | `user_boards` | (ログイン名, ボード ID) | [`UserBoard`](crate::boards::UserBoard)(JSON) |

use std::path::Path;

use redb::backends::InMemoryBackend;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::Serialize;
use serde::de::DeserializeOwned;

const ROOMS: TableDefinition<&str, &[u8]> = TableDefinition::new("rooms");
pub(crate) const USERS: TableDefinition<&str, &[u8]> = TableDefinition::new("users");
pub(crate) const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");
pub(crate) const BOARDS: TableDefinition<&str, &[u8]> = TableDefinition::new("boards");
pub(crate) const USER_BOARDS: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("user_boards");

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
        txn.commit()?;
        Ok(Self { db, persistent })
    }

    /// ディスクに保存しているか。
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    pub fn load(&self, room: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(ROOMS)?;
        Ok(table.get(room)?.map(|v| v.value().to_vec()))
    }

    pub fn save(&self, room: &str, state: &[u8]) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(ROOMS)?;
            table.insert(room, state)?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn delete_room(&self, room: &str) -> Result<(), StoreError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
