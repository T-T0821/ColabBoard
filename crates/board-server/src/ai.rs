//! 生成 AI(Claude API)で図を作る。
//!
//! `POST /api/ai/diagram` に文章を送ると、Claude が `board_core::diagram::DiagramSpec`
//! (グループ・ノード・矢印の JSON)を返す。レイアウトと要素への変換はクライアントが行う
//! (自分の操作として Undo でき、そのまま同期される)。
//!
//! API キーは環境変数 `ANTHROPIC_API_KEY` で渡す(なければこの機能は無効)。

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use board_core::api::Role;
use board_core::diagram::{self, DiagramSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::AppState;
use crate::api::{Error, board_access, current_user};

/// 既定のモデル。
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2026-10-21";
/// 安全分類器で断られたとき、サーバー側で別モデルに回してもらう。
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// 指示文の上限(文字数)。
const MAX_PROMPT_CHARS: usize = 4000;
/// 同時に投げる生成の上限(API の使いすぎを防ぐ)。
const MAX_CONCURRENT: usize = 4;
/// 図の大きさの上限。
const MAX_NODES: usize = 120;

/// Claude API の接続設定。
pub struct Ai {
    api_key: String,
    base_url: String,
    model: String,
    http: reqwest::Client,
    permits: Semaphore,
}

impl Ai {
    pub fn new(api_key: String, base_url: Option<String>, model: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .expect("HTTP クライアント");
        Self {
            api_key,
            base_url: base_url
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.into())
                .trim_end_matches('/')
                .to_string(),
            model: model
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_MODEL.into()),
            http,
            permits: Semaphore::new(MAX_CONCURRENT),
        }
    }

    /// 環境変数 `ANTHROPIC_API_KEY`・`ANTHROPIC_BASE_URL`・`CLAUDE_MODEL` から作る。
    /// キーがなければ `None`(AI 機能は無効)。
    pub fn from_env() -> Option<Arc<Self>> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())?;
        Some(Arc::new(Self::new(
            key,
            std::env::var("ANTHROPIC_BASE_URL").ok(),
            std::env::var("CLAUDE_MODEL").ok(),
        )))
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// 文章から図を作る。
    pub async fn diagram(&self, prompt: &str, mode: Mode) -> Result<DiagramSpec, AiError> {
        let _permit = self.permits.try_acquire().map_err(|_| AiError::Busy)?;
        let body = request_body(&self.model, prompt, mode);
        let res = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", FALLBACK_BETA)
            .json(&body)
            .send()
            .await
            .map_err(|e| AiError::Upstream(format!("Claude API に接続できません: {e}")))?;
        let status = res.status();
        let value: Value = res
            .json()
            .await
            .map_err(|e| AiError::Upstream(format!("Claude API の応答を読めません: {e}")))?;
        if !status.is_success() {
            let msg = value["error"]["message"].as_str().unwrap_or("不明なエラー");
            tracing::warn!("Claude API エラー {status}: {msg}");
            return Err(match status.as_u16() {
                429 | 529 => AiError::Busy,
                _ => AiError::Upstream(format!("Claude API エラー({status}): {msg}")),
            });
        }
        parse_response(&value)
    }
}

/// 何の図を作るか(指示文の補足)。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// 内容から判断する。
    #[default]
    Auto,
    Aws,
    Flowchart,
}

#[derive(Debug)]
pub enum AiError {
    /// 同時実行の上限、または API のレート制限。
    Busy,
    /// 安全上の理由で断られた。
    Refused,
    /// 応答が図として読めない。
    Invalid(String),
    Upstream(String),
}

impl From<AiError> for Error {
    fn from(e: AiError) -> Self {
        match e {
            AiError::Busy => Error::new(
                StatusCode::TOO_MANY_REQUESTS,
                "生成 AI が混み合っています。少し待ってからもう一度試してください",
            ),
            AiError::Refused => Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "この内容では図を作れませんでした。指示を変えて試してください",
            ),
            AiError::Invalid(m) => Error::new(StatusCode::BAD_GATEWAY, m),
            AiError::Upstream(m) => Error::new(StatusCode::BAD_GATEWAY, m),
        }
    }
}

