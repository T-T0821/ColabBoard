//! ボードの定期バックアップと復元。
//!
//! バージョン履歴(ボードごとの過去の状態)とは別に、サーバーの全データ(redb の全テーブルと、
//! チームの保存場所に置いたボードの中身)を 1 つのファイルにまとめて、別の場所に残す。
//!
//! - 定期: `BACKUP_INTERVAL_HOURS`(既定 24)ごと。`0` で止める(手動では取れる)。
//! - 残す数: `BACKUP_KEEP`(既定 14)。定期と手動でそれぞれ数える。
//! - 置き場所: `BACKUP_LOCATION`。書き方は [`crate::storage`] の保存場所と同じ
//!   (`file:///mnt/backup` か `s3://バケット/接頭辞?endpoint=…&region=…`)。認証情報は
//!   `BACKUP_ACCESS_KEY_ID` / `BACKUP_SECRET_ACCESS_KEY`(なければ `AWS_…`)。
//!   Google Cloud Storage は HMAC キーで `endpoint=https://storage.googleapis.com&region=auto`。
//!   指定がなければ `DATA_DIR/backups`(同じディスクなので、ディスクの故障には別の場所が要る)。
//!
//! 戻し方:
//! - ボードを 1 つ: 運用の画面(`POST /api/admin/backups/{id}/boards/{board}/restore`)。
//!   今もあるボードは中身だけを戻し(戻す前の状態はバージョン履歴に残る)、削除されたボードは
//!   コメント・バージョン履歴・動画メモ・メンバーごと作り直す。
//! - 全体: サーバーを止めて `board-server restore <ファイルか ID> --yes`(今のデータは脇に残す)。
//!
//! ファイルの形式(`.grabak.gz`、gzip の中身):
//!
//! ```text
//! "GRABOARD-BACKUP\n"  u32 ヘッダーの長さ  ヘッダー(JSON)
//! 記録の並び: u8 種類
//!   1 = テーブル: 文字列 テーブル名, u8 キーの形, キー, バイト列 値
//!   2 = 保存場所: 文字列 保存場所 ID, 文字列 キー, バイト列 値
//!   0 = 終わり
//! 文字列 = u32 長さ + UTF-8、バイト列 = u64 長さ + 中身(数はすべてリトルエンディアン)
//! ```

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use board_core::api::ops::{BackupBoard, BackupInfo, BackupStatus, RestoreResult};
use redb::{ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition, TableHandle};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::Error;
use crate::audit::{self, Entry as AuditEntry};
use crate::auth::{UserRecord, now_ms};
use crate::boards::{self, BoardMeta};
use crate::api::require_admin;
use crate::room::Hub;
use crate::storage::{self, Blob, DirBlob};
use crate::store::{
    COMMENTS, MEDIA, MEDIA_INFO, Store, StoreError, TEAMS, USER_BOARDS, VERSION_INFO,
};

const MAGIC: &[u8; 16] = b"GRABOARD-BACKUP\n";
const FORMAT: u32 = 1;
const INDEX_KEY: &str = "backups/index.json";
pub const DEFAULT_INTERVAL_HOURS: u64 = 24;
pub const DEFAULT_KEEP: usize = 14;

fn backup_key(id: u64) -> String {
    format!("backups/{id}.grabak.gz")
}

// ---- 形式 ----

/// テーブルのキーの形。
const KEY_STR: u8 = 1;
const KEY_STR_U64: u8 = 2;
const KEY_STR_STR: u8 = 3;
const KEY_U64: u8 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Str(String),
    StrU64(String, u64),
    StrStr(String, String),
    U64(u64),
}

