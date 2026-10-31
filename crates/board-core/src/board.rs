use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use glam::Vec2;
use yrs::types::{Event, PathSegment};
use yrs::undo::Options as UndoOptions;
use yrs::updates::decoder::Decode;
use yrs::{
    DeepObservable, Doc, Map, MapPrelim, MapRef, Origin, ReadTxn, StateVector, Transact,
    UndoManager, Update,
};

use crate::color::{Rgba8, STICKY_COLORS};
use crate::crdt;
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::Shape;

/// 自分の操作(Undo の対象)。
const ORIGIN_LOCAL: &str = "local";
/// 他の参加者から届いた更新。
const ORIGIN_REMOTE: &str = "remote";
/// 初期データの投入など、Undo の対象にしない操作。
const ORIGIN_INIT: &str = "init";

/// 共同編集用の yrs ドキュメントとその付属物。
struct Sync {
    doc: Doc,
    map: MapRef,
    undo: UndoManager,
    /// 前回の反映以降に変わった要素のキー。
    changed: Arc<Mutex<HashSet<Arc<str>>>>,
    /// 他の参加者へ送る更新(yrs v1 形式)。
    outbox: Arc<Mutex<Vec<Vec<u8>>>>,
}

/// 監視コールバックの登録キー。
const OBSERVER_KEY: &str = "colabboard";

/// ボード全体。並び順がそのまま描画順(後ろほど手前)。
///
/// 共同編集できるボードは yrs(CRDT)のドキュメントを正とし、`elements` はその表示用のビュー。
/// 変更はすべてドキュメントに書き、変わった要素だけをビューに反映する。
/// 性能確認用のダミーボードはドキュメントを持たない(`Board::local`)。
pub struct Board {
    elements: Vec<Element>,
    /// `elements` と同じ並びの重なり順キー。
    z: Vec<f64>,
    index: HashMap<ElementId, usize>,
    /// 内容が変わるたびに増える。描画側がバッファ再転送の要否を判断する。
    revision: u64,
    /// 要素 ID の上位 32 ビット(参加者ごとに違う)。
    client: u64,
    counter: u64,
    sync: Option<Sync>,
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Board {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Board")
            .field("len", &self.elements.len())
            .field("revision", &self.revision)
            .field("collaborative", &self.sync.is_some())
            .finish()
    }
}

impl Board {
    /// 共同編集できる空のボード。
    pub fn new() -> Self {
        Self::with_doc(Doc::new())
    }

    /// 参加者 ID を指定して作る(テスト用)。
    pub fn with_client_id(client_id: u64) -> Self {
        Self::with_doc(Doc::with_client_id(client_id))
    }

    fn with_doc(doc: Doc) -> Self {
        let map = doc.get_or_insert_map("elements");
        let changed: Arc<Mutex<HashSet<Arc<str>>>> = Arc::default();
        let outbox: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();

        let changed2 = changed.clone();
        map.observe_deep(
            OBSERVER_KEY,
            move |txn: &yrs::TransactionMut, events: &yrs::types::Events| {
                let mut set = changed2.lock().unwrap();
                for e in events.iter() {
                    match e.path().front() {
                        Some(PathSegment::Key(k)) => {
                            set.insert(k.clone());
                        }
                        _ => {
                            if let Event::Map(m) = e {
                                set.extend(m.keys(txn).keys().cloned());
                            }
                        }
                    }
                }
            },
        );
        let outbox2 = outbox.clone();
        let remote = Origin::from(ORIGIN_REMOTE);
        doc.observe_update_v1(
            OBSERVER_KEY,
            move |txn: &yrs::TransactionMut, u: &yrs::UpdateEvent| {
                if txn.origin() != Some(&remote) {
                    outbox2.lock().unwrap().push(u.update.clone());
                }
            },
        )
        .expect("observe_update_v1");

        // 1 回の操作(ドラッグ 1 回など)を 1 つの Undo 単位にしたいので、時間ではなく
        // `end_gesture` での区切りだけでまとめる。
        let options = UndoOptions {
            capture_timeout_millis: u64::MAX,
            tracked_origins: HashSet::new(),
            capture_transaction: None,
            timestamp: Arc::new(|| 1u64),
            init_undo_stack: Vec::new(),
            init_redo_stack: Vec::new(),
        };
        let mut undo = UndoManager::with_options(options);
        undo.expand_scope(&doc, &map);
        undo.include_origin(Origin::from(ORIGIN_LOCAL));

        let client = doc.client_id().get() & 0xffff_ffff;
        Self {
            elements: Vec::new(),
            z: Vec::new(),
            index: HashMap::new(),
            revision: 0,
            client,
            counter: 0,
            sync: Some(Sync {
                doc,
                map,
                undo,
                changed,
                outbox,
            }),
        }
    }