const SYSTEM_PROMPT: &str = "\
You turn a user's description into a diagram for a collaborative whiteboard. \
The user usually writes in Japanese; write every label in the user's language, \
except product names such as AWS service names.

Output only the diagram object. The whiteboard lays it out automatically, so give \
structure, not coordinates:
- groups: nested frames. Use `parent` to nest (empty string for top level). For AWS \
architecture, nest like aws_cloud > region > vpc > availability_zone > public_subnet / \
private_subnet. Use `generic` for any other kind of grouping (team, phase, system).
- nodes: one per component or step. `group` is the id of the innermost group that \
contains it (empty string if none). `kind` must be one of the listed kinds; pick the \
specific AWS service when one fits, `users`/`client`/`internet` for actors outside AWS, \
and process/decision/terminal/data/note for flowcharts and general diagrams. \
`label` is a short name (about 2-12 characters): for an AWS service do not repeat the \
service name, describe its role instead (e.g. 'Web サーバー'), or leave it empty.
- edges: the main flow of requests or data, from upstream to downstream. Keep labels \
short or empty. For a decision node, label each outgoing edge (e.g. 'はい' / 'いいえ').
- direction: 'right' for architecture and data-flow diagrams, 'down' for flowcharts \
and procedures.

List nodes in reading order (entry point first). Keep the diagram focused: usually \
5-25 nodes, never more than 60. Use short ascii ids such as 'alb' or 'step3'. \
When the request is vague, choose a sensible, commonly used design rather than asking.";

fn mode_hint(mode: Mode) -> &'static str {
    match mode {
        Mode::Auto => "",
        Mode::Aws => "\n\n(Draw this as an AWS architecture diagram.)",
        Mode::Flowchart => "\n\n(Draw this as a flowchart, direction 'down'.)",
    }
}

/// 出力の JSON Schema(構造化出力)。種類は一覧の中からだけ選ばせる。
pub fn schema() -> Value {
    let node_kinds: Vec<&str> = diagram::NODE_KINDS.iter().map(|k| k.id).collect();
    let group_kinds: Vec<&str> = diagram::GROUP_KINDS.iter().map(|k| k.id).collect();
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "direction", "groups", "nodes", "edges"],
        "properties": {
            "title": {"type": "string"},
            "direction": {"type": "string", "enum": ["right", "down"]},
            "groups": {"type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "kind", "label", "parent"],
                "properties": {
                    "id": {"type": "string"},
                    "kind": {"type": "string", "enum": group_kinds},
                    "label": {"type": "string"},
                    "parent": {"type": "string"}
                }
            }},
            "nodes": {"type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "kind", "label", "group"],
                "properties": {
                    "id": {"type": "string"},
                    "kind": {"type": "string", "enum": node_kinds},
                    "label": {"type": "string"},
                    "group": {"type": "string"}
                }
            }},
            "edges": {"type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["from", "to", "label"],
                "properties": {
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                    "label": {"type": "string"}
                }
            }}
        }
    })
}

pub fn request_body(model: &str, prompt: &str, mode: Mode) -> Value {
    json!({
        "model": model,
        "max_tokens": 16000,
        "fallbacks": "default",
        "output_config": {
            "effort": "medium",
            "format": {"type": "json_schema", "schema": schema()}
        },
        "system": SYSTEM_PROMPT,
        "messages": [
            {"role": "user", "content": format!("{}{}", prompt.trim(), mode_hint(mode))}
        ]
    })
}

/// Messages API の応答から図を取り出す。
pub fn parse_response(v: &Value) -> Result<DiagramSpec, AiError> {
    match v["stop_reason"].as_str() {
        Some("refusal") => return Err(AiError::Refused),
        Some("max_tokens") => {
            return Err(AiError::Invalid(
                "図が大きすぎて途中で切れました。範囲を絞って試してください".into(),
            ));
        }
        _ => {}
    }
    let text = v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<String>();
    let mut spec = DiagramSpec::from_json(text.trim())
        .map_err(|e| AiError::Invalid(format!("生成された図を読めませんでした: {e}")))?;
    if spec.nodes.is_empty() {
        return Err(AiError::Invalid("図の要素が生成されませんでした".into()));
    }
    // 空文字は「なし」として扱う。
    for g in &mut spec.groups {
        if g.parent.as_deref() == Some("") {
            g.parent = None;
        }
    }
    for n in &mut spec.nodes {
        if n.group.as_deref() == Some("") {
            n.group = None;
        }
    }
    spec.nodes.truncate(MAX_NODES);
    Ok(spec)
}