impl Key {
    /// 1 つ目の文字列(ボード ID など)。
    fn first(&self) -> Option<&str> {
        match self {
            Key::Str(a) | Key::StrU64(a, _) | Key::StrStr(a, _) => Some(a),
            Key::U64(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Table {
        table: String,
        key: Key,
        value: Vec<u8>,
    },
    Blob {
        location: String,
        key: String,
        value: Vec<u8>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub format: u32,
    pub created: u64,
    pub release: String,
    pub tables: Vec<String>,
    /// キーの形がわからず入れられなかったテーブル。
    #[serde(default)]
    pub skipped: Vec<String>,
}

/// 読み込んだバックアップ。
#[derive(Clone, Debug, Default)]
pub struct BackupData {
    pub header: Header,
    pub records: Vec<Record>,
}

impl BackupData {
    fn table<'a>(&'a self, name: &'a str) -> impl Iterator<Item = (&'a Key, &'a [u8])> + 'a {
        self.records.iter().filter_map(move |r| match r {
            Record::Table { table, key, value } if table == name => Some((key, value.as_slice())),
            _ => None,
        })
    }

    fn blob(&self, key: &str) -> Option<&[u8]> {
        self.records.iter().find_map(|r| match r {
            Record::Blob { key: k, value, .. } if k == key => Some(value.as_slice()),
            _ => None,
        })
    }

    /// ボードの情報(削除済みも含む)。
    pub fn boards(&self) -> Vec<BoardMeta> {
        self.table("boards")
            .filter_map(|(_, v)| serde_json::from_slice(v).ok())
            .collect()
    }

    pub fn board(&self, id: &str) -> Option<BoardMeta> {
        self.table("boards")
            .find(|(k, _)| matches!(k, Key::Str(s) if s == id))
            .and_then(|(_, v)| serde_json::from_slice(v).ok())
    }

    /// ボードの中身(保存場所に置いていたものを含む)。
    pub fn board_state(&self, id: &str) -> Option<&[u8]> {
        self.blob(&storage::board_key(id)).or_else(|| {
            self.table("rooms")
                .find(|(k, _)| matches!(k, Key::Str(s) if s == id))
                .map(|(_, v)| v)
        })
    }

    /// 1 つ目のキーがボード ID のもの(コメント・バージョンなど)。
    fn of_board<'a>(
        &'a self,
        table: &'a str,
        id: &'a str,
    ) -> impl Iterator<Item = (&'a Key, &'a [u8])> + 'a {
        self.table(table)
            .filter(move |(k, _)| k.first() == Some(id))
    }

    fn version_state(&self, id: &str, vid: u64) -> Option<&[u8]> {
        self.blob(&storage::version_key(id, vid)).or_else(|| {
            self.table("versions")
                .find(|(k, _)| matches!(k, Key::StrU64(s, v) if s == id && *v == vid))
                .map(|(_, v)| v)
        })
    }
}

struct Writer<W: Write> {
    w: W,
}

impl<W: Write> Writer<W> {
    fn str(&mut self, s: &str) -> std::io::Result<()> {
        self.w.write_all(&(s.len() as u32).to_le_bytes())?;
        self.w.write_all(s.as_bytes())
    }
    fn bytes(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.w.write_all(&(b.len() as u64).to_le_bytes())?;
        self.w.write_all(b)
    }
    fn table(&mut self, table: &str, key: &Key, value: &[u8]) -> std::io::Result<()> {
        self.w.write_all(&[1])?;
        self.str(table)?;
        match key {
            Key::Str(a) => {
                self.w.write_all(&[KEY_STR])?;
                self.str(a)?;
            }
            Key::StrU64(a, n) => {
                self.w.write_all(&[KEY_STR_U64])?;
                self.str(a)?;
                self.w.write_all(&n.to_le_bytes())?;
            }
            Key::StrStr(a, b) => {
                self.w.write_all(&[KEY_STR_STR])?;
                self.str(a)?;
                self.str(b)?;
            }
            Key::U64(n) => {
                self.w.write_all(&[KEY_U64])?;
                self.w.write_all(&n.to_le_bytes())?;
            }
        }
        self.bytes(value)
    }
    fn blob(&mut self, location: &str, key: &str, value: &[u8]) -> std::io::Result<()> {
        self.w.write_all(&[2])?;
        self.str(location)?;
        self.str(key)?;
        self.bytes(value)
    }
}

/// 中身の数。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DumpStats {
    pub boards: usize,
    pub users: usize,
    pub records: usize,
    pub skipped: Vec<String>,
}

/// 全データを 1 つのファイル(gzip)にまとめる。
pub fn dump(store: &Store) -> Result<(Vec<u8>, DumpStats), StoreError> {
    let txn = store.database().begin_read()?;
    let mut tables: Vec<String> = txn.list_tables()?.map(|h| h.name().to_string()).collect();
    tables.sort();
    let mut stats = DumpStats::default();
    let mut body = Writer {
        w: flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()),
    };
    // ヘッダーは最後にわかる「入れられなかったテーブル」を含むので、記録を先にメモリへ書く。
    let mut records = Writer { w: Vec::new() };
    for name in &tables {
        let n = dump_table(&txn, name, &mut records)?;
        match n {
            Some(n) => {
                stats.records += n;
                if name == "users" {
                    stats.users = n;
                }
            }
            None => {
                tracing::warn!(table = %name, "キーの形がわからないので、バックアップに入れません");
                stats.skipped.push(name.clone());
            }
        }
    }
    drop(txn);
    // チームの保存場所に置いたボードの中身とバージョン。
    for meta in store.scan_all_json::<BoardMeta>(crate::store::BOARDS)? {
        if !meta.deleted {
            stats.boards += 1;
        }
        let Some(loc) = store.location_of(&meta.id).ok().flatten() else {
            continue;
        };
        let key = storage::board_key(&meta.id);
        if let Some(b) = loc.blob.get(&key)? {
            records.blob(&loc.id, &key, &b)?;
            stats.records += 1;
        }
        for vid in store.seq_ids(VERSION_INFO, &meta.id)? {
            let key = storage::version_key(&meta.id, vid);
            if let Some(b) = loc.blob.get(&key)? {
                records.blob(&loc.id, &key, &b)?;
                stats.records += 1;
            }
        }
    }
    records.w.push(0);
    let header = Header {
        format: FORMAT,
        created: now_ms(),
        release: crate::logging::RELEASE.into(),
        tables,
        skipped: stats.skipped.clone(),
    };
    let header = serde_json::to_vec(&header)?;
    body.w.write_all(MAGIC)?;
    body.w.write_all(&(header.len() as u32).to_le_bytes())?;
    body.w.write_all(&header)?;
    body.w.write_all(&records.w)?;
    Ok((body.w.finish()?, stats))
}

