//! ルーム(1 枚のボード)と、その集合。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use board_core::Board;
use tokio::sync::broadcast;
use yrs::types::ToJson;
use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, Map, MapPrelim, Out, ReadTxn, StateVector, Transact, Update};

use crate::sealed::{self, CompactError, SealedLog};
use crate::store::Store;

/// ボードの要素を入れるルートマップの名前(`board_core::Board` と同じ)。
const ELEMENTS: &str = "elements";

/// ルーム内に配る 1 通。送り主には返さない。
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub from: u32,
    /// 宛先。`None` なら送り主以外の全員。
    pub to: Option<u32>,
    pub bytes: Arc<[u8]>,
}

impl Outgoing {
    /// 中身が空の 1 通は「接続を切って、つなぎ直させる」合図(権限が変わったときなど)。
    pub fn is_kick(&self) -> bool {
        self.bytes.is_empty()
    }

    /// 参加者 `peer` に届けるか。
    pub fn is_for(&self, peer: u32) -> bool {
        self.from != peer && self.to.is_none_or(|t| t == peer)
    }
}

pub struct Room {
    pub name: String,
    doc: Doc,
    /// yrs のトランザクションは同時に 1 つだけ。
    lock: Mutex<()>,
    pub tx: broadcast::Sender<Outgoing>,
    /// 参加者ごとの最新のカーソル(エンコード済み)。新しく来た人に送る。
    presence: Mutex<HashMap<u32, Arc<[u8]>>>,
    /// 参加者ごとの最新の表示範囲(エンコード済み)。途中参加でもプレゼンに追従できるように。
    views: Mutex<HashMap<u32, Arc<[u8]>>>,
    /// 参加者ごとの音声通話の状態(エンコード済み)。通話中の人だけ。
    voices: Mutex<HashMap<u32, Arc<[u8]>>>,
    dirty: AtomicBool,
    peers: AtomicUsize,
    /// 最後のバージョン(履歴)以降に変更があるか。
    unversioned: AtomicBool,
    /// 最後にバージョンを作った時刻(UNIX ミリ秒。0 はまだ調べていない)。
    last_version: AtomicU64,
    /// エンドツーエンド暗号化ボードなら、暗号文のログ。このとき `doc` は使わない(空のまま)。
    sealed: Option<Mutex<SealedLog>>,
}

/// 暗号化ボードでは使えない操作。
pub const SEALED_ERROR: &str = "エンドツーエンド暗号化ボードの中身はサーバーでは読めません";

impl Room {
    fn new(name: String, saved: Option<Vec<u8>>, e2e: bool) -> Self {
        let doc = Doc::new();
        // 暗号化ボード: 保存データは暗号文のログ。新しいボードでも案内の付箋は置かない(サーバーは書けない)。
        let sealed_saved = saved.as_deref().is_some_and(sealed::is_sealed);
        if e2e || sealed_saved {
            let log = match saved.as_deref() {
                Some(bytes) if sealed_saved => SealedLog::decode(bytes).unwrap_or_else(|e| {
                    tracing::error!(room = %name, "{e}");
                    SealedLog::default()
                }),
                Some(_) => {
                    tracing::error!(room = %name, "暗号化ボードに平文の保存データがあるので使いません");
                    SealedLog::default()
                }
                None => SealedLog::default(),
            };
            return Self::with(name, doc, Some(Mutex::new(log)));
        }
        let initial = match saved {
            Some(state) => Some(state),
            // 新しいルームには案内の付箋を置いておく。
            None => Board::welcome().encode_diff(None).ok(),
        };
        let room = Self::with(name, doc, None);
        if let Some(state) = initial
            && let Err(e) = room.apply(&state)
        {
            tracing::error!(room = %room.name, "保存データを読み込めません: {e}");
        }
        // 読み込んだだけではバージョンを作らない。
        room.unversioned.store(false, Ordering::Release);
        room
    }

    fn with(name: String, doc: Doc, sealed: Option<Mutex<SealedLog>>) -> Self {
        Self {
            name,
            doc,
            lock: Mutex::new(()),
            tx: broadcast::channel(1024).0,
            presence: Mutex::default(),
            views: Mutex::default(),
            voices: Mutex::default(),
            dirty: AtomicBool::new(false),
            peers: AtomicUsize::new(0),
            unversioned: AtomicBool::new(false),
            last_version: AtomicU64::new(0),
            sealed,
        }
    }

