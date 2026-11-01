//! Whisper(OpenAI の音声認識モデル)をブラウザ内で動かす。モデル本体は candle の実装を使う。
//!
//! 読み込むファイルは Hugging Face の `openai/whisper-*` と同じ形式:
//! `config.json`・`tokenizer.json`・`model.safetensors`。
//!
//! 文字起こしは貪欲法(毎回いちばん確からしいトークンを選ぶ)。タイムスタンプは出さない。
//! 無音や雑音だけのときは「発話なし」の確率を見て空文字を返し、同じ言い回しを繰り返し始めたら
//! そこで打ち切る(Whisper が無音で起こしやすい失敗)。

use candle_core::{D, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_nn::ops::{log_softmax, softmax};
use candle_transformers::models::whisper::{self as w, model::Whisper as Model};

use crate::mel;
use crate::tokenizer::Tokenizer;

/// 1 回に出すトークンの上限(Whisper の文脈長 448 の半分)。
const MAX_TOKENS: usize = 224;
/// 「発話なし」とみなす確率(このうえ平均の対数確率が [`LOGPROB_THRESHOLD`] より低いとき)。
const NO_SPEECH_THRESHOLD: f32 = 0.6;
const LOGPROB_THRESHOLD: f32 = -1.0;
/// これより静かな音声(RMS)は読まずに「発話なし」にする。
const SILENCE_RMS: f32 = 0.002;

pub struct Whisper {
    model: Model,
    tokenizer: Tokenizer,
    device: Device,
    sot: u32,
    eot: u32,
    transcribe: u32,
    no_timestamps: u32,
    no_speech: Option<u32>,
    /// 出さないトークン(特別なトークンとタイムスタンプ)を -inf にするための足し算用の表。
    suppress: Tensor,
    /// 最初のトークンでは、さらに文末と空白も出さない。
    suppress_first: Tensor,
}

/// 文字起こしの結果。
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    /// 使った(または推定した)言語。
    pub language: String,
    /// 発話がないと判断した。
    pub no_speech: bool,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Whisper {
    pub fn load(config: &[u8], tokenizer: &[u8], weights: Vec<u8>) -> Result<Self, String> {
        let device = Device::Cpu;
        let config: w::Config =
            serde_json::from_slice(config).map_err(|e| format!("config.json: {e}"))?;
        let tokenizer = Tokenizer::from_json(tokenizer)?;
        let vb = VarBuilder::from_buffered_safetensors(weights, w::DTYPE, &device)
            .map_err(|e| format!("model.safetensors: {e}"))?;
        let need = |name: &str| {
            tokenizer
                .special(name)
                .ok_or_else(|| format!("tokenizer.json に {name} がありません"))
        };
        let sot = need(w::SOT_TOKEN)?;
        let eot = need(w::EOT_TOKEN)?;
        let transcribe = need(w::TRANSCRIBE_TOKEN)?;
        let no_timestamps = need(w::NO_TIMESTAMPS_TOKEN)?;
        let no_speech = w::NO_SPEECH_TOKENS
            .iter()
            .find_map(|t| tokenizer.special(t));
        let vocab = config.vocab_size;
        let blocked = |first: bool| -> Vec<f32> {
            (0..vocab as u32)
                .map(|id| {
                    let blocked = config.suppress_tokens.contains(&id)
                        || (id != eot && tokenizer.is_special(id))
                        || (first && (id == eot || tokenizer.decode(&[id]) == " "));
                    if blocked { f32::NEG_INFINITY } else { 0.0 }
                })
                .collect()
        };
        let suppress = Tensor::new(blocked(false).as_slice(), &device).map_err(err)?;
        let suppress_first = Tensor::new(blocked(true).as_slice(), &device).map_err(err)?;
        let model = Model::load(&vb, config).map_err(|e| format!("モデルを読めません: {e}"))?;
        Ok(Self {
            model,
            tokenizer,
            device,
            sot,
            eot,
            transcribe,
            no_timestamps,
            no_speech,
            suppress,
            suppress_first,
        })
    }

    pub fn n_mels(&self) -> usize {
        self.model.config.num_mel_bins
    }

    /// 最後の位置のロジット。
    fn logits(&mut self, tokens: &[u32], features: &Tensor, first: bool) -> Result<Tensor, String> {
        let t = Tensor::new(tokens, &self.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(err)?;
        let ys = self
            .model
            .decoder
            .forward(&t, features, first)
            .map_err(err)?;
        let (_, n, _) = ys.dims3().map_err(err)?;
        self.model
            .decoder
            .final_linear(&ys.i((..1, n - 1..)).map_err(err)?)
            .and_then(|l| l.i(0)?.i(0))
            .map_err(err)
    }

    /// 言語を推定する。
    fn detect_language(&mut self, features: &Tensor) -> Result<(u32, String), String> {
        let logits = self.logits(&[self.sot], features, true)?;
        let langs = self.tokenizer.languages();
        let ids: Vec<u32> = langs.iter().map(|(id, _)| *id).collect();
        let ids = Tensor::new(ids.as_slice(), &self.device).map_err(err)?;
        let scores: Vec<f32> = logits
            .index_select(&ids, 0)
            .and_then(|t| t.to_vec1())
            .map_err(err)?;
        let best = scores
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .ok_or("言語トークンがありません")?;
        Ok(langs[best].clone())
    }

    /// 16 kHz モノラルの音声(30 秒まで)を文字にする。`language` は "ja" などで、`None` なら推定する。
    pub fn transcribe(
        &mut self,
        pcm: &[f32],
        language: Option<&str>,
    ) -> Result<Transcript, String> {
        let rms = (pcm.iter().map(|x| x * x).sum::<f32>() / pcm.len().max(1) as f32).sqrt();
        if rms < SILENCE_RMS {
            return Ok(Transcript {
                text: String::new(),
                language: language.unwrap_or_default().to_string(),
                no_speech: true,
            });
        }
        let n_mels = self.n_mels();
        let mel = mel::log_mel(pcm, n_mels);
        let mel = Tensor::from_vec(mel, (1, n_mels, mel::N_FRAMES), &self.device).map_err(err)?;
        self.model.reset_kv_cache();
        let features = self.model.encoder.forward(&mel, true).map_err(err)?;

        let (lang_id, lang) = match language.and_then(|l| {
            self.tokenizer
                .special(&format!("<|{l}|>"))
                .map(|id| (id, l.to_string()))
        }) {
            Some(x) => x,
            None => self.detect_language(&features)?,
        };
        let prompt = [self.sot, lang_id, self.transcribe, self.no_timestamps];
        let mut tokens = prompt.to_vec();
        let mut no_speech_p = 0f32;
        let mut logprob_sum = 0f32;
        for step in 0..MAX_TOKENS {
            let t = Tensor::new(tokens.as_slice(), &self.device)
                .and_then(|t| t.unsqueeze(0))
                .map_err(err)?;
            let ys = self
                .model
                .decoder
                .forward(&t, &features, step == 0)
                .map_err(err)?;
            let (_, n, _) = ys.dims3().map_err(err)?;
            if step == 0 {
                // 先頭(sot の位置)の出力で「発話なし」の確率を見る。
                if let Some(ns) = self.no_speech {
                    let first = self
                        .model
                        .decoder
                        .final_linear(&ys.i((..1, ..1)).map_err(err)?)
                        .and_then(|l| l.i(0)?.i(0))
                        .map_err(err)?;
                    no_speech_p = softmax(&first, D::Minus1)
                        .and_then(|p| p.i(ns as usize)?.to_scalar())
                        .map_err(err)?;
                }
            }
            let logits = self
                .model
                .decoder
                .final_linear(&ys.i((..1, n - 1..)).map_err(err)?)
                .and_then(|l| l.i(0)?.i(0))
                .map_err(err)?;
            let mask = if step == 0 {
                &self.suppress_first
            } else {
                &self.suppress
            };
            let logits = (logits + mask).map_err(err)?;
            let next = logits
                .argmax(D::Minus1)
                .and_then(|t| t.to_scalar::<u32>())
                .map_err(err)?;
            let logprob: f32 = log_softmax(&logits, D::Minus1)
                .and_then(|l| l.i(next as usize)?.to_scalar())
                .map_err(err)?;
            logprob_sum += logprob;
            if next == self.eot {
                break;
            }
            tokens.push(next);
            if let Some(cut) = repetition(&tokens[prompt.len()..]) {
                tokens.truncate(prompt.len() + cut);
                break;
            }
        }
        let body = &tokens[prompt.len()..];
        let avg_logprob = logprob_sum / (body.len() + 1) as f32;
        let text = self.tokenizer.decode(body).trim().to_string();
        let silent = (no_speech_p > NO_SPEECH_THRESHOLD && avg_logprob < LOGPROB_THRESHOLD)
            || (no_speech_p > HALLUCINATION_P && is_stock_phrase(&text));
        let text = if silent { String::new() } else { text };
        Ok(Transcript {
            no_speech: text.is_empty(),
            text,
            language: lang,
        })
    }
}

/// 発話がなさそうなときに、決まり文句が出たら捨てる確率。
const HALLUCINATION_P: f32 = 0.3;

/// 無音や雑音のときに Whisper がよく作ってしまう決まり文句(動画の字幕で学習したため)。
pub fn is_stock_phrase(text: &str) -> bool {
    const PHRASES: [&str; 12] = [
        "ご視聴ありがとうございました",
        "ご覧いただきありがとうございます",
        "チャンネル登録",
        "おやすみなさい",
        "字幕",
        "最後までご視聴",
        "thank you for watching",
        "thanks for watching",
        "please subscribe",
        "subtitles by",
        "thank you.",
        "you",
    ];
    let t = text
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation() || "。、!?! ".contains(c))
        .to_lowercase();
    !t.is_empty()
        && PHRASES.iter().any(|p| {
            let p = p.trim_end_matches('.');
            t == p
                || (p.chars().count() >= 4
                    && t.contains(p)
                    && t.chars().count() < p.chars().count() + 8)
        })
}

/// 同じ並びを繰り返し始めていたら、1 回目の終わりの位置を返す。
/// 1〜16 トークンの並びが 4 回以上続いたら繰り返しとみなす。
pub fn repetition(tokens: &[u32]) -> Option<usize> {
    for len in 1..=16 {
        let times = if len == 1 { 8 } else { 4 };
        let span = len * times;
        if tokens.len() < span {
            continue;
        }
        let tail = &tokens[tokens.len() - span..];
        let unit = &tail[..len];
        if tail.chunks(len).all(|c| c == unit) {
            return Some(tokens.len() - span + len);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_phrases() {
        assert!(is_stock_phrase("ご視聴ありがとうございました"));
        assert!(is_stock_phrase("ご視聴ありがとうございました。"));
        assert!(is_stock_phrase("Thank you for watching!"));
        assert!(is_stock_phrase("you"));
        assert!(!is_stock_phrase("youtube の話をしましょう"));
        assert!(!is_stock_phrase(
            "次は字幕の色を決めて、来週までに田中さんが案を出します"
        ));
    }

    #[test]
    fn finds_loops() {
        assert_eq!(repetition(&[1, 2, 3]), None);
        assert_eq!(repetition(&[9, 1, 2, 1, 2, 1, 2, 1, 2]), Some(3));
        assert_eq!(repetition(&[5; 8]), Some(1));
        assert_eq!(repetition(&[5; 7]), None);
        // 「はい、はい」程度の繰り返しは残す。
        assert_eq!(repetition(&[1, 2, 1, 2, 1, 2]), None);
    }
}
