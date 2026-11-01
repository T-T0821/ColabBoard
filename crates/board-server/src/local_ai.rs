//! ブラウザ内 AI(音声認識のモデル)の取得元。
//!
//! 音声認識(Whisper)はクライアントのブラウザで動き、音声はサーバーに届かない。サーバーが決めるのは
//! モデルのファイル(`config.json`・`tokenizer.json`・`model.safetensors`)をどこから取るかだけ。
//!
//! - `LOCAL_AI_MODEL_URL`: 取得元の URL。`{model}` が `tiny` / `base` に置き換わる。
//!   既定は Hugging Face(`https://huggingface.co/openai/whisper-{model}/resolve/main/`)。
//! - `LOCAL_AI_MODELS_DIR`: モデルを置いたディレクトリ。`/models/` で配る(社外に出られない環境向け)。
//!   `whisper-tiny/`・`whisper-base/` の下に 3 つのファイルを置く。`LOCAL_AI_MODEL_URL` がなければ
//!   自動で `/models/whisper-{model}/` を使う。
//!
//! クライアントは `GET /api/ai/status` の `local_model_url` で知る(なければ既定の取得元)。

use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct LocalAiConfig {
    pub model_url: Option<String>,
    pub models_dir: Option<PathBuf>,
}

/// `LOCAL_AI_MODELS_DIR` を配るときの URL。
pub const MODELS_PATH: &str = "/models";

impl LocalAiConfig {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self {
            model_url: var("LOCAL_AI_MODEL_URL").map(|v| v.trim().to_string()),
            models_dir: var("LOCAL_AI_MODELS_DIR").map(PathBuf::from),
        }
    }

    /// クライアントに知らせる取得元(`{model}` を含む)。指定がなければ `None`(クライアントの既定)。
    pub fn client_url(&self) -> Option<String> {
        self.model_url.clone().or_else(|| {
            self.models_dir
                .as_ref()
                .map(|_| format!("{MODELS_PATH}/whisper-{{model}}/"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_implies_local_url() {
        assert_eq!(LocalAiConfig::default().client_url(), None);
        let c = LocalAiConfig {
            model_url: None,
            models_dir: Some("/srv/models".into()),
        };
        assert_eq!(c.client_url().as_deref(), Some("/models/whisper-{model}/"));
        let c = LocalAiConfig {
            model_url: Some("https://cdn.example.com/w-{model}/".into()),
            models_dir: Some("/srv/models".into()),
        };
        assert_eq!(
            c.client_url().as_deref(),
            Some("https://cdn.example.com/w-{model}/")
        );
    }
}
