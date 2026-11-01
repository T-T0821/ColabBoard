//! 音声(16 kHz モノラル)を Whisper が読む log-mel スペクトログラムにする。
//!
//! OpenAI の実装(`whisper/audio.py`)と同じ手順:
//! 1. 30 秒に満たなければ無音で埋める(長ければ切る)
//! 2. 400 点のハン窓・160 点ずらしの STFT(両端は反射で 200 点ずつ延ばす)
//! 3. パワースペクトルに mel フィルタ(Slaney 型、librosa と同じ)をかける
//! 4. log10、最大値 - 8 で下を切り、`(x + 4) / 4`
//!
//! candle にも実装があるが、スレッドを立てるのでブラウザ(wasm)では動かない。

use std::f32::consts::PI;
use std::sync::OnceLock;

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// サンプリング周波数(Hz)。
pub const SAMPLE_RATE: usize = 16_000;
pub const N_FFT: usize = 400;
pub const HOP: usize = 160;
/// 1 回に読む長さ(サンプル)= 30 秒。
pub const N_SAMPLES: usize = 30 * SAMPLE_RATE;
/// 1 回に読むフレーム数。
pub const N_FRAMES: usize = N_SAMPLES / HOP;

fn hz_to_mel(f: f64) -> f64 {
    // Slaney: 1000 Hz までは線形、そこから対数。
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    if f >= min_log_hz {
        min_log_mel + (f / min_log_hz).ln() / logstep
    } else {
        f / f_sp
    }
}

fn mel_to_hz(m: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    if m >= min_log_mel {
        min_log_hz * (logstep * (m - min_log_mel)).exp()
    } else {
        f_sp * m
    }
}

/// mel フィルタ(`n_mels` × (N_FFT/2+1)、行優先)。`librosa.filters.mel(sr=16000, n_fft=400, n_mels)` と同じ。
pub fn mel_filters(n_mels: usize) -> Vec<f32> {
    let n_freq = N_FFT / 2 + 1;
    let fft_freqs: Vec<f64> = (0..n_freq)
        .map(|i| i as f64 * SAMPLE_RATE as f64 / N_FFT as f64)
        .collect();
    let min_mel = hz_to_mel(0.0);
    let max_mel = hz_to_mel(SAMPLE_RATE as f64 / 2.0);
    let mel_f: Vec<f64> = (0..n_mels + 2)
        .map(|i| mel_to_hz(min_mel + (max_mel - min_mel) * i as f64 / (n_mels + 1) as f64))
        .collect();
    let mut out = vec![0f32; n_mels * n_freq];
    for m in 0..n_mels {
        let (lo, mid, hi) = (mel_f[m], mel_f[m + 1], mel_f[m + 2]);
        let enorm = 2.0 / (hi - lo);
        for (k, &f) in fft_freqs.iter().enumerate() {
            let lower = (f - lo) / (mid - lo);
            let upper = (hi - f) / (hi - mid);
            let w = lower.min(upper).max(0.0);
            out[m * n_freq + k] = (w * enorm) as f32;
        }
    }
    out
}

fn hann() -> &'static [f32] {
    static HANN: OnceLock<Vec<f32>> = OnceLock::new();
    HANN.get_or_init(|| {
        // torch.hann_window(400) は周期的(periodic)。
        (0..N_FFT)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / N_FFT as f32).cos())
            .collect()
    })
}

