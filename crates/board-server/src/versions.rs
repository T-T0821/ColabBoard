//! バージョン履歴: ある時点のボードの状態を丸ごと保存し、見たり戻したりできるようにする。
//!
//! - 自動: 変更があれば [`AUTO_INTERVAL_MS`] ごと、または全員が抜けたときに作る([`Hub`](crate::Hub))。
//!   古い自動保存は [`MAX_AUTO_VERSIONS`] 件を超えたら消す。
//! - 手動: 「今の状態を保存」で名前を付けて作る。名前付きは自動では消さない。
//! - 戻す: 昔の状態と同じになるよう、普通の編集として変更を加える(CRDT なので全員にそのまま配れる)。

use board_core::Board;
use board_core::api::VersionInfo;

use crate::auth::{UserRecord, now_ms};
use crate::store::{Store, StoreError, VERSION_INFO, VERSIONS, next_id};

/// 自動保存の最短間隔。
pub const AUTO_INTERVAL_MS: u64 = 10 * 60 * 1000;
/// 残す自動保存の数。
pub const MAX_AUTO_VERSIONS: usize = 100;
pub const MAX_LABEL_CHARS: usize = 80;

fn clean_label(label: Option<&str>) -> Option<String> {
    let l: String = label?
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect();
    (!l.is_empty()).then_some(l)
}

fn element_count(state: &[u8]) -> usize {
    let mut b = Board::new();
    match b.apply_remote(state) {
        Ok(()) => b.len(),
        Err(_) => 0,
    }
}

/// バージョンを作る。`label` がなければ自動保存として扱う。
pub fn create(
    store: &Store,
    board: &str,
    state: &[u8],
    label: Option<&str>,
    author: Option<&UserRecord>,
) -> Result<VersionInfo, StoreError> {
    let info = VersionInfo {
        id: next_id(),
        created: now_ms(),
        label: clean_label(label),
        author: author.map(|u| u.login.clone()),
        author_name: author.map(|u| u.name.clone()),
        elements: element_count(state),
        bytes: state.len(),
    };
    store.put_seq_raw(VERSIONS, board, info.id, state)?;
    store.put_seq(VERSION_INFO, board, info.id, &info)?;
    prune(store, board)?;
    Ok(info)
}

/// 古い自動保存を消す。
fn prune(store: &Store, board: &str) -> Result<(), StoreError> {
    let autos: Vec<u64> = store
        .scan_seq::<VersionInfo>(VERSION_INFO, board)?
        .into_iter()
        .filter(|(_, v)| v.label.is_none() && v.author.is_none())
        .map(|(id, _)| id)
        .collect();
    if autos.len() > MAX_AUTO_VERSIONS {
        let old = &autos[..autos.len() - MAX_AUTO_VERSIONS];
        store.remove_seq(VERSIONS, board, old)?;
        store.remove_seq(VERSION_INFO, board, old)?;
    }
    Ok(())
}

/// 新しい順。
pub fn list(store: &Store, board: &str) -> Result<Vec<VersionInfo>, StoreError> {
    let mut v: Vec<VersionInfo> = store
        .scan_seq::<VersionInfo>(VERSION_INFO, board)?
        .into_iter()
        .map(|(_, v)| v)
        .collect();
    v.reverse();
    Ok(v)
}

pub fn latest_time(store: &Store, board: &str) -> Result<Option<u64>, StoreError> {
    Ok(list(store, board)?.first().map(|v| v.created))
}

pub fn get_info(store: &Store, board: &str, id: u64) -> Result<Option<VersionInfo>, StoreError> {
    store.get_seq(VERSION_INFO, board, id)
}

pub fn state(store: &Store, board: &str, id: u64) -> Result<Option<Vec<u8>>, StoreError> {
    store.get_seq_raw(VERSIONS, board, id)
}

pub fn rename(
    store: &Store,
    board: &str,
    id: u64,
    label: Option<&str>,
) -> Result<Option<VersionInfo>, StoreError> {
    let Some(mut info) = get_info(store, board, id)? else {
        return Ok(None);
    };
    info.label = clean_label(label);
    store.put_seq(VERSION_INFO, board, id, &info)?;
    Ok(Some(info))
}

pub fn delete_all(store: &Store, board: &str) -> Result<(), StoreError> {
    store.clear_seq(VERSIONS, board)?;
    store.clear_seq(VERSION_INFO, board)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_prune() {
        let store = Store::memory();
        let state = Board::welcome().encode_diff(None).unwrap();
        let named = create(&store, "b", &state, Some("  初稿  "), None).unwrap();
        assert_eq!(named.label.as_deref(), Some("初稿"));
        assert_eq!(named.elements, Board::welcome().len());
        for _ in 0..MAX_AUTO_VERSIONS + 5 {
            create(&store, "b", &state, None, None).unwrap();
        }
        let all = list(&store, "b").unwrap();
        // 名前付きは残り、自動保存は上限まで。
        assert_eq!(all.len(), MAX_AUTO_VERSIONS + 1);
        assert!(all.iter().any(|v| v.id == named.id));
        assert!(all[0].created >= all[all.len() - 1].created);
        assert_eq!(super::state(&store, "b", named.id).unwrap().unwrap(), state);
        let renamed = rename(&store, "b", named.id, Some("第 2 稿"))
            .unwrap()
            .unwrap();
        assert_eq!(renamed.label.as_deref(), Some("第 2 稿"));
        delete_all(&store, "b").unwrap();
        assert!(list(&store, "b").unwrap().is_empty());
    }
}