/// 1 つのテーブルを書く。キーの形がわからなければ `None`。
fn dump_table<W: Write>(
    txn: &ReadTransaction,
    name: &str,
    out: &mut Writer<W>,
) -> Result<Option<usize>, StoreError> {
    macro_rules! try_kind {
        ($k:ty, |$v:ident| $key:expr) => {{
            let def: TableDefinition<$k, &[u8]> = TableDefinition::new(name);
            match txn.open_table(def) {
                Ok(t) => {
                    let mut n = 0;
                    for e in t.iter()? {
                        let (k, v) = e?;
                        let $v = k.value();
                        out.table(name, &$key, v.value())?;
                        n += 1;
                    }
                    return Ok(Some(n));
                }
                Err(redb::TableError::TableTypeMismatch { .. })
                | Err(redb::TableError::TableIsMultimap(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }};
    }
    try_kind!(&str, |k| Key::Str(k.to_string()));
    try_kind!((&str, u64), |k| Key::StrU64(k.0.to_string(), k.1));
    try_kind!((&str, &str), |k| Key::StrStr(
        k.0.to_string(),
        k.1.to_string()
    ));
    try_kind!(u64, |k| Key::U64(k));
    Ok(None)
}

/// gzip のバックアップを読む。
pub fn parse(gz: &[u8]) -> Result<BackupData, StoreError> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_end(&mut raw)
        .map_err(|e| format!("バックアップを展開できません: {e}"))?;
    let mut r = Reader { b: &raw, at: 0 };
    if r.take(MAGIC.len())? != MAGIC {
        return Err("グラボードのバックアップではありません".into());
    }
    let hlen = r.u32()? as usize;
    let header: Header = serde_json::from_slice(r.take(hlen)?)?;
    if header.format > FORMAT {
        return Err(format!(
            "新しい形式({})のバックアップです。サーバーを更新してください",
            header.format
        )
        .into());
    }
    let mut records = Vec::new();
    loop {
        match r.u8()? {
            0 => break,
            1 => {
                let table = r.str()?;
                let key = match r.u8()? {
                    KEY_STR => Key::Str(r.str()?),
                    KEY_STR_U64 => Key::StrU64(r.str()?, r.u64()?),
                    KEY_STR_STR => Key::StrStr(r.str()?, r.str()?),
                    KEY_U64 => Key::U64(r.u64()?),
                    k => return Err(format!("知らないキーの形です({k})").into()),
                };
                let value = r.bytes()?;
                records.push(Record::Table { table, key, value });
            }
            2 => {
                let location = r.str()?;
                let key = r.str()?;
                let value = r.bytes()?;
                records.push(Record::Blob {
                    location,
                    key,
                    value,
                });
            }
            t => return Err(format!("壊れたバックアップです(記録の種類 {t})").into()),
        }
    }
    Ok(BackupData { header, records })
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], StoreError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or("バックアップが途中で切れています")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, StoreError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, StoreError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64, StoreError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, StoreError> {
        let n = usize::try_from(self.u64()?)?;
        Ok(self.take(n)?.to_vec())
    }
    fn str(&mut self) -> Result<String, StoreError> {
        let n = self.u32()? as usize;
        Ok(String::from_utf8(self.take(n)?.to_vec())?)
    }
}

/// 全体の復元の結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreAllStats {
    pub records: usize,
    /// 保存場所が設定されていないので、このサーバーに置いた中身の数。
    pub blobs_kept_local: usize,
}

/// バックアップを(空の)ストアに丸ごと書き込む。チームの保存場所に置いていた中身は、
/// その保存場所が設定されていればそこへ、なければこのサーバーに戻す。
pub fn restore_all(store: &Store, data: &BackupData) -> Result<RestoreAllStats, StoreError> {
    let mut stats = RestoreAllStats::default();
    let txn = store.database().begin_write()?;
    for r in &data.records {
        let Record::Table { table, key, value } = r else {
            continue;
        };
        let name = table.as_str();
        let v = value.as_slice();
        match key {
            Key::Str(a) => {
                let mut t = txn.open_table(TableDefinition::<&str, &[u8]>::new(name))?;
                t.insert(a.as_str(), v)?;
            }
            Key::StrU64(a, n) => {
                let mut t = txn.open_table(TableDefinition::<(&str, u64), &[u8]>::new(name))?;
                t.insert((a.as_str(), *n), v)?;
            }
            Key::StrStr(a, b) => {
                let mut t = txn.open_table(TableDefinition::<(&str, &str), &[u8]>::new(name))?;
                t.insert((a.as_str(), b.as_str()), v)?;
            }
            Key::U64(n) => {
                let mut t = txn.open_table(TableDefinition::<u64, &[u8]>::new(name))?;
                t.insert(*n, v)?;
            }
        }
        stats.records += 1;
    }
    txn.commit()?;
    for r in &data.records {
        let Record::Blob {
            location,
            key,
            value,
        } = r
        else {
            continue;
        };
        stats.records += 1;
        if let Some(loc) = store.location(location) {
            loc.blob.put(key, value)?;
            continue;
        }
        stats.blobs_kept_local += 1;
        tracing::warn!(location = %location, key = %key, "保存場所が設定されていないので、このサーバーに戻します");
        put_local_blob(store, key, value)?;
    }
    Ok(stats)
}

