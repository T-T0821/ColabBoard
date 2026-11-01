//! 手元で Whisper の文字起こしを試す。
//!
//! `cargo run --release -p board-ai --example transcribe -- <モデルのフォルダ> <音声.wav> [言語]`
//!
//! フォルダには `config.json`・`tokenizer.json`・`model.safetensors` を置く
//! (Hugging Face の `openai/whisper-tiny` などと同じもの)。WAV は 16 bit PCM。

use std::time::Instant;

use board_ai::{Whisper, mel};

/// 16 bit PCM の WAV をモノラルの f32 にする。(サンプル, サンプリング周波数)
fn read_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32), String> {
    let u16le = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32le = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("WAV ではありません".into());
    }
    let (mut pos, mut channels, mut rate, mut bits) = (12, 1u16, 16_000u32, 16u16);
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32le(pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            channels = u16le(body + 2);
            rate = u32le(body + 4);
            bits = u16le(body + 14);
        } else if id == b"data" {
            if bits != 16 {
                return Err(format!("{bits} bit の WAV は読めません"));
            }
            let end = (body + size).min(bytes.len());
            let samples: Vec<f32> = bytes[body..end]
                .chunks_exact(2 * channels as usize)
                .map(|frame| {
                    frame
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|s| i16::from_le_bytes(*s) as f32 / 32768.0)
                        .sum::<f32>()
                        / channels as f32
                })
                .collect();
            return Ok((samples, rate));
        }
        pos = body + size + (size & 1);
    }
    Err("data がありません".into())
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, wav, rest @ ..] = args.as_slice() else {
        return Err("使い方: transcribe <モデルのフォルダ> <音声.wav> [言語]".into());
    };
    let read =
        |name: &str| std::fs::read(format!("{dir}/{name}")).map_err(|e| format!("{name}: {e}"));
    let t = Instant::now();
    let mut model = Whisper::load(
        &read("config.json")?,
        &read("tokenizer.json")?,
        read("model.safetensors")?,
    )?;
    eprintln!("読み込み: {:?}", t.elapsed());
    let (pcm, rate) = read_wav(&std::fs::read(wav).map_err(|e| e.to_string())?)?;
    let pcm = mel::resample(&pcm, rate as f32);
    let t = Instant::now();
    let out = model.transcribe(&pcm, rest.first().map(String::as_str))?;
    eprintln!("文字起こし: {:?}({})", t.elapsed(), out.language);
    println!("{}", out.text);
    Ok(())
}