/// 30 秒ぶんの log-mel(`n_mels` × [`N_FRAMES`]、行優先)。
pub fn log_mel(pcm: &[f32], n_mels: usize) -> Vec<f32> {
    let mut audio = pcm[..pcm.len().min(N_SAMPLES)].to_vec();
    audio.resize(N_SAMPLES, 0.0);
    // center=True: 両端を反射で延ばす。
    let pad = N_FFT / 2;
    let mut padded = Vec::with_capacity(N_SAMPLES + 2 * pad);
    padded.extend((1..=pad).rev().map(|i| audio[i]));
    padded.extend_from_slice(&audio);
    padded.extend((0..pad).map(|i| audio[N_SAMPLES - 2 - i]));

    let n_freq = N_FFT / 2 + 1;
    let filters = mel_filters(n_mels);
    let window = hann();
    let fft = FftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let mut buf = vec![Complex::new(0.0, 0.0); N_FFT];
    let mut scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let mut power = vec![0f32; n_freq];
    let mut mel = vec![0f32; n_mels * N_FRAMES];
    // STFT は N_FRAMES + 1 フレームできるが、最後の 1 つは捨てる(OpenAI の実装と同じ)。
    for t in 0..N_FRAMES {
        let start = t * HOP;
        for (i, c) in buf.iter_mut().enumerate() {
            *c = Complex::new(padded[start + i] * window[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scratch);
        for (k, p) in power.iter_mut().enumerate() {
            *p = buf[k].norm_sqr();
        }
        for m in 0..n_mels {
            let row = &filters[m * n_freq..(m + 1) * n_freq];
            let s: f32 = row.iter().zip(&power).map(|(a, b)| a * b).sum();
            mel[m * N_FRAMES + t] = s.max(1e-10).log10();
        }
    }
    let max = mel.iter().copied().fold(f32::MIN, f32::max);
    for v in &mut mel {
        *v = (v.max(max - 8.0) + 4.0) / 4.0;
    }
    mel
}

/// 任意のサンプリング周波数の音声を 16 kHz にする(線形補間。前に簡単な低域通過をかける)。
pub fn resample(input: &[f32], from_rate: f32) -> Vec<f32> {
    let to = SAMPLE_RATE as f32;
    if input.is_empty() || (from_rate - to).abs() < 1.0 {
        return input.to_vec();
    }
    let ratio = from_rate / to;
    // 下げるときは、折り返し雑音を減らすために区間平均をとる。
    let smoothed: Vec<f32> = if ratio > 1.0 {
        let w = ratio.round().max(1.0) as usize;
        let mut acc = 0f32;
        let mut out = Vec::with_capacity(input.len());
        for i in 0..input.len() {
            acc += input[i];
            if i >= w {
                acc -= input[i - w];
            }
            out.push(acc / (i + 1).min(w) as f32);
        }
        out
    } else {
        input.to_vec()
    };
    let n = ((input.len() as f32) / ratio).floor() as usize;
    (0..n)
        .map(|i| {
            let x = i as f32 * ratio;
            let i0 = x.floor() as usize;
            let i1 = (i0 + 1).min(smoothed.len() - 1);
            let f = x - i0 as f32;
            smoothed[i0] * (1.0 - f) + smoothed[i1] * f
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_match_librosa_shape() {
        let f = mel_filters(80);
        assert_eq!(f.len(), 80 * 201);
        // 各フィルタは三角形で、どれかの周波数に重みがある。
        for m in 0..80 {
            assert!(f[m * 201..(m + 1) * 201].iter().any(|&w| w > 0.0), "{m}");
        }
        // librosa.filters.mel(sr=16000, n_fft=400, n_mels=80)[0][1] = 0.02486259
        assert!((f[1] - 0.024_862_59).abs() < 1e-6, "{}", f[1]);
        for (m, k, w) in [
            (10, 10, 0.019_908_218),
            (40, 43, 0.014_735_566),
            (79, 192, 0.003_164_711_9),
        ] {
            assert!(
                (f[m * 201 + k] - w).abs() < 1e-6,
                "{m} {k} {}",
                f[m * 201 + k]
            );
        }
        // large-v3 などの 128 帯。
        let f = mel_filters(128);
        assert!((f[1] - 0.012_373_987).abs() < 1e-6);
        assert!((f[100 * 201 + 102] - 0.009_409_416_5).abs() < 1e-6);
    }

    #[test]
    fn silence_is_flat() {
        let mel = log_mel(&[0.0; 1600], 80);
        assert_eq!(mel.len(), 80 * N_FRAMES);
        assert!(mel.iter().all(|&v| (v - mel[0]).abs() < 1e-6));
    }

    #[test]
    fn tone_peaks_in_low_band() {
        let pcm: Vec<f32> = (0..SAMPLE_RATE)
            .map(|i| (2.0 * PI * 440.0 * i as f32 / SAMPLE_RATE as f32).sin())
            .collect();
        let mel = log_mel(&pcm, 80);
        let frame = 50;
        let best = (0..80)
            .max_by(|&a, &b| mel[a * N_FRAMES + frame].total_cmp(&mel[b * N_FRAMES + frame]))
            .unwrap();
        // 440 Hz は mel で 6.6 → 80 帯のうち 13 番目あたり。
        assert!((10..=16).contains(&best), "{best}");
    }

    #[test]
    fn resample_halves() {
        let x: Vec<f32> = (0..32_000).map(|i| (i as f32 * 0.001).sin()).collect();
        let y = resample(&x, 32_000.0);
        assert_eq!(y.len(), 16_000);
        assert!((y[8000] - (16_000.0f32 * 0.001).sin()).abs() < 0.01);
    }
}