// ---- HTTP ----

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/status", get(status))
        .route("/diagram", post(generate_diagram))
}

#[derive(Serialize)]
struct Status {
    enabled: bool,
    model: Option<String>,
}

async fn status(State(state): State<Arc<AppState>>) -> Json<Status> {
    let ai = state.config.ai.as_ref();
    Json(Status {
        enabled: ai.is_some(),
        model: ai.map(|a| a.model().to_string()),
    })
}

#[derive(Deserialize)]
pub struct DiagramRequest {
    /// 図を置くボード(編集権限が必要)。
    pub room: String,
    #[serde(default)]
    pub key: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub mode: Mode,
}

async fn generate_diagram(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<DiagramRequest>,
) -> Result<Json<DiagramSpec>, Error> {
    let ai = state.config.ai.clone().ok_or_else(|| {
        Error::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "生成 AI は無効です(サーバーに ANTHROPIC_API_KEY を設定してください)",
        )
    })?;
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(Error::bad_request("作りたい図の説明を入力してください"));
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(Error::bad_request(format!(
            "説明は {MAX_PROMPT_CHARS} 文字以内にしてください"
        )));
    }
    // そのボードを編集できる人だけが使える(API の利用料がかかるため)。
    let user = current_user(&state, &headers)?;
    let (_, role) = board_access(
        &state,
        &req.room,
        user.as_ref().map(|u| u.login.as_str()),
        req.key.as_deref(),
    )?;
    if role < Role::Editor {
        return Err(Error::forbidden());
    }
    let spec = ai.diagram(prompt, req.mode).await?;
    tracing::info!(
        "AI で図を生成: {} ノード, {} 矢印",
        spec.nodes.len(),
        spec.edges.len()
    );
    Ok(Json(spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_lists_every_kind() {
        let s = schema();
        let kinds = s["properties"]["nodes"]["items"]["properties"]["kind"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(kinds.len(), diagram::NODE_KINDS.len());
        assert!(kinds.iter().any(|k| k == "lambda"));
    }

    #[test]
    fn request_uses_structured_output_and_fallback() {
        let b = request_body(DEFAULT_MODEL, " VPC に EC2 ", Mode::Aws);
        assert_eq!(b["model"], DEFAULT_MODEL);
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        let content = b["messages"][0]["content"].as_str().unwrap();
        assert!(content.starts_with("VPC に EC2"));
        assert!(content.contains("AWS"));
        assert!(b.get("temperature").is_none());
    }

    #[test]
    fn parse_text_block_and_normalize_empty_refs() {
        let spec = r#"{"title":"t","direction":"right","groups":[{"id":"v","kind":"vpc","label":"","parent":""}],
            "nodes":[{"id":"a","kind":"ec2","label":"web","group":"v"},{"id":"b","kind":"s3","label":"","group":""}],
            "edges":[{"from":"a","to":"b","label":""}]}"#;
        let v = json!({
            "stop_reason": "end_turn",
            "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": spec}]
        });
        let s = parse_response(&v).unwrap();
        assert_eq!(s.groups[0].parent, None);
        assert_eq!(s.nodes[0].group.as_deref(), Some("v"));
        assert_eq!(s.nodes[1].group, None);
    }

    #[test]
    fn parse_errors() {
        assert!(matches!(
            parse_response(&json!({"stop_reason": "refusal", "content": []})),
            Err(AiError::Refused)
        ));
        assert!(matches!(
            parse_response(
                &json!({"stop_reason": "end_turn", "content": [{"type":"text","text":"oops"}]})
            ),
            Err(AiError::Invalid(_))
        ));
        assert!(matches!(
            parse_response(
                &json!({"stop_reason": "end_turn", "content": [{"type":"text","text":"{\"nodes\":[]}"}]})
            ),
            Err(AiError::Invalid(_))
        ));
    }
}
