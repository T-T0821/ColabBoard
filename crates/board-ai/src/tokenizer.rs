//! Whisper のトークン列を文字に戻す(Hugging Face の `tokenizer.json` を読む)。
//!
//! 文字起こしに要るのは「ID → 文字」の変換と、特別なトークンの ID だけなので、
//! `tokenizers` クレート(wasm では重い)は使わずに自前で読む。
//! トークンは GPT-2 と同じバイト単位の BPE で、各バイトが印字できる 1 文字に置き換えてある。

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Deserialize)]
struct Raw {
    model: RawModel,
    #[serde(default)]
    added_tokens: Vec<Added>,
}

#[derive(Deserialize)]
struct RawModel {
    vocab: HashMap<String, u32>,
}

#[derive(Deserialize)]
struct Added {
    id: u32,
    content: String,
}

pub struct Tokenizer {
    /// ID → トークンの元のバイト列。特別なトークンは `None`。
    bytes: Vec<Option<Vec<u8>>>,
    special: HashMap<String, u32>,
}

/// GPT-2 の「印字できる文字 → バイト」の表。
fn byte_decoder() -> HashMap<char, u8> {
    let mut bs: Vec<u32> = (b'!' as u32..=b'~' as u32)
        .chain(0xA1..=0xAC)
        .chain(0xAE..=0xFF)
        .collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    bs.into_iter()
        .zip(cs)
        .filter_map(|(b, c)| Some((char::from_u32(c)?, b as u8)))
        .collect()
}

impl Tokenizer {
    pub fn from_json(json: &[u8]) -> Result<Self, String> {
        let raw: Raw = serde_json::from_slice(json).map_err(|e| format!("tokenizer.json: {e}"))?;
        let dec = byte_decoder();
        let max = raw
            .model
            .vocab
            .values()
            .chain(raw.added_tokens.iter().map(|a| &a.id))
            .copied()
            .max()
            .unwrap_or(0) as usize;
        let mut bytes = vec![None; max + 1];
        for (tok, id) in &raw.model.vocab {
            let b: Option<Vec<u8>> = tok.chars().map(|c| dec.get(&c).copied()).collect();
            bytes[*id as usize] = b;
        }
        let mut special = HashMap::new();
        for a in raw.added_tokens {
            if a.content.starts_with("<|") {
                bytes[a.id as usize] = None;
                special.insert(a.content, a.id);
            }
        }
        Ok(Self { bytes, special })
    }

    /// 特別なトークン(`<|ja|>` など)の ID。
    pub fn special(&self, name: &str) -> Option<u32> {
        self.special.get(name).copied()
    }

    /// 特別なトークンかどうか(タイムスタンプも含む)。
    pub fn is_special(&self, id: u32) -> bool {
        self.bytes.get(id as usize).is_none_or(Option::is_none)
    }

    /// 文字に戻す。特別なトークンは飛ばす。
    pub fn decode(&self, ids: &[u32]) -> String {
        let mut out = Vec::new();
        for id in ids {
            if let Some(Some(b)) = self.bytes.get(*id as usize) {
                out.extend_from_slice(b);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// 言語トークン(`<|en|>` … )の (ID, 言語コード) の一覧。
    pub fn languages(&self) -> Vec<(u32, String)> {
        let mut out: Vec<(u32, String)> = self
            .special
            .iter()
            .filter_map(|(name, id)| {
                let code = name.strip_prefix("<|")?.strip_suffix("|>")?;
                let is_lang = ((2..=3).contains(&code.len())
                    && code.chars().all(|c| c.is_ascii_lowercase()))
                    || code == "haw"
                    || code == "yue";
                is_lang.then(|| (*id, code.to_string()))
            })
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tokenizer {
        // 「日本」は UTF-8 で e6 97 a5 e6 9c ac。GPT-2 の表では 0xA5 → 'Â¥' など。
        let dec = byte_decoder();
        let enc: HashMap<u8, char> = dec.iter().map(|(c, b)| (*b, *c)).collect();
        let s = |b: &[u8]| b.iter().map(|x| enc[x]).collect::<String>();
        let json = serde_json::json!({
            "model": { "vocab": {
                s(b"Hello"): 0, s(b" world"): 1, s(&[0xe6, 0x97]): 2, s(&[0xa5, 0xe6, 0x9c, 0xac]): 3,
            }},
            "added_tokens": [
                { "id": 4, "content": "<|endoftext|>" },
                { "id": 5, "content": "<|ja|>" },
                { "id": 6, "content": "<|en|>" },
                { "id": 7, "content": "<|transcribe|>" },
            ]
        });
        Tokenizer::from_json(json.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn decodes_bytes_across_tokens() {
        let t = sample();
        assert_eq!(t.decode(&[0, 1, 4]), "Hello world");
        assert_eq!(t.decode(&[2, 3]), "日本");
        assert!(t.is_special(4));
        assert!(!t.is_special(0));
        assert_eq!(t.special("<|ja|>"), Some(5));
    }

    #[test]
    fn lists_languages_only() {
        let t = sample();
        let langs: Vec<String> = t.languages().into_iter().map(|(_, c)| c).collect();
        assert_eq!(langs, ["ja", "en"]);
    }

    #[test]
    fn byte_table_is_complete() {
        assert_eq!(byte_decoder().len(), 256);
    }
}