/// 保存場所のキーを、このサーバーのテーブルに置き直す。
fn put_local_blob(store: &Store, key: &str, value: &[u8]) -> Result<(), StoreError> {
    let rest = key
        .strip_prefix("boards/")
        .ok_or_else(|| format!("知らないキーです: {key}"))?;
    if let Some(id) = rest.strip_suffix(".ybin")
        && !id.contains('/')
    {
        return store.save_at(None, id, value);
    }
    if let Some((id, v)) = rest.split_once("/versions/")
        && let Some(vid) = v.strip_suffix(".ybin").and_then(|v| v.parse().ok())
    {
        return store.version_put_at(None, id, vid, value);
    }
    Err(format!("知らないキーです: {key}").into())
}

// ---- 1 つのボードを戻す ----

/// 今のサーバーでのボードの状態。
fn current_state(store: &Store, id: &str) -> Result<&'static str, StoreError> {
    Ok(match boards::get(store, id)? {
        Some(m) if !m.deleted => "exists",
        Some(_) => "deleted",
        None => "missing",
    })
}

/// バックアップに入っているボード(削除済みは除く)。
pub fn list_boards(store: &Store, data: &BackupData) -> Result<Vec<BackupBoard>, StoreError> {
    let mut out = Vec::new();
    for m in data.boards() {
        if m.deleted {
            continue;
        }
        out.push(BackupBoard {
            bytes: data.board_state(&m.id).map_or(0, <[u8]>::len),
            comments: data.of_board("comments", &m.id).count(),
            versions: data.of_board("version_info", &m.id).count(),
            current: current_state(store, &m.id)?.into(),
            id: m.id,
            title: m.title,
            owner: m.owner,
            team: m.team,
            updated: m.updated,
        });
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated).then(a.id.cmp(&b.id)));
    Ok(out)
}

#[derive(Debug)]
pub enum RestoreError {
    NotFound,
    Failed(String),
    Store(StoreError),
}

impl From<StoreError> for RestoreError {
    fn from(e: StoreError) -> Self {
        RestoreError::Store(e)
    }
}

impl From<RestoreError> for Error {
    fn from(e: RestoreError) -> Self {
        match e {
            RestoreError::NotFound => Error::new(
                StatusCode::NOT_FOUND,
                "このバックアップにそのボードは入っていません",
            ),
            RestoreError::Failed(m) => Error::bad_request(m),
            RestoreError::Store(e) => e.into(),
        }
    }
}

/// ボードを 1 つ戻す。今もあれば中身を戻し(戻す前の状態をバージョンに残す)、
/// 削除されていれば作り直す。
pub fn restore_board(
    hub: &Hub,
    data: &BackupData,
    id: &str,
    actor: Option<&UserRecord>,
) -> Result<RestoreResult, RestoreError> {
    let store = hub.store();
    let meta = data
        .board(id)
        .filter(|m| !m.deleted)
        .ok_or(RestoreError::NotFound)?;
    let state = data.board_state(id).unwrap_or(&[]).to_vec();
    let when = crate::logging::rfc3339(data.header.created + 9 * 3600 * 1000);
    let when = when.get(..16).unwrap_or(&when).replace('T', " ");
    match boards::get(store, id)? {
        Some(cur) if !cur.deleted => {
            // 中身だけ戻す。今の状態はバージョン履歴に残す(取り消せるように)。
            let previous = match hub.current_state(id)? {
                Some(current) => Some(crate::versions::create(
                    store,
                    id,
                    &current,
                    Some(&format!("バックアップ({when})から戻す前")),
                    actor,
                )?),
                None => None,
            };
            if !state.is_empty() {
                hub.restore(id, &state).map_err(RestoreError::Failed)?;
            }
            Ok(RestoreResult {
                board: id.to_string(),
                recreated: false,
                previous,
            })
        }
        _ => {
            let mut meta = meta;
            // チームがもうなければ、オーナーの個人ボードとして戻す。
            if let Some(team) = &meta.team
                && store.get_json::<serde_json::Value>(TEAMS, team)?.is_none()
            {
                meta.team = None;
                meta.team_access = None;
            }
            meta.updated = now_ms();
            boards::put(store, &meta)?;
            if let Some(team) = &meta.team {
                boards::attach(store, team, id)?;
            }
            for (k, v) in data.table("user_boards") {
                if let Key::StrStr(login, b) = k
                    && b == id
                {
                    store.put_pair_bytes(USER_BOARDS, login, id, v)?;
                }
            }
            for (table, def) in [
                ("comments", COMMENTS),
                ("media", MEDIA),
                ("media_info", MEDIA_INFO),
                ("version_info", VERSION_INFO),
            ] {
                for (k, v) in data.of_board(table, id) {
                    if let Key::StrU64(_, n) = k {
                        store.put_seq_raw(def, id, *n, v)?;
                    }
                }
            }
            for (k, _) in data.of_board("version_info", id) {
                if let Key::StrU64(_, vid) = k
                    && let Some(v) = data.version_state(id, *vid)
                {
                    store.version_put(id, *vid, v)?;
                }
            }
            // 削除のあとに同じ ID のルームが開かれていても、戻した中身を正とする。
            if hub.get(id).is_some() {
                hub.delete(id)?;
            }
            // 一度も開かれていないボードには中身がない(開いたときに新しく作られる)。
            if !state.is_empty() {
                store.save(id, &state)?;
            }
            Ok(RestoreResult {
                board: id.to_string(),
                recreated: true,
                previous: None,
            })
        }
    }
}

