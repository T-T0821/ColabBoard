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
use crate::connector::{Connector, Endpoint};
use crate::crdt;
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind};
use crate::spatial::SpatialIndex;

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

/// 追加・削除・並べ替えがこれより多いときは、1 個ずつではなくまとめて反映する。
const BULK_THRESHOLD: usize = 16;
/// 空間インデックスに登録する範囲の余白(ヒットテストの許容距離などを見込む)。
const INDEX_SLOP: f32 = 8.0;

/// 空間インデックスに登録する範囲。文字(コネクタのラベルなど)がはみ出す分も含める。
fn index_bounds(e: &Element) -> Rect {
    let mut r = e.bounds();
    if !e.text().is_empty()
        && let Some(tb) = e.text_box()
    {
        r = r.union(&tb.clip);
    }
    r.expand(INDEX_SLOP)
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
    /// 位置から要素を引くための空間インデックス。
    spatial: SpatialIndex,
    /// コネクタの ID(経路の計算し直しで全要素を見ないため)。
    connectors: HashSet<ElementId>,
    /// 内容が変わるたびに増える。描画側がバッファ再転送の要否を判断する。
    revision: u64,
    /// 要素 ID の上位 32 ビット(参加者ごとに違う)。
    client: u64,
    counter: u64,
    sync: Option<Sync>,
    /// 新しい付箋に入れる作者名。
    author: String,
    /// マインドマップの要素が変わった(自動レイアウトのやり直しが要る)。
    mind_dirty: bool,
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
            spatial: SpatialIndex::new(),
            connectors: HashSet::new(),
            revision: 0,
            client,
            counter: 0,
            author: String::new(),
            sync: Some(Sync {
                doc,
                map,
                undo,
                changed,
                outbox,
            }),
            mind_dirty: false,
        }
    }

    /// 共同編集しないボード(性能確認用。大量の要素を軽く扱える)。
    pub fn local() -> Self {
        Self {
            elements: Vec::new(),
            z: Vec::new(),
            index: HashMap::new(),
            spatial: SpatialIndex::new(),
            connectors: HashSet::new(),
            revision: 0,
            client: 0,
            counter: 0,
            author: String::new(),
            sync: None,
            mind_dirty: false,
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

    /// 新しく作る付箋に入れる作者名(ログイン中の表示名など)。
    pub fn set_author(&mut self, name: &str) {
        self.author = name.trim().chars().take(40).collect();
    }

    fn push_with(&mut self, origin: &str, mut element: Element) -> ElementId {
        let id = element.id();
        // 新しい付箋には作者を入れる(複製・貼り付けは元の作者のまま。匿名モード中は入れない)。
        if !self.author.is_empty()
            && !self.index.contains_key(&id)
            && let Element::Shape(s) = &mut element
            && s.kind == ShapeKind::Sticky
            && s.meta.author.is_empty()
            && !self.anonymous()
        {
            s.meta.author = self.author.clone();
        }
        let z = match self.index.get(&id) {
            Some(&i) => self.z[i],
            None => self.top_z() + 1.0,
        };
        self.write(origin, vec![(id, Some((element, z)))]);
        id
    }

    /// 要素の追加・置き換えと削除を、Undo の対象にせずに書く(自動レイアウトの結果など)。
    /// 追加する要素の重なり順は、既存ならそのまま、新規なら `z` で指定する(`None` なら最前面)。
    pub fn write_untracked(&mut self, puts: Vec<(Element, Option<f64>)>, removes: &[ElementId]) {
        let mut top = self.top_z();
        let mut writes: Vec<(ElementId, Option<(Element, f64)>)> = puts
            .into_iter()
            .map(|(e, z)| {
                let id = e.id();
                let z = match (self.index.get(&id), z) {
                    (Some(&i), _) => self.z[i],
                    (None, Some(z)) => z,
                    (None, None) => {
                        top += 1.0;
                        top
                    }
                };
                (id, Some((e, z)))
            })
            .collect();
        writes.extend(
            removes
                .iter()
                .filter(|id| self.index.contains_key(id))
                .map(|id| (*id, None)),
        );
        if !writes.is_empty() {
            self.write(ORIGIN_INIT, writes);
        }
    }

    /// マインドマップの要素が前回から変わったか(読むと戻る)。
    pub fn take_mind_dirty(&mut self) -> bool {
        std::mem::take(&mut self.mind_dirty)
    }

    /// 重なり順の値(大きいほど手前)。
    pub fn z_of(&self, id: ElementId) -> Option<f64> {
        self.index.get(&id).map(|&i| self.z[i])
    }

    /// 重なり順の範囲(最背面, 最前面)。
    pub fn z_range(&self) -> (f64, f64) {
        (
            self.z.first().copied().unwrap_or(0.0),
            self.z.last().copied().unwrap_or(0.0),
        )
    }

    /// 重なり順を変える。
    pub fn set_z(&mut self, changes: &[(ElementId, f64)]) {
        let writes: Vec<_> = changes
            .iter()
            .filter_map(|&(id, z)| {
                let &i = self.index.get(&id)?;
                (self.z[i] != z).then(|| (id, Some((self.elements[i].clone(), z))))
            })
            .collect();
        if !writes.is_empty() {
            self.write(ORIGIN_LOCAL, writes);
        }
    }

    /// 複数の要素をまとめて追加・置き換える(z は既存ならそのまま、新規は最前面に順に積む)。
    pub fn push_many(&mut self, elements: Vec<Element>) {
        let mut top = self.top_z();
        let writes = elements
            .into_iter()
            .map(|e| {
                let id = e.id();
                let z = match self.index.get(&id) {
                    Some(&i) => self.z[i],
                    None => {
                        top += 1.0;
                        top
                    }
                };
                (id, Some((e, z)))
            })
            .collect();
        self.write(ORIGIN_LOCAL, writes);
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
            self.apply_view(changes);
            self.refresh_routes();
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
        self.apply_view(updates);
        self.refresh_routes();
    }

    /// まとめて届いた変更をビューに反映する。
    ///
    /// 同じ重なり順のままの変更(移動・色変更など)はその場で置き換える。
    /// 追加・削除・重なり順の変更が多いとき(初回同期や貼り付け)は、1 個ずつ挿入すると
    /// 要素数の 2 乗の時間がかかるので、並べ直しを 1 回のマージで済ませる。
    fn apply_view(&mut self, updates: Vec<(ElementId, Option<(Element, f64)>)>) {
        if !self.mind_dirty {
            self.mind_dirty = updates.iter().any(|(id, change)| {
                change
                    .as_ref()
                    .is_some_and(|(e, _)| e.meta().mind.is_some())
                    || self.get(*id).is_some_and(|e| e.meta().mind.is_some())
            });
        }
        let mut structural = Vec::new();
        for (id, change) in updates {
            match (change, self.index.get(&id).copied()) {
                (Some((e, z)), Some(i)) if self.z[i] == z => self.upsert_view(e, z),
                // 最前面への新規追加は末尾に足すだけなので速い。
                (Some((e, z)), None) if self.z.last().is_none_or(|&last| z > last) => {
                    self.upsert_view(e, z)
                }
                (None, None) => {}
                (change, _) => structural.push((id, change)),
            }
        }
        if structural.len() <= BULK_THRESHOLD {
            for (id, change) in structural {
                match change {
                    Some((e, z)) => self.upsert_view(e, z),
                    None => self.remove_view(id),
                }
            }
            return;
        }
        self.bulk_apply(structural);
    }

    /// 多数の追加・削除・並べ替えを 1 回のマージで反映する。
    fn bulk_apply(&mut self, changes: Vec<(ElementId, Option<(Element, f64)>)>) {
        let affected: HashSet<ElementId> = changes.iter().map(|c| c.0).collect();
        let mut added: Vec<(f64, Element)> = Vec::with_capacity(changes.len());
        for (id, change) in changes {
            match change {
                Some((e, z)) => {
                    if id.0 >> 32 == self.client {
                        self.counter = self.counter.max(id.0 & 0xffff_ffff);
                    }
                    self.spatial.insert(id, index_bounds(&e));
                    self.track_connector(&e);
                    added.push((z, e));
                }
                None => {
                    self.spatial.remove(id);
                    self.connectors.remove(&id);
                }
            }
        }
        added.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.id().cmp(&b.1.id())));

        let old_elements = std::mem::take(&mut self.elements);
        let old_z = std::mem::take(&mut self.z);
        let n = old_elements.len() + added.len();
        self.elements.reserve(n);
        self.z.reserve(n);
        let mut old = old_elements
            .into_iter()
            .zip(old_z)
            .filter(|(e, _)| !affected.contains(&e.id()))
            .peekable();
        let mut new = added.into_iter().peekable();
        loop {
            let take_old = match (old.peek(), new.peek()) {
                (Some((oe, oz)), Some((nz, ne))) => (*oz, oe.id()) < (*nz, ne.id()),
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            let (e, z) = if take_old {
                old.next().unwrap()
            } else {
                let (z, e) = new.next().unwrap();
                (e, z)
            };
            self.elements.push(e);
            self.z.push(z);
        }
        self.index.clear();
        self.index.reserve(self.elements.len());
        for (i, e) in self.elements.iter().enumerate() {
            self.index.insert(e.id(), i);
        }
        self.revision += 1;
    }

    /// コネクタの経路を、つながった相手の現在位置から計算し直す。
    fn refresh_routes(&mut self) {
        let mut changed = false;
        let mut targets: Vec<usize> = self
            .connectors
            .iter()
            .filter_map(|id| self.index.get(id).copied())
            .collect();
        targets.sort_unstable();
        for i in targets {
            let Element::Connector(c) = &self.elements[i] else {
                continue;
            };
            let mut c = c.clone();
            let elements = &self.elements;
            let index = &self.index;
            let lookup = |id: ElementId| {
                index
                    .get(&id)
                    .map(|&j| &elements[j])
                    .and_then(crate::connector::Target::of)
            };
            if c.route(lookup) {
                let e = Element::Connector(c);
                self.spatial.insert(e.id(), index_bounds(&e));
                self.elements[i] = e;
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
        }
    }

    fn upsert_view(&mut self, e: Element, z: f64) {
        let id = e.id();
        // 他の参加者の ID と衝突しないよう、自分の払い出し番号を進めておく。
        if id.0 >> 32 == self.client {
            self.counter = self.counter.max(id.0 & 0xffff_ffff);
        }
        self.revision += 1;
        self.spatial.insert(id, index_bounds(&e));
        self.track_connector(&e);
        if let Some(&i) = self.index.get(&id) {
            if self.z[i] == z {
                self.elements[i] = e;
                return;
            }
            self.remove_at(i);
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
        if let Some(&i) = self.index.get(&id) {
            self.spatial.remove(id);
            self.connectors.remove(&id);
            self.remove_at(i);
            self.revision += 1;
        }
    }

    fn track_connector(&mut self, e: &Element) {
        if matches!(e, Element::Connector(_)) {
            self.connectors.insert(e.id());
        } else {
            self.connectors.remove(&e.id());
        }
    }

    /// ビューの `i` 番目を外す(空間インデックスはそのまま)。
    fn remove_at(&mut self, i: usize) {
        let e = self.elements.remove(i);
        self.index.remove(&e.id());
        self.z.remove(i);
        self.reindex_from(i);
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

    /// 相手から届いたボード全体(`update`)に含まれていない、手元だけの変更を作る。
    ///
    /// 再接続やオフライン編集の後、手元の全体ではなく相手が持っていない分だけを送るのに使う。
    /// 削除の情報は常に全部入る(yrs の仕様)。
    pub fn encode_missing_from(&self, update: &[u8]) -> Result<Vec<u8>, String> {
        let sync = self.sync.as_ref().ok_or("共同編集できないボードです")?;
        let theirs = Update::decode_v1(update).map_err(|e| e.to_string())?;
        Ok(sync
            .doc
            .transact()
            .encode_state_as_update_v1(&theirs.state_vector()))
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
        // 登録範囲には `INDEX_SLOP` の余白があるので、それを超える許容距離だけ広げて探す。
        let probe = Rect::new(world, world).expand((tolerance - INDEX_SLOP).max(0.0));
        let mut best: Option<usize> = None;
        let indexed = self.spatial.query(&probe, |id| {
            if let Some(&i) = self.index.get(&id)
                && best.is_none_or(|b| i > b)
                && self.elements[i].hit(world, tolerance)
            {
                best = Some(i);
            }
        });
        if !indexed {
            return self.elements.iter().rev().find(|e| e.hit(world, tolerance));
        }
        best.map(|i| &self.elements[i])
    }

    /// `r` と重なるかもしれない要素の位置(描画順・昇順・重複なし)。
    ///
    /// 外接矩形に加えて文字がはみ出す分(コネクタのラベルなど)も含めて広めに返すので、
    /// 呼び出し側で正確に判定する。範囲が広すぎて全要素を見るほうが速いときは `None`。
    pub fn query_candidates(&self, r: &Rect) -> Option<Vec<usize>> {
        let mut out = Vec::new();
        if !self.spatial.query(r, |id| {
            if let Some(&i) = self.index.get(&id) {
                out.push(i);
            }
        }) {
            return None;
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    }

    /// 矩形に完全に含まれる要素(範囲選択用)。描画順。
    pub fn query_inside(&self, r: &Rect) -> Vec<ElementId> {
        let inside = |e: &&Element| !e.meta().hidden && r.contains_rect(&e.bounds());
        match self.query_candidates(r) {
            Some(c) => c
                .into_iter()
                .map(|i| &self.elements[i])
                .filter(inside)
                .map(Element::id)
                .collect(),
            None => self
                .elements
                .iter()
                .filter(inside)
                .map(Element::id)
                .collect(),
        }
    }

    /// 矩形と重なる要素(外接矩形で判定)。描画順(奥から手前)。
    pub fn query_visible(&self, r: &Rect) -> Vec<&Element> {
        let hit = |e: &&Element| e.bounds().intersects(r);
        match self.query_candidates(r) {
            Some(c) => c
                .into_iter()
                .map(|i| &self.elements[i])
                .filter(hit)
                .collect(),
            None => self.elements.iter().filter(hit).collect(),
        }
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
            "ようこそ グラボード へ!\nダブルクリックで編集できます",
            "付箋・図形・ペンは下のツールバーから",
            "Ctrl/⌘ + スクロールでズーム",
        ];
        for ((pos, c), text) in notes.into_iter().zip(texts) {
            let id = b.alloc_id();
            let mut note = Shape::sticky(id, pos, c);
            note.text = text.to_string();
            b.push_with(ORIGIN_INIT, note.into());
        }
        // フレームの中に、矢印でつないだ小さな流れ図を置く。
        let id = b.alloc_id();
        let mut frame = Shape::frame(id, Vec2::new(0.0, 280.0), Vec2::new(680.0, 220.0));
        frame.text = "はじめてのフロー".into();
        b.push_with(ORIGIN_INIT, frame.into());
        let steps = [
            (
                ShapeKind::Rect,
                Vec2::new(30.0, 330.0),
                0xe8f0fe,
                "アイデア",
            ),
            (
                ShapeKind::Diamond,
                Vec2::new(250.0, 320.0),
                0xfff3a3,
                "検討",
            ),
            (
                ShapeKind::Ellipse,
                Vec2::new(480.0, 330.0),
                0xe6f4ea,
                "実行",
            ),
        ];
        let mut prev: Option<ElementId> = None;
        for (kind, pos, fill, text) in steps {
            let id = b.alloc_id();
            let size = if kind == ShapeKind::Diamond {
                Vec2::new(180.0, 140.0)
            } else {
                Vec2::new(170.0, 120.0)
            };
            let mut s = Shape::new(id, kind, pos, size, Rgba8::hex(fill));
            s.text = text.into();
            b.push_with(ORIGIN_INIT, s.into());
            if let Some(from) = prev {
                let cid = b.alloc_id();
                let mut c = Connector::new(
                    cid,
                    Endpoint::Attached {
                        id: from,
                        anchor: None,
                    },
                    Endpoint::Attached { id, anchor: None },
                );
                if kind == ShapeKind::Ellipse {
                    c.text = "OK".into();
                }
                b.push_with(ORIGIN_INIT, c.into());
            }
            prev = Some(id);
        }
    }

    /// 性能確認用に `n` 個の要素を格子状に並べたボードを作る(決定的)。
    pub fn demo(n: usize) -> Self {
        let mut board = Self::local();
        board.elements.reserve(n);
        board.z.reserve(n);
        board.index.reserve(n);
        board.spatial.reserve(n);
        board.fill_demo(n, ORIGIN_LOCAL);
        board
    }

    /// `demo` と同じ要素を持つ共同編集用ボード(ベンチマーク・初回同期の計測用)。
    /// 全要素を 1 つのトランザクションで書き込み、Undo の対象にはしない。
    pub fn demo_shared(n: usize, client_id: u64) -> Self {
        let mut board = Self::with_client_id(client_id);
        board.fill_demo(n, ORIGIN_INIT);
        board
    }

    fn fill_demo(&mut self, n: usize, origin: &str) {
        let base = self.top_z();
        let mut changes = Vec::with_capacity(n);
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
            let id = self.alloc_id();
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
                    let kind = ShapeKind::LIBRARY[(r >> 40) as usize % ShapeKind::LIBRARY.len()];
                    Shape::new(
                        id,
                        kind,
                        pos,
                        size,
                        palette[(r >> 32) as usize % palette.len()],
                    )
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
            changes.push((id, Some((Element::from(shape), base + 1.0 + i as f64))));
        }
        self.write(origin, changes);
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
    fn new_stickies_get_author_and_sync_it() {
        let mut a = Board::new();
        a.set_author("  つのもん ");
        let id = a.alloc_id();
        a.push(Shape::sticky(id, Vec2::ZERO, Rgba8::WHITE));
        let rect = a.alloc_id();
        a.push(Shape::rect(rect, Vec2::ZERO, Vec2::ONE, Rgba8::WHITE));
        assert_eq!(a.get(id).unwrap().meta().author, "つのもん");
        // 付箋以外には入れない。
        assert!(a.get(rect).unwrap().meta().author.is_empty());

        // 他の人が書き換えても作者は変わらない。
        let mut b = Board::new();
        b.apply_remote(&a.encode_diff(None).unwrap()).unwrap();
        b.set_author("はな");
        b.update(id, |e| {
            if let Element::Shape(s) = e {
                s.text = "追記".into();
            }
        });
        assert_eq!(b.get(id).unwrap().meta().author, "つのもん");
        let s = b.get(id).unwrap().as_shape().unwrap();
        let tb = s.author_box().unwrap();
        assert!(tb.rect.max.y <= s.bounds().max.y && tb.rect.min.y > s.bounds().min.y);
    }
    use crate::connector::PathStyle;

    #[test]
    fn connectors_follow_and_sync() {
        let mut a = Board::with_client_id(1);
        let s1 = a.alloc_id();
        a.push(Shape::rect(
            s1,
            Vec2::ZERO,
            Vec2::splat(100.0),
            Rgba8::WHITE,
        ));
        let s2 = a.alloc_id();
        a.push(Shape::new(
            s2,
            ShapeKind::Star,
            Vec2::new(300.0, 0.0),
            Vec2::splat(100.0),
            Rgba8::WHITE,
        ));
        let cid = a.alloc_id();
        let mut c = Connector::new(
            cid,
            Endpoint::Attached {
                id: s1,
                anchor: None,
            },
            Endpoint::Attached {
                id: s2,
                anchor: None,
            },
        );
        c.style = PathStyle::Straight;
        c.start_cap = crate::Cap::DiamondFilled;
        c.end_cap = crate::Cap::ZeroOrMany;
        c.dashed = true;
        a.push(c);
        let path = |b: &Board| match b.get(cid) {
            Some(Element::Connector(c)) => c.path.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            path(&a),
            vec![Vec2::new(100.0, 50.0), Vec2::new(300.0, 50.0)]
        );

        // 相手を動かすと追従する。
        a.update(s2, |e| e.translate(Vec2::new(100.0, 0.0)));
        assert_eq!(path(&a)[1], Vec2::new(400.0, 50.0));

        // 別の参加者にも同じ形で届く(経路は受け手が計算する)。
        let mut b = Board::with_client_id(2);
        b.apply_remote(&a.encode_diff(None).unwrap()).unwrap();
        assert_eq!(path(&b), path(&a));
        assert!(matches!(b.get(s2), Some(Element::Shape(s)) if s.kind == ShapeKind::Star));
        // UML・ER の記号と破線も届く。
        assert!(matches!(b.get(cid), Some(Element::Connector(c))
            if c.dashed && c.start_cap == crate::Cap::DiamondFilled
                && c.end_cap == crate::Cap::ZeroOrMany));

        // 経路が変わったコネクタも、新しい位置でヒットテストに当たる。
        assert_eq!(
            a.hit_test(Vec2::new(350.0, 50.0), 2.0).map(Element::id),
            Some(cid)
        );
    }

    /// 空間インデックスを使った検索が、全要素を見る素朴な方法と同じ結果になる。
    #[test]
    fn spatial_queries_match_linear_scan() {
        use crate::element::Stroke;
        let mut b = Board::demo_shared(2000, 7);
        // 移動・削除・手書き線・コネクタ・巨大な要素・ラベル付きコネクタを混ぜる。
        let ids: Vec<_> = b.elements().iter().map(Element::id).collect();
        for (k, id) in ids.iter().enumerate().step_by(7) {
            b.update(*id, |e| {
                e.translate(Vec2::new(k as f32 * 0.37, -(k as f32) * 0.11))
            });
        }
        b.remove(&ids[..ids.len() / 10]);
        let sid = b.alloc_id();
        let mut st = Stroke::new(sid, Vec2::new(100.0, 100.0), Rgba8::BLACK, 6.0);
        for i in 0..50 {
            st.push_point(
                Vec2::new(100.0 + i as f32 * 40.0, 100.0 + (i % 5) as f32 * 30.0),
                0.0,
            );
        }
        b.push(st);
        let cid = b.alloc_id();
        let mut c = Connector::new(
            cid,
            Endpoint::Attached {
                id: ids[500],
                anchor: None,
            },
            Endpoint::Attached {
                id: ids[1500],
                anchor: None,
            },
        );
        c.text = "とても長いラベルの文字がはみ出す".repeat(4);
        b.push(c);
        b.update(ids[1500], |e| e.translate(Vec2::new(-700.0, 900.0)));
        let big = b.alloc_id();
        b.push(Shape::rect(
            big,
            Vec2::splat(3000.0),
            Vec2::splat(30000.0),
            Rgba8::WHITE,
        ));
        // 重なり順の変更(並べ替え)。
        let z = b.z_range().0 - 1.0;
        let back: Vec<_> = ids[1000..1040].iter().map(|id| (*id, z)).collect();
        b.set_z(&back);

        let mut rng = SplitMix64(42);
        let extent = b.bounds().unwrap();
        let mut rand_point = || {
            let r = rng.next();
            let t = Vec2::new(
                (r & 0xffff) as f32 / 65535.0,
                (r >> 16 & 0xffff) as f32 / 65535.0,
            );
            extent.min + (extent.max - extent.min) * t
        };
        for k in 0..600 {
            let p = rand_point();
            let tol = [0.0, 4.0, 20.0][k % 3];
            let linear = b
                .elements()
                .iter()
                .rev()
                .find(|e| e.hit(p, tol))
                .map(Element::id);
            assert_eq!(b.hit_test(p, tol).map(Element::id), linear, "hit {p:?}");

            let r = Rect::from_points(p, rand_point()).expand(10.0);
            let linear: Vec<_> = b
                .elements()
                .iter()
                .filter(|e| r.contains_rect(&e.bounds()))
                .map(Element::id)
                .collect();
            assert_eq!(b.query_inside(&r), linear);
            let small = Rect::from_points(p, p + Vec2::new(900.0, 600.0));
            let linear: Vec<_> = b
                .elements()
                .iter()
                .filter(|e| e.bounds().intersects(&small))
                .map(Element::id)
                .collect();
            let got: Vec<_> = b
                .query_visible(&small)
                .into_iter()
                .map(Element::id)
                .collect();
            assert_eq!(got, linear);
            // 文字の領域が重なる要素も漏れない。
            for (i, e) in b.elements().iter().enumerate() {
                if let Some(tb) = e.text_box()
                    && !e.text().is_empty()
                    && tb.clip.intersects(&small)
                {
                    assert!(b.query_candidates(&small).unwrap().contains(&i));
                }
            }
        }
    }

    /// 大量の要素をまとめて取り込んでも、1 個ずつ取り込んだときと同じ並びになる。
    #[test]
    fn bulk_reconcile_matches_incremental() {
        let src = Board::demo_shared(3000, 9);
        let full = src.encode_diff(None).unwrap();
        let mut a = Board::with_client_id(1);
        a.apply_remote(&full).unwrap();
        assert_eq!(a.elements(), src.elements());
        // 既存のボードに後から大量に混ざる場合も、並びと ID の引き当てが正しい。
        let mut b = Board::with_client_id(2);
        b.seed_welcome();
        b.apply_remote(&full).unwrap();
        let mut zs: Vec<_> = b
            .elements()
            .iter()
            .map(|e| b.z_of(e.id()).unwrap())
            .collect();
        let sorted = {
            let mut v = zs.clone();
            v.sort_by(f64::total_cmp);
            v
        };
        assert_eq!(zs, sorted);
        for (i, e) in b.elements().iter().enumerate() {
            assert_eq!(b.get(e.id()).map(Element::id), Some(e.id()), "{i}");
        }
        zs.clear();
        assert_eq!(b.len(), src.len() + Board::welcome().len());
    }

    #[test]
    fn meta_roundtrips() {
        let mut a = Board::with_client_id(1);
        let id = a.alloc_id();
        let mut s = Shape::text(id, Vec2::ZERO, Vec2::new(200.0, 40.0));
        s.text = "見出し".into();
        s.font_size = 40.0;
        s.meta.group = 77;
        s.meta.locked = true;
        s.meta.link = "https://example.com".into();
        s.icon = "group:vpc".into();
        a.push(s.clone());
        let mut b = Board::with_client_id(2);
        b.apply_remote(&a.encode_diff(None).unwrap()).unwrap();
        assert_eq!(b.get(id), Some(&Element::Shape(s)));
    }

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

    /// オフライン中の変更だけを送れば、サーバー側と収束する。
    #[test]
    fn missing_changes_only() {
        let mut server = Board::demo_shared(500, 1);
        let snapshot = server.encode_diff(None).unwrap();
        let mut client = Board::with_client_id(2);
        client.apply_remote(&snapshot).unwrap();
        // オフラインで 1 個追加・1 個移動。サーバー側でも別の変更。
        let id = client.alloc_id();
        client.push(Shape::sticky(id, Vec2::splat(-500.0), Rgba8::WHITE));
        let moved = client.elements()[3].id();
        client.update(moved, |e| e.translate(Vec2::new(7.0, 0.0)));
        let sid = server.alloc_id();
        server.push(Shape::rect(
            sid,
            Vec2::ZERO,
            Vec2::splat(10.0),
            Rgba8::WHITE,
        ));
        let _ = client.take_outgoing();
        let _ = server.take_outgoing();

        let now = server.encode_diff(None).unwrap();
        client.apply_remote(&now).unwrap();
        let missing = client.encode_missing_from(&now).unwrap();
        assert!(
            missing.len() < now.len() / 10,
            "{} / {}",
            missing.len(),
            now.len()
        );
        server.apply_remote(&missing).unwrap();
        assert_eq!(server.elements(), client.elements());
        assert!(server.get(id).is_some() && client.get(sid).is_some());
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
