//! オフライン編集: 共同編集のボードをこの端末(IndexedDB)に保存し、次に開くときに使う。
//!
//! - 開くとき: サーバーを待たずに前回保存した内容を表示する(サーバーがなくても開ける)
//! - 編集中: 変更があれば数秒おきに保存する。タブを閉じる・隠すときにも保存する
//! - 再接続したとき: サーバーの全体を取り込み、サーバーにない手元の変更だけを送る。CRDT なので
//!   オフライン中の自分の編集と、その間の他の人の編集がマージされる

use std::cell::Cell;
use std::rc::Rc;

use super::{App, now_ms};
use crate::storage;

/// 保存の最短間隔(ミリ秒)。大きなボードでは保存にかかった時間の 10 倍まで延ばす。
const SAVE_INTERVAL_MS: f64 = 2000.0;

/// 端末への保存の状態。
pub(super) struct LocalStore {
    key: String,
    saved_revision: Option<u64>,
    next_save_at: f64,
    /// 保存中か(同時に 2 つ走らせない)。
    busy: Rc<Cell<bool>>,
    /// 保存できたことがあるか(できない環境なら表示を変える)。
    pub(super) ok: Rc<Cell<bool>>,
}

impl LocalStore {
    pub(super) fn new(room: &str) -> Self {
        Self {
            key: storage::room_key(room),
            saved_revision: None,
            next_save_at: 0.0,
            busy: Rc::default(),
            ok: Rc::new(Cell::new(true)),
        }
    }

    pub(super) fn key(&self) -> &str {
        &self.key
    }

    /// 読み込んだ直後の状態を「保存済み」とみなす。
    pub(super) fn mark_saved(&mut self, revision: u64) {
        self.saved_revision = Some(revision);
    }
}

impl App {
    /// 変更があれば端末に保存する。`force` なら間隔を待たない(タブを閉じるときなど)。
    pub(super) fn save_local(&mut self, now: f64, force: bool) {
        let live = self.live_board.as_ref().unwrap_or(&self.board);
        let Some(store) = self.local_store.as_mut() else {
            return;
        };
        let revision = live.revision();
        if store.saved_revision == Some(revision)
            || store.busy.get()
            || (!force && now < store.next_save_at)
            || (live.is_empty() && store.saved_revision.is_none())
        {
            return;
        }
        let started = now_ms(&self.window);
        let Ok(bytes) = live.encode_diff(None) else {
            return;
        };
        let cost = now_ms(&self.window) - started;
        store.saved_revision = Some(revision);
        store.next_save_at = now + SAVE_INTERVAL_MS.max(cost * 10.0);
        store.busy.set(true);
        let (busy, ok, key) = (store.busy.clone(), store.ok.clone(), store.key.clone());
        wasm_bindgen_futures::spawn_local(async move {
            match storage::save(&key, &bytes).await {
                Ok(()) => ok.set(true),
                Err(e) => {
                    log::warn!("この端末に保存できません: {e}");
                    ok.set(false);
                }
            }
            busy.set(false);
        });
    }

    /// 端末に保存できているか(状態表示用)。
    pub(super) fn saves_locally(&self) -> bool {
        self.local_store.as_ref().is_some_and(|s| s.ok.get())
    }
}
