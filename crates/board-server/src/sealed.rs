//! エンドツーエンド暗号化ボードの保存形式: 暗号文を順に並べたログ。
//!
//! サーバーは鍵を持たないので、中身(yrs の更新)を合成できない。届いた暗号文に通し番号を付けて
//! 後ろに足していき、新しく来た人にはログ全体を渡す。クライアントが復号して合成する。
//! ログが長くなったら、クライアントが全体をまとめた暗号文で先頭から置き換える(`compact`)。
//!
//! 保存するバイト列: `GBE1` + 先頭の通し番号 `u64` + 件数 `u32` + (長さ `u32` + 暗号文) × 件数。
//! 先頭 4 バイトの印で、平文のボード(yrs v1 更新)と見分ける。

use std::sync::Arc;

/// 保存データの先頭の印。yrs v1 更新はこの並びで始まらない(先頭は構造体の個数の varint)。
pub const MAGIC: &[u8; 4] = b"GBE1";

/// これより長くなったら、クライアントにまとめてもらう目安(件数)。
pub const COMPACT_HINT: usize = 64;

/// 保存データが暗号化ボードのログか。
pub fn is_sealed(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SealedLog {
    /// `entries[0]` の通し番号。
    pub base: u64,
    pub entries: Vec<Arc<[u8]>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompactError {
    /// 先頭が合わない(ほかの人が先にまとめた)。
    Stale,
    /// 範囲がおかしい。
    Range,
}

impl SealedLog {
    /// 次に付ける通し番号。
    pub fn head(&self) -> u64 {
        self.base + self.entries.len() as u64
    }

    pub fn bytes(&self) -> usize {
        self.entries.iter().map(|e| e.len()).sum()
    }

    /// 後ろに足し、付けた通し番号を返す。
    pub fn append(&mut self, blob: Arc<[u8]>) -> u64 {
        let seq = self.head();
        self.entries.push(blob);
        seq
    }

    /// `base..upto` を `snapshot` 1 つに置き換える。`snapshot` の通し番号は `upto - 1` になる。
    pub fn compact(
        &mut self,
        base: u64,
        upto: u64,
        snapshot: Arc<[u8]>,
    ) -> Result<(), CompactError> {
        if base != self.base {
            return Err(CompactError::Stale);
        }
        if upto <= base || upto > self.head() {
            return Err(CompactError::Range);
        }
        let n = (upto - base) as usize;
        let mut rest = self.entries.split_off(n);
        self.entries.clear();
        self.entries.push(snapshot);
        self.entries.append(&mut rest);
        self.base = upto - 1;
        Ok(())
    }

    /// 全体を 1 つの暗号文にする(鍵の作り直し)。通し番号は続きから。
    pub fn reset(&mut self, snapshot: Arc<[u8]>) {
        self.base = self.head();
        self.entries = vec![snapshot];
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.bytes() + 4 * self.entries.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.base.to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for e in &self.entries {
            out.extend_from_slice(&(e.len() as u32).to_le_bytes());
            out.extend_from_slice(e);
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let err = || "暗号化ボードの保存データが壊れています".to_string();
        if !is_sealed(bytes) {
            return Err(err());
        }
        let u32_at = |at: usize| -> Result<u32, String> {
            Ok(u32::from_le_bytes(
                bytes.get(at..at + 4).ok_or_else(err)?.try_into().unwrap(),
            ))
        };
        let base = u64::from_le_bytes(bytes.get(4..12).ok_or_else(err)?.try_into().unwrap());
        let count = u32_at(12)? as usize;
        let mut at = 16;
        let mut entries = Vec::with_capacity(count.min(bytes.len() / 4));
        for _ in 0..count {
            let len = u32_at(at)? as usize;
            at += 4;
            entries.push(Arc::from(bytes.get(at..at + len).ok_or_else(err)?));
            at += len;
        }
        Ok(Self { base, entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(v: &[u8]) -> Arc<[u8]> {
        Arc::from(v)
    }

    #[test]
    fn append_compact_reset_and_encode() {
        let mut log = SealedLog::default();
        assert_eq!(log.append(b(&[1])), 0);
        assert_eq!(log.append(b(&[2])), 1);
        assert_eq!(log.append(b(&[3])), 2);
        assert_eq!(log.head(), 3);

        // 0..2 をまとめる。3 件目(番号 2)は残る。
        assert_eq!(log.compact(1, 2, b(&[9])), Err(CompactError::Stale));
        assert_eq!(log.compact(0, 4, b(&[9])), Err(CompactError::Range));
        log.compact(0, 2, b(&[12])).unwrap();
        assert_eq!(log.base, 1);
        assert_eq!(log.entries, vec![b(&[12]), b(&[3])]);
        assert_eq!(log.head(), 3);
        assert_eq!(log.append(b(&[4])), 3);

        let back = SealedLog::decode(&log.encode()).unwrap();
        assert_eq!(back, log);
        assert!(is_sealed(&log.encode()));

        log.reset(b(&[7, 7]));
        assert_eq!(log.base, 4);
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.append(b(&[5])), 5);
    }

    #[test]
    fn plain_state_is_not_sealed() {
        let state = board_core::Board::welcome().encode_diff(None).unwrap();
        assert!(!is_sealed(&state));
        assert!(SealedLog::decode(&state).is_err());
        assert!(SealedLog::decode(b"GBE1\0\0").is_err());
        let mut bad = SealedLog {
            base: 0,
            entries: vec![b(&[1, 2, 3])],
        }
        .encode();
        bad.truncate(bad.len() - 1);
        assert!(SealedLog::decode(&bad).is_err());
    }
}
