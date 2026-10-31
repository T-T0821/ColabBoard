//! ルーム(1 枚のボード)と、その集合。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use board_core::Board;
use tokio::sync::broadcast;
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

use crate::store::Store;

/// ルーム内に配る 1 通。送り主には返さない。
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub from: u32,
    pub bytes: Arc<[u8]>,
}

pub struct Room {
    pub name: String,
    doc: Doc,
    /// yrs のトランザクションは同時に 1 つだけ。
    lock: Mutex<()>,
    pub tx: broadcast::Sender<Outgoing>,
    /// 参加者ごとの最新のカーソル(エンコード済み)。新しく来た人に送る。
    presence: Mutex<HashMap<u32, Arc<[u8]>>>,
    dirty: AtomicBool,
    peers: AtomicUsize,
}

impl Room {
    fn new(name: String, saved: Option<Vec<u8>>) -> Self {
        let doc = Doc::new();
        let initial = match saved {
            Some(state) => Some(state),
            // 新しいルームには案内の付箋を置いておく。
            None => Board::welcome().encode_diff(None).ok(),
        };
        let room = Self {
            name,
            doc,
            lock: Mutex::new(()),
            tx: broadcast::channel(1024).0,
            presence: Mutex::default(),
            dirty: AtomicBool::new(false),
            peers: AtomicUsize::new(0),
        };
        if let Some(state) = initial
            && let Err(e) = room.apply(&state)
        {
            tracing::error!(room = %room.name, "保存データを読み込めません: {e}");
        }
        room
    }

    /// 更新を取り込む。
    pub fn apply(&self, update: &[u8]) -> Result<(), String> {
        let update = Update::decode_v1(update).map_err(|e| e.to_string())?;
        let _g = self.lock.lock().unwrap();
        self.doc
            .transact_mut()
            .apply_update(update)
            .map_err(|e| e.to_string())?;
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    /// ボード全体(yrs v1 更新)。
    pub fn snapshot(&self) -> Vec<u8> {
        let _g = self.lock.lock().unwrap();
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    pub fn set_presence(&self, peer: u32, encoded: Arc<[u8]>) {
        self.presence.lock().unwrap().insert(peer, encoded);
    }

    /// カーソルを消す。表示していたなら true。
    pub fn remove_presence(&self, peer: u32) -> bool {
        self.presence.lock().unwrap().remove(&peer).is_some()
    }

    pub fn presences(&self) -> Vec<Arc<[u8]>> {
        self.presence.lock().unwrap().values().cloned().collect()
    }

    pub fn peer_count(&self) -> usize {
        self.peers.load(Ordering::Acquire)
    }

    pub fn broadcast(&self, from: u32, bytes: Arc<[u8]>) {
        // 受け手がいないときのエラーは無視してよい。
        let _ = self.tx.send(Outgoing { from, bytes });
    }
}

/// 全ルームと保存先。
pub struct Hub {
    rooms: Mutex<HashMap<String, Arc<Room>>>,
    store: Option<Store>,
    next_peer: AtomicU32,
}

/// 参加中の印。落とすと退出になる。
pub struct Membership {
    pub room: Arc<Room>,
    pub peer: u32,
}

impl Drop for Membership {
    fn drop(&mut self) {
        self.room.peers.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Hub {
    pub fn new(store: Option<Store>) -> Self {
        Self {
            rooms: Mutex::default(),
            store,
            next_peer: AtomicU32::new(1),
        }
    }

    /// ルームに入る。なければ保存データから(なければ新規に)作る。
    pub fn join(&self, name: &str) -> Membership {
        let mut rooms = self.rooms.lock().unwrap();
        let room = rooms
            .entry(name.to_string())
            .or_insert_with(|| {
                let saved = self.store.as_ref().and_then(|s| match s.load(name) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!(room = name, "読み込みに失敗: {e}");
                        None
                    }
                });
                tracing::info!(
                    room = name,
                    restored = saved.is_some(),
                    "ルームを開きました"
                );
                Arc::new(Room::new(name.to_string(), saved))
            })
            .clone();
        // ルームの片付け(`flush`)と競合しないよう、ロックを持ったまま数える。
        room.peers.fetch_add(1, Ordering::AcqRel);
        let peer = self.next_peer.fetch_add(1, Ordering::Relaxed);
        Membership { room, peer }
    }

    pub fn room_count(&self) -> usize {
        self.rooms.lock().unwrap().len()
    }

    /// 変更のあったルームを保存し、誰もいないルームをメモリから外す。
    pub fn flush(&self) {
        let rooms: Vec<Arc<Room>> = self.rooms.lock().unwrap().values().cloned().collect();
        for room in &rooms {
            self.save(room);
        }
        let mut map = self.rooms.lock().unwrap();
        map.retain(|_, room| {
            // 保存が済んでいて誰もいないルームだけ外す(保存先がなければ残す)。
            self.store.is_none() || room.peer_count() > 0 || room.dirty.load(Ordering::Acquire)
        });
    }

    fn save(&self, room: &Room) {
        let Some(store) = &self.store else { return };
        if !room.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let state = room.snapshot();
        match store.save(&room.name, &state) {
            Ok(()) => tracing::debug!(room = %room.name, bytes = state.len(), "保存しました"),
            Err(e) => {
                room.dirty.store(true, Ordering::Release);
                tracing::error!(room = %room.name, "保存に失敗: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_len(room: &Room) -> usize {
        let mut b = Board::new();
        b.apply_remote(&room.snapshot()).unwrap();
        b.len()
    }

    #[test]
    fn new_room_has_welcome_board() {
        let hub = Hub::new(None);
        let m = hub.join("a");
        assert_eq!(state_len(&m.room), Board::welcome().len());
    }

    #[test]
    fn rooms_persist_and_unload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.redb");
        let hub = Hub::new(Some(Store::open(&path).unwrap()));
        {
            let m = hub.join("r");
            let mut client = Board::new();
            client.apply_remote(&m.room.snapshot()).unwrap();
            let id = client.alloc_id();
            client.push(board_core::Shape::rect(
                id,
                board_core::Vec2::ZERO,
                board_core::Vec2::ONE,
                board_core::Rgba8::WHITE,
            ));
            for u in client.take_outgoing() {
                m.room.apply(&u).unwrap();
            }
            hub.flush();
            // 参加中は外さない。
            assert_eq!(hub.room_count(), 1);
        }
        hub.flush();
        assert_eq!(hub.room_count(), 0);
        drop(hub);

        let hub = Hub::new(Some(Store::open(&path).unwrap()));
        let m = hub.join("r");
        assert_eq!(state_len(&m.room), Board::welcome().len() + 1);
    }

    #[test]
    fn peers_get_distinct_ids() {
        let hub = Hub::new(None);
        let a = hub.join("x");
        let b = hub.join("x");
        assert_ne!(a.peer, b.peer);
        assert_eq!(a.room.peer_count(), 2);
        drop(b);
        assert_eq!(a.room.peer_count(), 1);
    }
}