// ---- 設定と定期実行 ----

pub struct BackupConfig {
    pub target: Arc<dyn Blob>,
    /// 画面に出す保存先の説明(認証情報は含めない)。
    pub description: String,
    pub interval_hours: u64,
    pub keep: usize,
}

impl BackupConfig {
    /// 環境変数から読む。`data_dir` は保存しているときの `DATA_DIR`。
    /// 保存しない設定(`DATA_DIR=`)で `BACKUP_LOCATION` もなければ `None`。
    pub fn from_env(
        data_dir: Option<&Path>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, String> {
        let interval_hours = env("BACKUP_INTERVAL_HOURS")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DEFAULT_INTERVAL_HOURS);
        let keep = env("BACKUP_KEEP")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DEFAULT_KEEP)
            .max(1);
        let (target, description): (Arc<dyn Blob>, String) = match env("BACKUP_LOCATION")
            .filter(|v| !v.trim().is_empty())
        {
            Some(url) => {
                // 保存場所と同じ書き方。認証情報は BACKUP_… で渡す。
                let mapped = |k: &str| match k.strip_prefix("STORAGE_BACKUP_") {
                    Some(rest) => env(&format!("BACKUP_{rest}")).or_else(|| env(k)),
                    None => env(k),
                };
                let mut locs = storage::parse_locations(&format!("backup={}", url.trim()), mapped)
                    .map_err(|e| {
                        format!(
                            "BACKUP_LOCATION: {}",
                            e.replace("STORAGE_BACKUP_", "BACKUP_")
                        )
                    })?;
                let loc = locs.pop().ok_or("BACKUP_LOCATION が空です")?;
                let desc = url.split('?').next().unwrap_or(&url).to_string();
                (loc.blob, format!("{}({desc})", loc.description))
            }
            None => match data_dir {
                Some(dir) => {
                    // キーが `backups/…` なので、ファイルは DATA_DIR/backups/ に並ぶ。
                    let desc = format!("データと同じディスク({})", dir.join("backups").display());
                    (Arc::new(DirBlob::new(dir.to_path_buf())), desc)
                }
                None => return Ok(None),
            },
        };
        Ok(Some(Self {
            target,
            description,
            interval_hours,
            keep,
        }))
    }
}

#[derive(Default)]
struct RunState {
    running: bool,
    last_error: Option<String>,
    last_attempt: Option<u64>,
    next_at: Option<u64>,
}

/// バックアップの置き場所と、取る・戻す処理。
pub struct Backups {
    config: BackupConfig,
    hub: Arc<Hub>,
    state: Mutex<RunState>,
    /// 一度に 1 つだけ取る(一覧の書き換えも守る)。
    op: Mutex<()>,
}

impl Backups {
    pub fn new(config: BackupConfig, hub: Arc<Hub>) -> Self {
        Self {
            config,
            hub,
            state: Mutex::default(),
            op: Mutex::new(()),
        }
    }

    pub fn config(&self) -> &BackupConfig {
        &self.config
    }

    /// 新しい順。
    pub fn list(&self) -> Result<Vec<BackupInfo>, StoreError> {
        let mut v: Vec<BackupInfo> = match self.config.target.get(INDEX_KEY)? {
            Some(b) => serde_json::from_slice(&b)?,
            None => Vec::new(),
        };
        v.sort_by_key(|b| std::cmp::Reverse(b.id));
        Ok(v)
    }

    fn write_index(&self, list: &[BackupInfo]) -> Result<(), StoreError> {
        self.config
            .target
            .put(INDEX_KEY, &serde_json::to_vec_pretty(list)?)
    }