    /// エンドツーエンド暗号化ボードか。
    pub fn is_sealed(&self) -> bool {
        self.sealed.is_some()
    }

    /// 暗号化ボードの初回同期のメッセージ(エンコード済み)。暗号化ボードでなければ `None`。
    pub fn sealed_sync(&self, you: u32) -> Option<Vec<u8>> {
        let log = self.sealed.as_ref()?.lock().unwrap();
        Some(
            board_core::protocol::Message::SealedSync {
                you,
                base: log.base,
                blobs: log.entries.iter().map(|e| e.to_vec()).collect(),
            }
            .encode(),
        )
    }

    /// 暗号文をログに足し、送り主以外に配る。付けた通し番号を返す。
    pub fn append_sealed(&self, from: u32, blob: Vec<u8>) -> Result<u64, String> {
        let mut log = self
            .sealed
            .as_ref()
            .ok_or("暗号化ボードではありません")?
            .lock()
            .unwrap();
        let seq = log.append(Arc::from(blob.as_slice()));
        self.dirty.store(true, Ordering::Release);
        // 番号順に届くよう、ログを持ったまま配る。
        self.broadcast(
            from,
            Arc::from(board_core::protocol::Message::Sealed { seq, blob }.encode()),
        );
        Ok(seq)
    }

    /// ログの先頭をまとめる(クライアントが全体を暗号化して送ってきたもの)。
    pub fn compact_sealed(&self, base: u64, upto: u64, blob: Vec<u8>) -> Result<(), String> {
        let mut log = self
            .sealed
            .as_ref()
            .ok_or("暗号化ボードではありません")?
            .lock()
            .unwrap();
        match log.compact(base, upto, Arc::from(blob)) {
            Ok(()) => {
                self.dirty.store(true, Ordering::Release);
                Ok(())
            }
            // ほかの人が先にまとめただけなら気にしなくてよい。
            Err(CompactError::Stale) => Ok(()),
            Err(CompactError::Range) => Err("まとめる範囲が正しくありません".into()),
        }
    }

