//! ルームの状態(yrs v1 更新)を redb に保存する。

use std::path::Path;

use redb::{Database, ReadableDatabase, TableDefinition};

const ROOMS: TableDefinition<&str, &[u8]> = TableDefinition::new("rooms");

pub struct Store {
    db: Database,
}

pub type StoreError = Box<dyn std::error::Error + Send + Sync>;

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Database::create(path)?;
        // 読み出し時に「テーブルがない」を気にしなくて済むよう、先に作っておく。
        let txn = db.begin_write()?;
        txn.open_table(ROOMS)?;
        txn.commit()?;
        Ok(Self { db })
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
}