    /// 今すぐ取る(同期。非同期の中からは `spawn_blocking` で呼ぶ)。
    pub fn create(&self, trigger: &str, by: Option<&str>) -> Result<BackupInfo, StoreError> {
        let _op = self.op.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.running = true;
            s.last_attempt = Some(now_ms());
        }
        let res = self.create_inner(trigger, by);
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.running = false;
        s.last_error = res.as_ref().err().map(|e| e.to_string());
        res
    }

    fn create_inner(&self, trigger: &str, by: Option<&str>) -> Result<BackupInfo, StoreError> {
        // 開いているボードの未保存の変更を先に書き出す。
        self.hub.flush();
        let (bytes, stats) = dump(self.hub.store())?;
        let info = BackupInfo {
            id: crate::store::next_id(),
            created: now_ms(),
            bytes: bytes.len() as u64,
            boards: stats.boards,
            users: stats.users,
            trigger: trigger.into(),
            by: by.map(str::to_string),
            skipped: stats.skipped,
        };
        self.config.target.put(&backup_key(info.id), &bytes)?;
        let mut list = self.list()?;
        list.push(info.clone());
        // 古いものを消す(定期と手動で別々に数える)。
        let keep = self.config.keep;
        let mut removed = Vec::new();
        for kind in ["auto", "manual"] {
            let ids: Vec<u64> = list
                .iter()
                .filter(|b| (b.trigger == "auto") == (kind == "auto"))
                .map(|b| b.id)
                .collect();
            if ids.len() > keep {
                let mut ids = ids;
                ids.sort_unstable_by(|a, b| b.cmp(a));
                removed.extend_from_slice(&ids[keep..]);
            }
        }
        list.retain(|b| !removed.contains(&b.id));
        self.write_index(&list)?;
        for id in removed {
            if let Err(e) = self.config.target.delete(&backup_key(id)) {
                tracing::warn!(backup = id, "古いバックアップを消せません: {e}");
            }
        }
        tracing::info!(
            backup = info.id,
            bytes = info.bytes,
            boards = info.boards,
            trigger,
            "バックアップを取りました"
        );
        Ok(info)
    }

    /// ファイル(gzip)のまま。
    pub fn read(&self, id: u64) -> Result<Option<Vec<u8>>, StoreError> {
        if !self.list()?.iter().any(|b| b.id == id) {
            return Ok(None);
        }
        self.config.target.get(&backup_key(id))
    }

    pub fn open(&self, id: u64) -> Result<Option<BackupData>, StoreError> {
        match self.read(id)? {
            Some(b) => Ok(Some(parse(&b)?)),
            None => Ok(None),
        }
    }

    pub fn status(&self) -> Result<BackupStatus, StoreError> {
        let backups = self.list()?;
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(BackupStatus {
            enabled: true,
            target: Some(self.config.description.clone()),
            interval_hours: self.config.interval_hours,
            keep: self.config.keep,
            next_at: s.next_at,
            last_error: s.last_error.clone(),
            running: s.running,
            backups,
        })
    }

    /// 次の定期バックアップの時刻。
    fn next_due(&self, now: u64) -> Result<u64, StoreError> {
        let interval = self.config.interval_hours * 3600 * 1000;
        let last_auto = self
            .list()?
            .iter()
            .filter(|b| b.trigger == "auto")
            .map(|b| b.created)
            .max();
        let attempt = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_attempt;
        let due = match last_auto.max(attempt) {
            Some(t) => t + interval,
            // まだ 1 つもなければ、起動して少ししたら取る。
            None => now + 5 * 60 * 1000,
        };
        Ok(due.max(now + 60 * 1000))
    }

    /// 定期バックアップを回す。
    pub fn spawn(self: Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        if self.config.interval_hours == 0 {
            tracing::info!("定期バックアップ: 止めています(BACKUP_INTERVAL_HOURS=0)");
            return None;
        }
        tracing::info!(
            "定期バックアップ: {} 時間ごと、{} 個まで({})",
            self.config.interval_hours,
            self.config.keep,
            self.config.description
        );
        Some(tokio::spawn(async move {
            loop {
                let me = self.clone();
                let now = now_ms();
                let due = match tokio::task::spawn_blocking(move || me.next_due(now)).await {
                    Ok(Ok(d)) => d,
                    Ok(Err(e)) => {
                        tracing::error!("バックアップの一覧を読めません: {e}");
                        now + 3600 * 1000
                    }
                    Err(_) => now + 3600 * 1000,
                };
                self.state.lock().unwrap_or_else(|e| e.into_inner()).next_at = Some(due);
                tokio::time::sleep(Duration::from_millis(due.saturating_sub(now))).await;
                let me = self.clone();
                match tokio::task::spawn_blocking(move || me.create("auto", None)).await {
                    Ok(Ok(_)) => {}
                    // 失敗はログの ERROR に出す。
                    Ok(Err(e)) => tracing::error!("定期バックアップに失敗しました: {e}"),
                    Err(e) => tracing::error!("定期バックアップが止まりました: {e}"),
                }
            }
        }))
    }
}

// ---- HTTP ----

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/admin/backups", get(status).post(create))
        .route("/admin/backups/{id}/download", get(download))
        .route("/admin/backups/{id}/boards", get(boards_in))
        .route("/admin/backups/{id}/boards/{board}/restore", post(restore))
}

fn backups(state: &AppState) -> Result<Arc<Backups>, Error> {
    state.config.backups.clone().ok_or_else(|| {
        Error::bad_request(
            "バックアップが設定されていません(DATA_DIR か BACKUP_LOCATION を設定してください)",
        )
    })
}

fn not_found() -> Error {
    Error::new(StatusCode::NOT_FOUND, "そのバックアップは見つかりません")
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::from(StoreError::from(e.to_string())))?
}