    /// ログ全体を 1 つの暗号文にする(鍵の作り直し)。
    pub fn reset_sealed(&self, blob: Vec<u8>) -> Result<(), String> {
        let mut log = self
            .sealed
            .as_ref()
            .ok_or("暗号化ボードではありません")?
            .lock()
            .unwrap();
        log.reset(Arc::from(blob));
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    /// ログの件数(まとめるかの目安)。
    pub fn sealed_len(&self) -> usize {
        self.sealed
            .as_ref()
            .map_or(0, |l| l.lock().unwrap().entries.len())
    }

    /// 保存するバイト列(平文のボードは yrs v1 更新、暗号化ボードはログ)。
    pub fn persisted(&self) -> Vec<u8> {
        match &self.sealed {
            Some(log) => log.lock().unwrap().encode(),
            None => self.snapshot(),
        }
    }

    /// ボードの中身(要素)を、昔の状態(yrs v1 更新)と同じにする。
    /// 変更は普通の編集として記録されるので、全員にそのまま配れる。返り値はその更新。
    ///
    /// 戻すのは要素だけ。タイマーや投票の状態は今のままにする。
    pub fn restore(&self, old_state: &[u8]) -> Result<Vec<u8>, String> {
        if self.is_sealed() {
            return Err(SEALED_ERROR.into());
        }
        let old = Doc::new();
        old.transact_mut()
            .apply_update(Update::decode_v1(old_state).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let old_map = old.get_or_insert_map(ELEMENTS);
        let old_txn = old.transact();
        let to_any = |o: Out| match o {
            Out::Any(a) => a,
            other => other.to_json(&old_txn),
        };
        // 昔の要素: キー → フィールド。
        let mut wanted: HashMap<String, Vec<(String, Any)>> = HashMap::new();
        for (k, v) in old_map.iter(&old_txn) {
            if let Out::YMap(m) = v {
                let fields = m
                    .iter(&old_txn)
                    .map(|(fk, fv)| (fk.to_string(), to_any(fv)))
                    .collect();
                wanted.insert(k.to_string(), fields);
            }
        }

        let _g = self.lock.lock().unwrap();
        let map = self.doc.get_or_insert_map(ELEMENTS);
        let before = self.doc.transact().state_vector();
        {
            let mut txn = self.doc.transact_mut();
            let current: Vec<String> = map.keys(&txn).map(str::to_string).collect();
            for k in &current {
                if !wanted.contains_key(k) {
                    map.remove(&mut txn, k);
                }
            }
            for (k, fields) in wanted {
                match map.get(&txn, &k) {
                    Some(Out::YMap(m)) => {
                        // 違うフィールドだけ書き換え、昔になかったフィールドは消す。
                        let now: Vec<(String, Any)> = m
                            .iter(&txn)
                            .map(|(fk, fv)| {
                                let a = match fv {
                                    Out::Any(a) => a,
                                    other => other.to_json(&txn),
                                };
                                (fk.to_string(), a)
                            })
                            .collect();
                        for (fk, _) in &now {
                            if !fields.iter().any(|(k2, _)| k2 == fk) {
                                m.remove(&mut txn, fk);
                            }
                        }
                        for (fk, fv) in fields {
                            if !now.iter().any(|(k2, v2)| *k2 == fk && *v2 == fv) {
                                m.insert(&mut txn, fk, fv);
                            }
                        }
                    }
                    _ => {
                        let prelim: MapPrelim = fields.into_iter().collect();
                        map.insert(&mut txn, k, prelim);
                    }
                }
            }
        }
        let update = self.doc.transact().encode_state_as_update_v1(&before);
        self.dirty.store(true, Ordering::Release);
        self.unversioned.store(true, Ordering::Release);
        Ok(update)
    }

    /// 更新を取り込む。
    pub fn apply(&self, update: &[u8]) -> Result<(), String> {
        if self.is_sealed() {
            return Err(SEALED_ERROR.into());
        }
        let update = Update::decode_v1(update).map_err(|e| e.to_string())?;
        let _g = self.lock.lock().unwrap();
        let mut txn = self.doc.transact_mut();
        let before = txn.state_vector();
        txn.apply_update(update).map_err(|e| e.to_string())?;
        // つなぎ直しで手元の全体が届いただけなど、何も変わらなければ印を付けない。
        let changed = txn.state_vector() != before || !txn.delete_set().is_empty();
        drop(txn);
        self.dirty.store(true, Ordering::Release);
        if changed {
            self.unversioned.store(true, Ordering::Release);
        }
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

    pub fn set_view(&self, peer: u32, encoded: Arc<[u8]>) {
        self.views.lock().unwrap().insert(peer, encoded);
    }

    /// 通話の状態を覚える(通話をやめたら忘れる)。
    pub fn set_voice(&self, peer: u32, in_call: bool, encoded: Arc<[u8]>) {
        let mut voices = self.voices.lock().unwrap();
        if in_call {
            voices.insert(peer, encoded);
        } else {
            voices.remove(&peer);
        }
    }

    /// 通話中の人数。
    pub fn voice_count(&self) -> usize {
        self.voices.lock().unwrap().len()
    }

    /// カーソル・表示範囲・通話の状態を消す。どれかを配っていたなら true。
    pub fn remove_presence(&self, peer: u32) -> bool {
        let view = self.views.lock().unwrap().remove(&peer).is_some();
        let voice = self.voices.lock().unwrap().remove(&peer).is_some();
        self.presence.lock().unwrap().remove(&peer).is_some() || view || voice
    }

    /// 新しく来た人に送る、他の参加者のカーソル・表示範囲・通話の状態。
    pub fn presences(&self) -> Vec<Arc<[u8]>> {
        let mut out: Vec<_> = self.presence.lock().unwrap().values().cloned().collect();
        out.extend(self.views.lock().unwrap().values().cloned());
        out.extend(self.voices.lock().unwrap().values().cloned());
        out
    }

    pub fn peer_count(&self) -> usize {
        self.peers.load(Ordering::Acquire)
    }

    pub fn broadcast(&self, from: u32, bytes: Arc<[u8]>) {
        // 受け手がいないときのエラーは無視してよい。
        let _ = self.tx.send(Outgoing {
            from,
            to: None,
            bytes,
        });
    }

    /// 1 人にだけ送る(通話の接続手順)。
    pub fn send_to(&self, from: u32, to: u32, bytes: Arc<[u8]>) {
        let _ = self.tx.send(Outgoing {
            from,
            to: Some(to),
            bytes,
        });
    }

    /// 参加者全員の接続を切る(クライアントはつなぎ直し、そのとき権限を確かめ直す)。
    pub fn kick_all(&self) {
        self.broadcast(0, Arc::from(Vec::new()));
    }
}

/// 全ルームと保存先。
pub struct Hub {
    rooms: Mutex<HashMap<String, Arc<Room>>>,
    store: Arc<Store>,
    next_peer: AtomicU32,
    /// 自動でバージョンを作る最短間隔(ミリ秒)。
    version_interval_ms: AtomicU64,
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
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            rooms: Mutex::default(),
            store,
            next_peer: AtomicU32::new(1),
            version_interval_ms: AtomicU64::new(crate::versions::AUTO_INTERVAL_MS),
        }
    }

    /// ルームに入る。なければ保存データから(なければ新規に)作る。
    pub fn join(&self, name: &str) -> Membership {
        let mut rooms = self.rooms.lock().unwrap();
        let room = rooms
            .entry(name.to_string())
            .or_insert_with(|| {
                let saved = match self.store.load(name) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!(room = name, "読み込みに失敗: {e}");
                        None
                    }
                };
                let e2e = match crate::boards::get(&self.store, name) {
                    Ok(meta) => meta.is_some_and(|m| m.e2e.is_some()),
                    Err(e) => {
                        tracing::error!(room = name, "ボードの情報を読めません: {e}");
                        false
                    }
                };
                tracing::info!(
                    room = name,
                    restored = saved.is_some(),
                    e2e,
                    "ルームを開きました"
                );
                Arc::new(Room::new(name.to_string(), saved, e2e))
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

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// 開いているルーム(なければ `None`)。
    pub fn get(&self, name: &str) -> Option<Arc<Room>> {
        self.rooms.lock().unwrap().get(name).cloned()
    }

    /// 参加者全員をつなぎ直させる(権限を確かめ直すため)。
    pub fn kick(&self, name: &str) {
        if let Some(room) = self.get(name) {
            room.kick_all();
        }
    }

    /// 参加者に「取り直して」と知らせる(コメントなど HTTP API で持つ情報が変わったとき)。
    pub fn refresh(&self, name: &str, topic: u8) {
        if let Some(room) = self.get(name) {
            let msg = board_core::protocol::Message::Refresh(topic).encode();
            room.broadcast(0, Arc::from(msg));
        }
    }

    /// ルームを消す(参加者は切断され、保存データも消える)。
    pub fn delete(&self, name: &str) -> Result<(), crate::store::StoreError> {
        if let Some(room) = self.rooms.lock().unwrap().remove(name) {
            room.dirty.store(false, Ordering::Release);
            room.kick_all();
        }
        self.store.delete_room(name)
    }

    /// 変更のあったルームを保存し、誰もいないルームをメモリから外す。
    pub fn flush(&self) {
        let rooms: Vec<Arc<Room>> = self.rooms.lock().unwrap().values().cloned().collect();
        for room in &rooms {
            self.save(room);
        }
        let mut map = self.rooms.lock().unwrap();
        map.retain(|_, room| {
            // 保存が済んでいて誰もいないルームだけ外す。
            room.peer_count() > 0 || room.dirty.load(Ordering::Acquire)
        });
    }

    fn save(&self, room: &Room) {
        let store = &self.store;
        if !room.dirty.swap(false, Ordering::AcqRel) {
            // 保存は済んでいても、バージョンはまだかもしれない(全員が抜けたときなど)。
            self.auto_version(room, None);
            return;
        }
        let state = room.persisted();
        match store.save(&room.name, &state) {
            Ok(()) => tracing::debug!(room = %room.name, bytes = state.len(), "保存しました"),
            Err(e) => {
                room.dirty.store(true, Ordering::Release);
                tracing::error!(room = %room.name, "保存に失敗: {e}");
                return;
            }
        }
        self.auto_version(room, Some(&state));
    }

    /// 自動のバージョン: 変更があり、前回から一定時間たったか、全員が抜けたときに作る。
    fn auto_version(&self, room: &Room, state: Option<&[u8]>) {
        // 暗号化ボードは中身を読めないので、サーバーではバージョンを作らない。
        if room.is_sealed() {
            return;
        }
        if !room.unversioned.load(Ordering::Acquire) {
            return;
        }
        let now = crate::auth::now_ms();
        let mut last = room.last_version.load(Ordering::Acquire);
        if last == 0 {
            last = crate::versions::latest_time(&self.store, &room.name)
                .ok()
                .flatten()
                .unwrap_or(1);
            room.last_version.store(last, Ordering::Release);
        }
        let interval = self.version_interval_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < interval && room.peer_count() > 0 {
            return;
        }
        room.unversioned.store(false, Ordering::Release);
        let snapshot;
        let state = match state {
            Some(s) => s,
            None => {
                snapshot = room.snapshot();
                &snapshot
            }
        };
        match crate::versions::create(&self.store, &room.name, state, None, None) {
            Ok(v) => {
                room.last_version.store(v.created, Ordering::Release);
                room.broadcast(
                    0,
                    Arc::from(
                        board_core::protocol::Message::Refresh(
                            board_core::protocol::TOPIC_VERSIONS,
                        )
                        .encode(),
                    ),
                );
            }
            Err(e) => {
                room.unversioned.store(true, Ordering::Release);
                tracing::error!(room = %room.name, "バージョンを保存できません: {e}");
            }
        }
    }

    /// 自動バージョンの間隔を変える(テスト用)。
    pub fn set_version_interval(&self, ms: u64) {
        self.version_interval_ms.store(ms, Ordering::Relaxed);
    }

    /// ボードの今の状態。開いていればメモリから、なければ保存データから。
    ///
    /// 暗号化ボードは中身を読めないので `None`(検索・公開ページ・バージョンなどに出さない)。
    pub fn current_state(&self, name: &str) -> Result<Option<Vec<u8>>, crate::store::StoreError> {
        if let Some(room) = self.get(name) {
            if room.is_sealed() {
                return Ok(None);
            }
            return Ok(Some(room.snapshot()));
        }
        Ok(self.store.load(name)?.filter(|b| !sealed::is_sealed(b)))
    }

    /// 暗号化ボードの鍵を作り直した: ログを新しい鍵の暗号文 1 つにし、保存して全員をつなぎ直させる。
    pub fn reset_sealed(&self, name: &str, blob: Vec<u8>) -> Result<(), String> {
        let member = self.join(name);
        member.room.reset_sealed(blob)?;
        member.room.kick_all();
        drop(member);
        self.flush();
        Ok(())
    }

    /// ボードの中身を読む(REST API・プラグイン用)。開いていなければ開く。
    pub fn read<R>(&self, name: &str, f: impl FnOnce(&Board) -> R) -> Result<R, String> {
        let member = self.join(name);
        if member.room.is_sealed() {
            return Err(SEALED_ERROR.into());
        }
        let mut board = Board::new();
        board.apply_remote(&member.room.snapshot())?;
        Ok(f(&board))
    }

    /// サーバー側でボードを編集し、変更を参加者全員に配る(REST API・プラグイン用)。
    ///
    /// 今の状態を写した使い捨てのボードで `f` を実行し、`Ok` のときだけ変更をルームに取り込む。
    /// その間に届いた他の人の編集とは yrs がそのまま合わせる。
    pub fn edit<R, E: From<String>>(
        &self,
        name: &str,
        f: impl FnOnce(&mut Board) -> Result<R, E>,
    ) -> Result<R, E> {
        let member = self.join(name);
        if member.room.is_sealed() {
            return Err(E::from(SEALED_ERROR.to_string()));
        }
        let mut board = Board::new();
        board.apply_remote(&member.room.snapshot())?;
        let out = f(&mut board)?;
        let updates = board.take_outgoing();
        if !updates.is_empty() {
            let merged = yrs::merge_updates_v1(updates.iter().map(Vec::as_slice))
                .map_err(|e| e.to_string())?;
            member.room.apply(&merged)?;
            member.room.broadcast(
                0,
                Arc::from(board_core::protocol::Message::Update(merged).encode()),
            );
        }
        Ok(out)
    }

    /// 昔の状態に戻し、参加者全員に配る。
    pub fn restore(&self, name: &str, old_state: &[u8]) -> Result<(), String> {
        // 開いていなければ開く(戻した結果は次の保存で書き出される)。
        let member = self.join(name);
        let update = member.room.restore(old_state)?;
        member.room.broadcast(
            0,
            Arc::from(board_core::protocol::Message::Update(update).encode()),
        );
        member
            .room
            .last_version
            .store(crate::auth::now_ms(), Ordering::Release);
        drop(member);
        self.flush();
        Ok(())
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
        let hub = Hub::new(Arc::new(Store::memory()));
        let m = hub.join("a");
        assert_eq!(state_len(&m.room), Board::welcome().len());
    }

    #[test]
    fn rooms_persist_and_unload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.redb");
        let hub = Hub::new(Arc::new(Store::open(&path).unwrap()));
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

        let hub = Hub::new(Arc::new(Store::open(&path).unwrap()));
        let m = hub.join("r");
        assert_eq!(state_len(&m.room), Board::welcome().len() + 1);
    }

    fn client_of(room: &Room) -> Board {
        let mut b = Board::new();
        b.apply_remote(&room.snapshot()).unwrap();
        b
    }

    #[test]
    fn restore_brings_back_old_elements() {
        let hub = Hub::new(Arc::new(Store::memory()));
        let m = hub.join("r");
        let mut a = client_of(&m.room);
        let old = m.room.snapshot();
        let old_ids: Vec<_> = a.elements().iter().map(|e| e.id()).collect();

        // 1 つ消し、1 つ動かし、1 つ足す。
        a.remove(&old_ids[..1]);
        a.update(old_ids[1], |e| {
            if let board_core::Element::Shape(s) = e {
                s.pos += board_core::Vec2::new(500.0, 0.0);
            }
        });
        let id = a.alloc_id();
        a.push(board_core::Shape::rect(
            id,
            board_core::Vec2::ZERO,
            board_core::Vec2::ONE,
            board_core::Rgba8::WHITE,
        ));
        for u in a.take_outgoing() {
            m.room.apply(&u).unwrap();
        }
        assert!(client_of(&m.room).get(id).is_some());

        // 別の参加者 b は戻す前の状態を持っていて、戻した更新を受け取る。
        let mut b = client_of(&m.room);
        let update = m.room.restore(&old).unwrap();
        b.apply_remote(&update).unwrap();
        for board in [client_of(&m.room), b] {
            let ids: Vec<_> = board.elements().iter().map(|e| e.id()).collect();
            assert_eq!(ids.len(), old_ids.len());
            assert!(board.get(id).is_none());
            for oid in &old_ids {
                assert!(board.get(*oid).is_some());
            }
            assert_eq!(
                board.get(old_ids[1]),
                Board::welcome().get(old_ids[1]).or(board.get(old_ids[1]))
            );
        }
        let restored = client_of(&m.room);
        let mut original = Board::new();
        original.apply_remote(&old).unwrap();
        assert_eq!(restored.get(old_ids[1]), original.get(old_ids[1]));
    }

    #[test]
    fn auto_versions() {
        let hub = Hub::new(Arc::new(Store::memory()));
        hub.set_version_interval(0);
        let m = hub.join("v");
        // 読み込んだだけでは作らない。
        hub.flush();
        assert!(crate::versions::list(hub.store(), "v").unwrap().is_empty());
        let mut a = client_of(&m.room);
        let id = a.alloc_id();
        a.push(board_core::Shape::rect(
            id,
            board_core::Vec2::ZERO,
            board_core::Vec2::ONE,
            board_core::Rgba8::WHITE,
        ));
        for u in a.take_outgoing() {
            m.room.apply(&u).unwrap();
        }
        hub.flush();
        assert_eq!(crate::versions::list(hub.store(), "v").unwrap().len(), 1);
        // 変更がなければ増えない(つなぎ直しで全体が届いただけのときも)。
        let full = m.room.snapshot();
        m.room.apply(&full).unwrap();
        hub.flush();
        assert_eq!(crate::versions::list(hub.store(), "v").unwrap().len(), 1);

        // 間隔が長くても、全員が抜けたら作る。
        hub.set_version_interval(u64::MAX);
        a.update(id, |e| {
            if let board_core::Element::Shape(s) = e {
                s.pos.x = 10.0;
            }
        });
        for u in a.take_outgoing() {
            m.room.apply(&u).unwrap();
        }
        hub.flush();
        assert_eq!(crate::versions::list(hub.store(), "v").unwrap().len(), 1);
        drop(m);
        hub.flush();
        assert_eq!(crate::versions::list(hub.store(), "v").unwrap().len(), 2);
    }

    #[test]
    fn peers_get_distinct_ids() {
        let hub = Hub::new(Arc::new(Store::memory()));
        let a = hub.join("x");
        let b = hub.join("x");
        assert_ne!(a.peer, b.peer);
        assert_eq!(a.room.peer_count(), 2);
        drop(b);
        assert_eq!(a.room.peer_count(), 1);
    }
}
