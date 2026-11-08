//! 生成 AI で図を作る。
//!
//! `POST /api/boards/{id}/ai/diagram?key=` に `{ "prompt": "…", "mode": "auto" | "aws" | "azure" | "gcp" | "flowchart" }`
//! を送ると、Claude が `board_core::diagram::DiagramSpec`(グループ・ノード・矢印の JSON)を返す。
//! レイアウトと要素への変換はクライアントが行う(自分の操作として Undo でき、そのまま同期される)。
//! Claude API の呼び出しと呼びすぎの制限は付箋の AI 機能([`crate::ai`])と共通。
//! API の利用料がかかるので、使えるのはそのボードの編集権限がある人だけ。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use board_core::diagram::{self, DiagramSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ai::AiError;
use crate::{AppState, api};

/// 指示文の上限(文字数)。
pub const MAX_PROMPT_CHARS: usize = 4000;
/// 図の大きさの上限。
const MAX_NODES: usize = 120;

/// 何の図を作るか(指示文の補足)。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// 内容から判断する。
    #[default]
    Auto,
    Aws,
    Azure,
    Gcp,
    Flowchart,
}

const SYSTEM_PROMPT: &str = "\
You turn a user's description into a diagram for a collaborative whiteboard. \
The user usually writes in Japanese; write every label in the user's language, \
except product names such as AWS service names.

Output only the diagram object. The whiteboard lays it out automatically, so give \
structure, not coordinates:
- groups: nested frames. Use `parent` to nest (empty string for top level). For AWS \
architecture, nest like aws_cloud > region > vpc > availability_zone > public_subnet / \
private_subnet. For Azure: azure_subscription > azure_region > azure_resource_group > \
azure_vnet > azure_subnet. For Google Cloud: gcp_project > gcp_region > gcp_vpc > \
gcp_subnet (gcp_zone inside a region when zones matter). Use `generic` for any other \
kind of grouping (team, phase, system).
- nodes: one per component or step. `group` is the id of the innermost group that \
contains it (empty string if none). `kind` must be one of the listed kinds; pick the \
specific cloud service when one fits (AWS kinds have no prefix, Azure kinds start \
with azure_, Google Cloud kinds with gcp_; stay within the provider the user names), `users`/`client`/`internet` for actors outside AWS, \
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
        Mode::Azure => "\n\n(Draw this as a Microsoft Azure architecture diagram.)",
        Mode::Gcp => "\n\n(Draw this as a Google Cloud architecture diagram.)",
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

/// Claude に渡す本文。
pub fn user_message(prompt: &str, mode: Mode) -> String {
    format!("{}{}", prompt.trim(), mode_hint(mode))
}

/// 構造化出力の JSON を図にする。空文字の所属は「なし」として扱う。
pub fn to_spec(v: &Value) -> Result<DiagramSpec, AiError> {
    let mut spec = DiagramSpec::from_json(&v.to_string()).map_err(AiError::Upstream)?;
    if spec.nodes.is_empty() {
        return Err(AiError::Upstream("図の要素が生成されませんでした".into()));
    }
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

#[derive(Debug, Deserialize)]
pub struct DiagramRequest {
    pub prompt: String,
    #[serde(default)]
    pub mode: Mode,
}

pub async fn generate(
    State(state): State<Arc<AppState>>,
    Path(room): Path<String>,
    Query(q): Query<api::KeyQuery>,
    headers: HeaderMap,
    Json(req): Json<DiagramRequest>,
) -> Result<Json<DiagramSpec>, AiError> {
    let ai = state.config.ai.clone().ok_or(AiError::Disabled)?;
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(AiError::Api(api::Error::bad_request(
            "作りたい図の説明を入力してください",
        )));
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(AiError::TooMany(format!(
            "説明は {MAX_PROMPT_CHARS} 文字以内にしてください"
        )));
    }
    let user = api::current_user(&state, &headers).map_err(AiError::Api)?;
    let (_, role) = api::board_access(
        &state,
        &room,
        user.as_ref().map(|u| u.login.as_str()),
        q.key.as_deref(),
    )
    .map_err(AiError::Api)?;
    if !role.can_edit() {
        return Err(AiError::Forbidden);
    }
    if !ai.try_acquire(&room) {
        return Err(AiError::Busy);
    }
    crate::billing::charge_ai(&state, &headers, &room).map_err(AiError::Api)?;
    let out = ai
        .call(SYSTEM_PROMPT, user_message(prompt, req.mode), schema())
        .await?;
    let spec = to_spec(&out)?;
    tracing::info!(
        room = %room,
        nodes = spec.nodes.len(),
        edges = spec.edges.len(),
        "AI で図を生成"
    );
    Ok(Json(spec))
}

/// AI 機能が使えるか(画面の案内用)。
pub async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "enabled": state.config.ai.is_some(),
        // ブラウザ内 AI のモデルの取得元(指定がなければクライアントの既定)。
        "local_model_url": state.config.local_ai.client_url(),
        "local_encoder_url": state.config.local_ai.client_encoder_url(),
    }))
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
    fn mode_adds_hint() {
        assert_eq!(user_message(" VPC ", Mode::Auto), "VPC");
        assert!(user_message("x", Mode::Aws).contains("AWS"));
        assert!(user_message("x", Mode::Azure).contains("Azure"));
        assert!(user_message("x", Mode::Gcp).contains("Google Cloud"));
        assert!(user_message("x", Mode::Flowchart).contains("flowchart"));
    }

    #[test]
    fn spec_normalizes_empty_refs() {
        let v = json!({"title":"t","direction":"right",
            "groups":[{"id":"v","kind":"vpc","label":"","parent":""}],
            "nodes":[{"id":"a","kind":"ec2","label":"web","group":"v"},{"id":"b","kind":"s3","label":"","group":""}],
            "edges":[{"from":"a","to":"b","label":""}]});
        let s = to_spec(&v).unwrap();
        assert_eq!(s.groups[0].parent, None);
        assert_eq!(s.nodes[0].group.as_deref(), Some("v"));
        assert_eq!(s.nodes[1].group, None);
        assert!(to_spec(&json!({"nodes": []})).is_err());
    }
}