async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<BackupStatus>, Error> {
    require_admin(&state, &headers)?;
    let Some(b) = state.config.backups.clone() else {
        return Ok(Json(BackupStatus::default()));
    };
    Ok(Json(blocking(move || Ok(b.status()?)).await?))
}

async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<BackupInfo>, Error> {
    let user = require_admin(&state, &headers)?;
    let b = backups(&state)?;
    let login = user.login.clone();
    let info = blocking(move || {
        b.create("manual", Some(&login))
            .map_err(|e| Error::bad_request(format!("バックアップを取れませんでした: {e}")))
    })
    .await?;
    audit::record(
        &state,
        &headers,
        AuditEntry::new("backup.create")
            .actor(&user.login)
            .target(info.id.to_string()),
    );
    Ok(Json(info))
}

async fn download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<u64>,
) -> Result<Response, Error> {
    require_admin(&state, &headers)?;
    let b = backups(&state)?;
    let bytes = blocking(move || Ok(b.read(id)?))
        .await?
        .ok_or_else(not_found)?;
    let name = format!(
        "graboard-{}.grabak.gz",
        crate::logging::rfc3339(id / 1000)
            .get(..19)
            .unwrap_or("backup")
            .replace([':', 'T'], "-")
    );
    Ok((
        [
            (CONTENT_TYPE, "application/gzip".to_string()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

async fn boards_in(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Vec<BackupBoard>>, Error> {
    require_admin(&state, &headers)?;
    let b = backups(&state)?;
    let store = state.store().clone();
    Ok(Json(
        blocking(move || {
            let data = b.open(id)?.ok_or_else(not_found)?;
            Ok(list_boards(&store, &data)?)
        })
        .await?,
    ))
}

async fn restore(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath((id, board)): UrlPath<(u64, String)>,
) -> Result<Json<RestoreResult>, Error> {
    let user = require_admin(&state, &headers)?;
    if !crate::valid_room_name(&board) {
        return Err(Error::bad_request("ボード ID が正しくありません"));
    }
    let b = backups(&state)?;
    let hub = state.hub.clone();
    let actor = user.clone();
    let room = board.clone();
    let result = blocking(move || {
        let data = b.open(id)?.ok_or_else(not_found)?;
        Ok(restore_board(&hub, &data, &room, Some(&actor))?)
    })
    .await?;
    tracing::info!(board = %board, backup = id, recreated = result.recreated, "バックアップから戻しました");
    let team = boards::get(state.store(), &board)?.and_then(|m| m.team);
    audit::record(
        &state,
        &headers,
        AuditEntry::new("backup.restore")
            .actor(&user.login)
            .board(&board)
            .team(team)
            .target(id.to_string())
            .detail(if result.recreated {
                "作り直し"
            } else {
                "中身を戻す"
            }),
    );
    state
        .hub
        .refresh(&board, board_core::protocol::TOPIC_VERSIONS);
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::Board;

    fn add_sticky(b: &mut Board, text: &str) {
        let id = b.alloc_id();
        let mut s = board_core::Shape::sticky(id, board_core::Vec2::ZERO, board_core::Rgba8::WHITE);
        s.text = text.into();
        b.push(s);
    }

    fn board_state(text: &str) -> Vec<u8> {
        let mut b = Board::new();
        add_sticky(&mut b, text);
        b.encode_diff(None).unwrap()
    }

    fn setup() -> (Arc<Hub>, BoardMeta) {
        let store = Arc::new(Store::memory());
        crate::auth::signup(&store, "alice", "Alice", "password-123").unwrap();
        let meta = boards::create(&store, "alice", "企画会議", None).unwrap();
        store.save(&meta.id, &board_state("最初")).unwrap();
        store
            .put_seq(
                COMMENTS,
                &meta.id,
                1,
                &serde_json::json!({"body": "コメント"}),
            )
            .unwrap();
        crate::versions::create(&store, &meta.id, &board_state("昔"), Some("v1"), None).unwrap();
        (Arc::new(Hub::new(store)), meta)
    }

    fn texts(hub: &Hub, id: &str) -> Vec<String> {
        hub.read(id, |b| {
            b.elements()
                .iter()
                .map(|e| e.text().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap()
    }

    #[test]
    fn dump_and_parse_roundtrip() {
        let (hub, meta) = setup();
        let (gz, stats) = dump(hub.store()).unwrap();
        assert_eq!(stats.boards, 1);
        assert_eq!(stats.users, 1);
        assert!(stats.skipped.is_empty(), "{:?}", stats.skipped);
        let data = parse(&gz).unwrap();
        assert_eq!(data.header.format, FORMAT);
        assert!(data.header.tables.contains(&"rooms".to_string()));
        assert_eq!(data.board(&meta.id).unwrap().title, "企画会議");
        assert!(data.board_state(&meta.id).is_some());

        // 別の空のストアに丸ごと戻す。
        let fresh = Store::memory();
        restore_all(&fresh, &data).unwrap();
        assert_eq!(
            fresh.load(&meta.id).unwrap(),
            hub.store().load(&meta.id).unwrap()
        );
        assert!(crate::auth::get_user(&fresh, "alice").unwrap().is_some());
        assert_eq!(crate::versions::list(&fresh, &meta.id).unwrap().len(), 1);
        let (again, _) = dump(&fresh).unwrap();
        assert_eq!(parse(&again).unwrap().records, data.records);

        assert!(parse(b"not gzip").is_err());
        assert!(parse(&gz[..gz.len() / 2]).is_err());
    }

    #[test]
    fn restore_existing_board_keeps_previous_as_version() {
        let (hub, meta) = setup();
        let data = parse(&dump(hub.store()).unwrap().0).unwrap();
        hub.edit::<_, String>(&meta.id, |b| {
            add_sticky(b, "あとから");
            Ok(())
        })
        .unwrap();
        assert_eq!(texts(&hub, &meta.id).len(), 2);
        let r = restore_board(&hub, &data, &meta.id, None).unwrap();
        assert!(!r.recreated);
        assert!(r.previous.unwrap().label.unwrap().contains("から戻す前"));
        assert_eq!(texts(&hub, &meta.id), vec!["最初".to_string()]);
        assert!(matches!(
            restore_board(&hub, &data, "nothing", None),
            Err(RestoreError::NotFound)
        ));
    }

    #[test]
    fn restore_deleted_board_recreates_everything() {
        let (hub, meta) = setup();
        let store = hub.store().clone();
        let data = parse(&dump(&store).unwrap().0).unwrap();
        assert_eq!(list_boards(&store, &data).unwrap()[0].current, "exists");

        // 削除の処理と同じことをする。
        let mut m = boards::get(&store, &meta.id).unwrap().unwrap();
        m.deleted = true;
        boards::put(&store, &m).unwrap();
        boards::forget(&store, "alice", &meta.id).unwrap();
        crate::comments::delete_all(&store, &meta.id).unwrap();
        crate::versions::delete_all(&store, &meta.id).unwrap();
        hub.delete(&meta.id).unwrap();
        let listed = list_boards(&store, &data).unwrap();
        assert_eq!(listed[0].current, "deleted");
        assert_eq!(listed[0].comments, 1);
        assert_eq!(listed[0].versions, 1);

        let r = restore_board(&hub, &data, &meta.id, None).unwrap();
        assert!(r.recreated);
        let back = boards::get(&store, &meta.id).unwrap().unwrap();
        assert!(!back.deleted);
        assert_eq!(boards::owned_count(&store, "alice").unwrap(), 1);
        assert_eq!(crate::versions::list(&store, &meta.id).unwrap().len(), 1);
        assert_eq!(
            store
                .scan_seq::<serde_json::Value>(COMMENTS, &meta.id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(texts(&hub, &meta.id), vec!["最初".to_string()]);
    }

    #[test]
    fn backups_rotate_and_index() {
        let (hub, meta) = setup();
        let dir = tempfile::tempdir().unwrap();
        let config = BackupConfig::from_env(Some(dir.path()), |k| match k {
            "BACKUP_KEEP" => Some("2".into()),
            _ => None,
        })
        .unwrap()
        .unwrap();
        assert_eq!(config.interval_hours, DEFAULT_INTERVAL_HOURS);
        let b = Backups::new(config, hub);
        let first = b.create("auto", None).unwrap();
        b.create("auto", None).unwrap();
        b.create("manual", Some("alice")).unwrap();
        let third = b.create("auto", None).unwrap();
        let list = b.list().unwrap();
        assert_eq!(list.len(), 3, "定期 2 つと手動 1 つ");
        assert_eq!(list[0].id, third.id);
        assert!(b.read(first.id).unwrap().is_none());
        assert!(
            !dir.path()
                .join(format!("backups/{}.grabak.gz", first.id))
                .exists()
        );
        let data = b.open(third.id).unwrap().unwrap();
        assert!(data.board(&meta.id).is_some());
        let st = b.status().unwrap();
        assert!(st.enabled && !st.running && st.last_error.is_none());
        // 次の定期は最後の定期から間隔をあける。
        let due = b.next_due(third.created).unwrap();
        assert!(due >= third.created + DEFAULT_INTERVAL_HOURS * 3600 * 1000 - 1000);
    }

    #[test]
    fn config_from_env() {
        assert!(BackupConfig::from_env(None, |_| None).unwrap().is_none());
        let err = BackupConfig::from_env(None, |k| {
            (k == "BACKUP_LOCATION").then(|| "s3://bucket/x".to_string())
        })
        .err()
        .unwrap();
        assert!(err.contains("BACKUP_ACCESS_KEY_ID"), "{err}");
        let c = BackupConfig::from_env(None, |k| match k {
            "BACKUP_LOCATION" => Some(
                "s3://bucket/graboard?endpoint=https://storage.googleapis.com&region=auto".into(),
            ),
            "BACKUP_ACCESS_KEY_ID" => Some("GOOG1".into()),
            "BACKUP_SECRET_ACCESS_KEY" => Some("secret".into()),
            "BACKUP_INTERVAL_HOURS" => Some("6".into()),
            _ => None,
        })
        .unwrap()
        .unwrap();
        assert_eq!(c.interval_hours, 6);
        assert!(c.description.contains("s3://bucket/graboard"));
        assert!(!c.description.contains("secret"));
    }
}
