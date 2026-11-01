//! 音声認識の部品のうち、ブラウザに依存しないもの(モデルの選択肢・発話の切り出し)。
//! 動かすのは [`crate::local_ai`](ブラウザ内 AI)。

/// モデルの取得元の既定。`{model}` を tiny / base に置き換える。
pub const DEFAULT_MODEL_URL: &str = "https://huggingface.co/openai/whisper-{model}/resolve/main/";

/// 選べる音声認識モデル。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    Tiny,
    Base,
}

impl Model {
    pub const ALL: [Model; 2] = [Model::Tiny, Model::Base];

    pub fn id(self) -> &'static str {
        match self {
            Model::Tiny => "tiny",
            Model::Base => "base",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == s)
    }

    /// 画面に出す名前(大きさの目安つき)。
    pub fn label(self) -> &'static str {
        match self {
            Model::Tiny => "軽い(tiny・約 150 MB)",
            Model::Base => "標準(base・約 290 MB、日本語向き)",
        }
    }
}

/// モデルのファイルの URL(config, tokenizer, weights)。`template` の `{model}` を置き換える。
pub fn model_files(template: &str, m: Model) -> [String; 3] {
    let base = template.trim().replace("{model}", m.id());
    let base = if base.ends_with('/') {
        base
    } else {
        format!("{base}/")
    };
    [
        format!("{base}config.json"),
        format!("{base}tokenizer.json"),
        format!("{base}model.safetensors"),
    ]
}

/// マイクの音を、発話ごとのかたまりに切り出す(音量による簡単な発話検出)。
///
/// 話し始めから、[`Segmenter::SILENCE`] 秒黙るまでを 1 つにする。長く話し続けたら
/// [`Segmenter::MAX`] 秒で区切る。短すぎるもの(咳など)は捨てる。
pub struct Segmenter {
    rate: f32,
    buf: Vec<f32>,
    /// 話し始める前の少しの音(言い始めが切れないように)。
    pre: Vec<f32>,
    speaking: bool,
    silent_for: f32,
    voiced: f32,
    /// 周りの雑音の大きさ(静かなときの音量の移動平均)。
    noise: f32,
}

impl Segmenter {
    /// これだけ黙ったら区切る(秒)。
    pub const SILENCE: f32 = 0.7;
    /// 1 つの長さの上限(秒)。Whisper は 30 秒まで読める。
    pub const MAX: f32 = 20.0;
    /// これより短い発話は捨てる(秒)。
    pub const MIN_VOICED: f32 = 0.3;
    const PRE: f32 = 0.3;
    const FRAME: usize = 480;

    pub fn new(rate: f32) -> Self {
        Self {
            rate,
            buf: Vec::new(),
            pre: Vec::new(),
            speaking: false,
            silent_for: 0.0,
            voiced: 0.0,
            noise: 0.003,
        }
    }

    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// 音を足す。区切れたかたまりを返す。
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        for frame in samples.chunks(Self::FRAME) {
            let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len() as f32).sqrt();
            let dt = frame.len() as f32 / self.rate;
            let loud = rms > (self.noise * 3.0).max(0.01);
            if !loud {
                self.noise = self.noise * 0.98 + rms * 0.02;
            }
            if self.speaking {
                self.buf.extend_from_slice(frame);
                if loud {
                    self.silent_for = 0.0;
                    self.voiced += dt;
                } else {
                    self.silent_for += dt;
                }
                let len = self.buf.len() as f32 / self.rate;
                if (self.silent_for >= Self::SILENCE || len >= Self::MAX)
                    && let Some(seg) = self.finish()
                {
                    out.push(seg);
                }
            } else {
                self.pre.extend_from_slice(frame);
                let keep = (Self::PRE * self.rate) as usize;
                if self.pre.len() > keep {
                    self.pre.drain(..self.pre.len() - keep);
                }
                if loud {
                    self.speaking = true;
                    self.silent_for = 0.0;
                    self.voiced = dt;
                    self.buf = std::mem::take(&mut self.pre);
                }
            }
        }
        out
    }

    /// 話している途中のかたまりを出す(録音を止めたとき)。
    pub fn flush(&mut self) -> Option<Vec<f32>> {
        if self.speaking { self.finish() } else { None }
    }

    fn finish(&mut self) -> Option<Vec<f32>> {
        self.speaking = false;
        self.silent_for = 0.0;
        let voiced = std::mem::replace(&mut self.voiced, 0.0);
        let seg = std::mem::take(&mut self.buf);
        (voiced >= Self::MIN_VOICED).then_some(seg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, rate: f32, amp: f32) -> Vec<f32> {
        (0..(secs * rate) as usize)
            .map(|i| amp * (i as f32 * 0.07).sin())
            .collect()
    }

    #[test]
    fn splits_on_silence() {
        let rate = 16_000.0;
        let mut s = Segmenter::new(rate);
        let mut audio = tone(0.5, rate, 0.0);
        audio.extend(tone(1.0, rate, 0.3));
        audio.extend(tone(1.0, rate, 0.0));
        audio.extend(tone(0.8, rate, 0.3));
        audio.extend(tone(1.0, rate, 0.0));
        let segs = s.push(&audio);
        assert_eq!(segs.len(), 2);
        // 言い始めの少し前から入る。
        assert!(segs[0].len() as f32 / rate > 1.0);
        assert!(s.flush().is_none());
    }

    #[test]
    fn drops_clicks_and_caps_length() {
        let rate = 16_000.0;
        let mut s = Segmenter::new(rate);
        let mut audio = tone(0.1, rate, 0.5);
        audio.extend(tone(1.0, rate, 0.0));
        assert!(s.push(&audio).is_empty());
        let long = tone(Segmenter::MAX + 2.0, rate, 0.3);
        let segs = s.push(&long);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].len() as f32 / rate <= Segmenter::MAX + 0.1);
        assert!(s.flush().is_some());
    }

    #[test]
    fn model_urls_follow_template() {
        let files = model_files("/models/whisper-{model}", Model::Tiny);
        assert_eq!(files[0], "/models/whisper-tiny/config.json");
        assert_eq!(files[2], "/models/whisper-tiny/model.safetensors");
        assert!(
            model_files(DEFAULT_MODEL_URL, Model::Base)[1]
                .ends_with("whisper-base/resolve/main/tokenizer.json")
        );
    }
}