    /// 共同編集しないボード(性能確認用。大量の要素を軽く扱える)。
    pub fn local() -> Self {
        Self {
            elements: Vec::new(),
            z: Vec::new(),
            index: HashMap::new(),
            revision: 0,
            client: 0,
            counter: 0,
            sync: None,
        }
    }

    pub fn is_collaborative(&self) -> bool {
        self.sync.is_some()
    }

    /// 共同編集用のドキュメント(要素以外の共有状態に使う)。
    pub(crate) fn doc(&self) -> Option<&Doc> {
        self.sync.as_ref().map(|s| &s.doc)
    }

    pub fn elements(&self) -> &[Element] {
        &self.elements
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// 参加者ごとに重ならない要素 ID を払い出す。
    pub fn alloc_id(&mut self) -> ElementId {
        self.counter += 1;
        ElementId((self.client << 32) | self.counter)
    }

    pub fn get(&self, id: ElementId) -> Option<&Element> {
        self.index.get(&id).map(|&i| &self.elements[i])
    }

    fn top_z(&self) -> f64 {
        self.z.last().copied().unwrap_or(0.0)
    }

    // ---- 変更 ----

    /// 要素を変更する。変更があればドキュメントに書く。
    pub fn update<R>(&mut self, id: ElementId, f: impl FnOnce(&mut Element) -> R) -> Option<R> {
        let i = *self.index.get(&id)?;
        let mut e = self.elements[i].clone();
        let r = f(&mut e);
        if e != self.elements[i] {
            let z = self.z[i];
            self.write(ORIGIN_LOCAL, vec![(id, Some((e, z)))]);
        }
        Some(r)
    }

    /// 要素を追加する(同じ ID があれば置き換える)。新しい要素は最前面。
    pub fn push(&mut self, element: impl Into<Element>) -> ElementId {
        self.push_with(ORIGIN_LOCAL, element.into())
    }

    fn push_with(&mut self, origin: &str, element: Element) -> ElementId {
        let id = element.id();
        let z = match self.index.get(&id) {
            Some(&i) => self.z[i],
            None => self.top_z() + 1.0,
        };
        self.write(origin, vec![(id, Some((element, z)))]);
        id
    }

    pub fn remove(&mut self, ids: &[ElementId]) -> usize {
        let present: Vec<_> = ids
            .iter()
            .copied()
            .filter(|id| self.index.contains_key(id))
            .collect();
        let n = present.len();
        if n > 0 {
            self.write(
                ORIGIN_LOCAL,
                present.into_iter().map(|id| (id, None)).collect(),
            );
        }
        n
    }

    /// 変更をドキュメント(なければビュー)に書く。`None` は削除。
    fn write(&mut self, origin: &str, changes: Vec<(ElementId, Option<(Element, f64)>)>) {
        let Some(sync) = self.sync.as_ref() else {
            for (id, change) in changes {
                match change {
                    Some((e, z)) => self.upsert_view(e, z),
                    None => self.remove_view(id),
                }
            }
            return;
        };
        {
            let mut txn = sync.doc.transact_mut_with(origin);
            for (id, change) in changes {
                let key = crdt::key_of(id);
                match change {
                    None => {
                        sync.map.remove(&mut txn, &key);
                    }
                    Some((e, z)) => {
                        let fields = crdt::encode(&e, z);
                        match sync.map.get(&txn, &key) {
                            Some(yrs::Out::YMap(m)) => {
                                // 変わったフィールドだけ書く(同時編集を壊さないため)。
                                let old = self
                                    .index
                                    .get(&id)
                                    .map(|&i| crdt::encode(&self.elements[i], self.z[i]))
                                    .unwrap_or_default();
                                for (k, v) in fields {
                                    if !old.iter().any(|(ok, ov)| *ok == k && *ov == v) {
                                        m.insert(&mut txn, k, v);
                                    }
                                }
                            }
                            _ => {
                                let prelim: MapPrelim = fields.into_iter().collect();
                                sync.map.insert(&mut txn, key, prelim);
                            }
                        }
                    }
                }
            }
        }
        self.reconcile();
    }

    /// ドキュメントで変わった要素をビューに反映する。
    fn reconcile(&mut self) {
        let Some(sync) = self.sync.as_ref() else {
            return;
        };
        let keys: Vec<Arc<str>> = sync.changed.lock().unwrap().drain().collect();
        if keys.is_empty() {
            return;
        }
        let mut updates = Vec::with_capacity(keys.len());
        {
            let txn = sync.doc.transact();
            for key in keys {
                let Some(id) = crdt::id_of(&key) else {
                    continue;
                };
                let decoded = match sync.map.get(&txn, &key) {
                    Some(yrs::Out::YMap(m)) => crdt::decode(id, &m, &txn),
                    _ => None,
                };
                updates.push((id, decoded));
            }
        }
        for (id, decoded) in updates {
            match decoded {
                Some((e, z)) => self.upsert_view(e, z),
                None => self.remove_view(id),
            }
        }
    }

    fn upsert_view(&mut self, e: Element, z: f64) {
        let id = e.id();
        // 他の参加者の ID と衝突しないよう、自分の払い出し番号を進めておく。
        if id.0 >> 32 == self.client {
            self.counter = self.counter.max(id.0 & 0xffff_ffff);
        }
        self.revision += 1;
        if let Some(&i) = self.index.get(&id) {
            if self.z[i] == z {
                self.elements[i] = e;
                return;
            }
            self.remove_view(id);
        }
        // (z, id) の順に並べる。最前面への追加は末尾なので速い。
        let pos = if self.z.last().is_none_or(|&last| z > last) {
            self.elements.len()
        } else {
            let mut lo = 0;
            let mut hi = self.elements.len();
            while lo < hi {
                let mid = (lo + hi) / 2;
                if (self.z[mid], self.elements[mid].id()) < (z, id) {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        self.elements.insert(pos, e);
        self.z.insert(pos, z);
        if pos + 1 == self.elements.len() {
            self.index.insert(id, pos);
        } else {
            self.reindex_from(pos);
        }
    }

    fn remove_view(&mut self, id: ElementId) {
        if let Some(i) = self.index.remove(&id) {
            self.elements.remove(i);
            self.z.remove(i);
            self.reindex_from(i);
            self.revision += 1;
        }
    }

    fn reindex_from(&mut self, start: usize) {
        for (i, e) in self.elements.iter().enumerate().skip(start) {
            self.index.insert(e.id(), i);
        }
    }

    // ---- Undo / 同期 ----

    /// 1 回の操作の区切り。次の変更は新しい Undo 単位になる。
    pub fn end_gesture(&mut self) {
        if let Some(sync) = self.sync.as_mut() {
            sync.undo.reset();
        }
    }

    /// 自分の直前の操作を取り消す。他の参加者の変更は取り消さない。
    pub fn undo(&mut self) -> bool {
        let done = self.sync.as_mut().is_some_and(|s| s.undo.undo_blocking());
        self.reconcile();
        done
    }

    pub fn redo(&mut self) -> bool {
        let done = self.sync.as_mut().is_some_and(|s| s.undo.redo_blocking());
        self.reconcile();
        done
    }

    pub fn can_undo(&self) -> bool {
        self.sync.as_ref().is_some_and(|s| s.undo.can_undo())
    }

    pub fn can_redo(&self) -> bool {
        self.sync.as_ref().is_some_and(|s| s.undo.can_redo())
    }

    /// 他の参加者へ送る更新を取り出す。
    pub fn take_outgoing(&mut self) -> Vec<Vec<u8>> {
        self.sync
            .as_ref()
            .map(|s| std::mem::take(&mut *s.outbox.lock().unwrap()))
            .unwrap_or_default()
    }

    /// 他の参加者から届いた更新(yrs v1 形式)を取り込む。
    pub fn apply_remote(&mut self, update: &[u8]) -> Result<(), String> {
        let sync = self.sync.as_ref().ok_or("共同編集できないボードです")?;
        let update = Update::decode_v1(update).map_err(|e| e.to_string())?;
        sync.doc
            .transact_mut_with(ORIGIN_REMOTE)
            .apply_update(update)
            .map_err(|e| e.to_string())?;
        self.reconcile();
        Ok(())
    }

    /// 相手の状態ベクトル(なければ空)以降の差分を作る。初回同期に使う。
    pub fn encode_diff(&self, state_vector: Option<&[u8]>) -> Result<Vec<u8>, String> {
        let sync = self.sync.as_ref().ok_or("共同編集できないボードです")?;
        let sv = match state_vector {
            Some(b) => StateVector::decode_v1(b).map_err(|e| e.to_string())?,
            None => StateVector::default(),
        };
        Ok(sync.doc.transact().encode_state_as_update_v1(&sv))
    }

    pub fn state_vector(&self) -> Vec<u8> {
        use yrs::updates::encoder::Encode;
        self.sync
            .as_ref()
            .map(|s| s.doc.transact().state_vector().encode_v1())
            .unwrap_or_default()
    }

    /// 全要素を囲む矩形。空なら `None`。
    pub fn bounds(&self) -> Option<Rect> {
        Self::union_bounds(self.elements.iter())
    }

    /// 指定要素を囲む矩形。
    pub fn bounds_of(&self, ids: &[ElementId]) -> Option<Rect> {
        Self::union_bounds(ids.iter().filter_map(|id| self.get(*id)))
    }

    fn union_bounds<'a>(mut it: impl Iterator<Item = &'a Element>) -> Option<Rect> {
        let first = it.next()?.bounds();
        Some(it.fold(first, |r, e| r.union(&e.bounds())))
    }

    /// 最前面から探して、点の下にある要素を返す。
    pub fn hit_test(&self, world: Vec2, tolerance: f32) -> Option<&Element> {
        self.elements.iter().rev().find(|e| e.hit(world, tolerance))
    }

    /// 矩形に完全に含まれる要素(範囲選択用)。
    pub fn query_inside(&self, r: &Rect) -> Vec<ElementId> {
        self.elements
            .iter()
            .filter(|e| r.contains_rect(&e.bounds()))
            .map(Element::id)
            .collect()
    }

    /// 初回表示用の小さなボード。
    pub fn welcome() -> Self {
        let mut b = Self::new();
        b.seed_welcome();
        b
    }

    /// 初期表示用の要素を(Undo の対象外として)追加する。
    pub fn seed_welcome(&mut self) {
        let b = self;
        let notes = [
            (Vec2::new(0.0, 0.0), STICKY_COLORS[0]),
            (Vec2::new(240.0, 0.0), STICKY_COLORS[4]),
            (Vec2::new(480.0, 0.0), STICKY_COLORS[2]),
        ];
        let texts = [
            "ようこそ ColabBoard へ!\nダブルクリックで編集できます",
            "付箋・図形・ペンは下のツールバーから",
            "Ctrl/⌘ + スクロールでズーム",
        ];
        for ((pos, c), text) in notes.into_iter().zip(texts) {
            let id = b.alloc_id();
            let mut note = Shape::sticky(id, pos, c);
            note.text = text.to_string();
            b.push_with(ORIGIN_INIT, note.into());
        }
        let id = b.alloc_id();
        let rect = Shape::rect(
            id,
            Vec2::new(0.0, 260.0),
            Vec2::new(300.0, 140.0),
            Rgba8::hex(0xe8f0fe),
        );
        b.push_with(ORIGIN_INIT, rect.into());
        let id = b.alloc_id();
        let ellipse = Shape::ellipse(
            id,
            Vec2::new(380.0, 250.0),
            Vec2::new(160.0, 160.0),
            Rgba8::hex(0xe6f4ea),
        );
        b.push_with(ORIGIN_INIT, ellipse.into());
    }

    /// 性能確認用に `n` 個の要素を格子状に並べたボードを作る(決定的)。
    pub fn demo(n: usize) -> Self {
        let mut board = Self::local();
        board.elements.reserve(n);
        board.z.reserve(n);
        board.index.reserve(n);
        let cols = (n as f32).sqrt().ceil().max(1.0) as usize;
        let cell = 260.0;
        let mut rng = SplitMix64(0x0005_eedc_01ab_0a2d);
        let palette = [
            Rgba8::hex(0xffffff),
            Rgba8::hex(0xe8f0fe),
            Rgba8::hex(0xfde7e9),
            Rgba8::hex(0xe6f4ea),
        ];
        for i in 0..n {
            let id = board.alloc_id();
            let pos = Vec2::new((i % cols) as f32, (i / cols) as f32) * cell;
            let r = rng.next();
            let shape = match r % 3 {
                0 => Shape::sticky(
                    id,
                    pos,
                    STICKY_COLORS[(r >> 8) as usize % STICKY_COLORS.len()],
                ),
                1 => {
                    let size = Vec2::new(
                        120.0 + (r >> 16) as f32 % 100.0,
                        80.0 + (r >> 24) as f32 % 120.0,
                    );
                    Shape::rect(id, pos, size, palette[(r >> 32) as usize % palette.len()])
                }
                _ => {
                    let d = 100.0 + (r >> 16) as f32 % 100.0;
                    Shape::ellipse(
                        id,
                        pos,
                        Vec2::splat(d),
                        palette[(r >> 32) as usize % palette.len()],
                    )
                }
            };
            board.push(shape);
        }
        board
    }
}

/// 依存を増やさないための小さな擬似乱数。
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_is_deterministic_and_sized() {
        let a = Board::demo(1000);
        let b = Board::demo(1000);
        assert_eq!(a.len(), 1000);
        assert_eq!(a.elements(), b.elements());
        let r = a.bounds().unwrap();
        assert!(r.min.cmpge(Vec2::ZERO).all());
    }

    #[test]
    fn hit_test_picks_topmost() {
        let mut b = Board::new();
        let id1 = b.alloc_id();
        b.push(Shape::rect(
            id1,
            Vec2::ZERO,
            Vec2::splat(100.0),
            Rgba8::WHITE,
        ));
        let id2 = b.alloc_id();
        b.push(Shape::rect(
            id2,
            Vec2::splat(50.0),
            Vec2::splat(100.0),
            Rgba8::WHITE,
        ));
        assert_eq!(b.hit_test(Vec2::splat(75.0), 0.0).unwrap().id(), id2);
        assert_eq!(b.hit_test(Vec2::splat(10.0), 0.0).unwrap().id(), id1);
        assert!(b.hit_test(Vec2::splat(500.0), 0.0).is_none());
    }

    /// 2 つのボード間で、溜まった更新を互いに送り合う。
    fn exchange(a: &mut Board, b: &mut Board) {
        for u in a.take_outgoing() {
            b.apply_remote(&u).unwrap();
        }
        for u in b.take_outgoing() {
            a.apply_remote(&u).unwrap();
        }
    }

    fn sorted_ids(b: &Board) -> Vec<ElementId> {
        b.elements().iter().map(Element::id).collect()
    }

    #[test]
    fn two_boards_converge() {
        let mut a = Board::with_client_id(1);
        let mut b = Board::with_client_id(2);
        let ia = a.alloc_id();
        a.push(Shape::sticky(ia, Vec2::ZERO, Rgba8::WHITE));
        let ib = b.alloc_id();
        b.push(Shape::rect(
            ib,
            Vec2::splat(300.0),
            Vec2::splat(50.0),
            Rgba8::WHITE,
        ));
        assert_ne!(ia, ib);
        exchange(&mut a, &mut b);
        assert_eq!(a.len(), 2);
        assert_eq!(sorted_ids(&a), sorted_ids(&b));
        assert_eq!(a.elements(), b.elements());

        // 同じ要素の別フィールドを同時に変えると、両方残る。
        a.update(ia, |e| e.translate(Vec2::new(10.0, 0.0)));
        b.update(ia, |e| e.set_color(Rgba8::hex(0xff0000)));
        exchange(&mut a, &mut b);
        let s = a.get(ia).unwrap().as_shape().unwrap().clone();
        assert_eq!(s.pos, Vec2::new(10.0, 0.0));
        assert_eq!(s.fill, Rgba8::hex(0xff0000));
        assert_eq!(a.elements(), b.elements());

        // 削除も伝わる。
        b.remove(&[ia]);
        exchange(&mut a, &mut b);
        assert!(a.get(ia).is_none());
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn late_joiner_gets_full_state() {
        let mut a = Board::with_client_id(1);
        a.seed_welcome();
        let mut c = Board::with_client_id(3);
        let diff = a.encode_diff(Some(&c.state_vector())).unwrap();
        c.apply_remote(&diff).unwrap();
        assert_eq!(a.elements(), c.elements());
    }

    #[test]
    fn undo_redo_only_own_changes() {
        let mut a = Board::with_client_id(1);
        let mut b = Board::with_client_id(2);
        a.seed_welcome();
        exchange(&mut a, &mut b);
        // 初期データは Undo の対象外。
        assert!(!a.can_undo());

        let id = sorted_ids(&a)[0];
        // ドラッグ 1 回(複数回の移動)が 1 つの Undo 単位になる。
        for _ in 0..5 {
            a.update(id, |e| e.translate(Vec2::new(10.0, 0.0)));
        }
        a.end_gesture();
        let nid = b.alloc_id();
        b.push(Shape::sticky(nid, Vec2::splat(900.0), Rgba8::WHITE));
        b.end_gesture();
        exchange(&mut a, &mut b);
        assert_eq!(a.get(id).unwrap().bounds().min.x, 50.0);

        assert!(a.undo());
        exchange(&mut a, &mut b);
        assert_eq!(a.get(id).unwrap().bounds().min.x, 0.0);
        assert_eq!(b.get(id).unwrap().bounds().min.x, 0.0);
        // b の追加は a の Undo で消えない。
        assert!(a.get(nid).is_some());

        assert!(a.redo());
        exchange(&mut a, &mut b);
        assert_eq!(b.get(id).unwrap().bounds().min.x, 50.0);
    }

    #[test]
    fn local_board_has_no_sync() {
        let mut b = Board::demo(10);
        assert!(!b.is_collaborative());
        assert!(b.take_outgoing().is_empty());
        assert!(!b.undo());
    }

    #[test]
    fn update_remove_and_index() {
        let mut b = Board::welcome();
        let ids: Vec<_> = b.elements().iter().map(Element::id).collect();
        let r0 = b.revision();
        b.update(ids[1], |e| e.translate(Vec2::new(5.0, 0.0)));
        assert!(b.revision() > r0);
        assert_eq!(b.remove(&[ids[0]]), 1);
        assert!(b.get(ids[0]).is_none());
        // 削除後も ID から引ける。
        assert_eq!(b.get(ids[2]).unwrap().id(), ids[2]);
        let inside = b.query_inside(&Rect::new(Vec2::splat(-1000.0), Vec2::splat(1000.0)));
        assert_eq!(inside.len(), b.len());
    }
}
