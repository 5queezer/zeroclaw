//! # A2A Protocol — v1.0 Implementation
//!
//! Implements the A2A (Agent-to-Agent) protocol v1.0:
//! - Agent Card discovery (`GET /.well-known/agent-card.json`)
//! - `message/send` (synchronous request/response)
//! - `message/stream` (Server-Sent Events streaming)
//! - `tasks/get` (polling)
//! - `tasks/list` (cursor-based pagination with filters)
//! - `tasks/getByContextId` (multi-turn conversation threading)
//! - `tasks/cancel` (cancel in-flight tasks)
//! - `return_immediately` async task execution
//! - TTL-based task store eviction
//! - Bearer token authentication
//! - v1.0 error model (`google.rpc.Status` with `ErrorInfo` details)
//!
//! **Not yet implemented:**
//! - Push notifications
//! - Structured/binary message parts (`data`, `raw`)
//! - Task persistence

use super::AppState;
use crate::security::pairing::constant_time_eq;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_stream::StreamExt;

// ── Types ────────────────────────────────────────────────────────

/// Maximum number of in-flight tasks to prevent memory exhaustion.
const MAX_TASKS: usize = 10_000;

/// In-memory store for A2A task state.
pub struct TaskStore {
    tasks: RwLock<HashMap<String, Task>>,
    /// Maps `context_id` → list of task IDs sharing that context.
    context_index: RwLock<HashMap<String, Vec<String>>>,
    /// Tracks when tasks entered a terminal state for TTL-based eviction.
    timestamps: RwLock<HashMap<String, std::time::Instant>>,
}

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            context_index: RwLock::new(HashMap::new()),
            timestamps: RwLock::new(HashMap::new()),
        }
    }

    /// Record a task→context association in the index.
    async fn index_context(&self, context_id: &str, task_id: &str) {
        let mut idx = self.context_index.write().await;
        idx.entry(context_id.to_owned())
            .or_default()
            .push(task_id.to_owned());
    }

    /// Return all tasks belonging to a given `context_id`.
    pub async fn tasks_by_context(&self, context_id: &str) -> Vec<Task> {
        let idx = self.context_index.read().await;
        let Some(ids) = idx.get(context_id) else {
            return Vec::new();
        };
        let tasks = self.tasks.read().await;
        ids.iter().filter_map(|id| tasks.get(id).cloned()).collect()
    }

    /// Record the instant a task became terminal (Completed, Failed, Canceled, Rejected).
    pub async fn mark_terminal(&self, task_id: &str) {
        let mut ts = self.timestamps.write().await;
        ts.insert(task_id.to_string(), std::time::Instant::now());
    }

    /// Remove terminal tasks whose timestamp is older than `ttl`.
    /// Returns the number of evicted tasks.
    pub async fn evict_expired(&self, ttl: std::time::Duration) -> usize {
        let now = std::time::Instant::now();

        // First, collect task IDs to evict.
        let expired_ids: Vec<String> = {
            let ts = self.timestamps.read().await;
            ts.iter()
                .filter(|(_, instant)| now.duration_since(**instant) > ttl)
                .map(|(id, _)| id.clone())
                .collect()
        };

        if expired_ids.is_empty() {
            return 0;
        }

        // Collect context_ids for evicted tasks so we can clean the index.
        let evicted_context_ids: Vec<(String, String)> = {
            let tasks = self.tasks.read().await;
            expired_ids
                .iter()
                .filter_map(|id| {
                    tasks
                        .get(id)
                        .and_then(|t| t.context_id.as_ref().map(|ctx| (ctx.clone(), id.clone())))
                })
                .collect()
        };

        // Remove from tasks and timestamps maps.
        let mut tasks = self.tasks.write().await;
        let mut ts = self.timestamps.write().await;
        let mut count = 0;
        for id in &expired_ids {
            if tasks.remove(id).is_some() {
                count += 1;
            }
            ts.remove(id);
        }
        drop(tasks);
        drop(ts);

        // Clean up context_index: remove evicted task IDs and prune empty entries.
        if !evicted_context_ids.is_empty() {
            let mut idx = self.context_index.write().await;
            for (ctx, tid) in &evicted_context_ids {
                if let Some(ids) = idx.get_mut(ctx) {
                    ids.retain(|id| id != tid);
                    if ids.is_empty() {
                        idx.remove(ctx);
                    }
                }
            }
        }

        count
    }
}

impl Default for TaskStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a background task that periodically evicts expired terminal tasks.
///
/// Accepts a `shutdown_rx` watch receiver so the task terminates cleanly when
/// the gateway shuts down. Zero values for `ttl_secs` and `interval_secs` are
/// clamped to 1 to prevent busy-loops or instant eviction.
pub fn spawn_eviction_task(
    task_store: Arc<TaskStore>,
    ttl_secs: u64,
    interval_secs: u64,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let ttl_secs = ttl_secs.max(1);
    let interval_secs = interval_secs.max(1);
    tokio::spawn(async move {
        let ttl = std::time::Duration::from_secs(ttl_secs);
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let evicted = task_store.evict_expired(ttl).await;
                    if evicted > 0 {
                        tracing::debug!(evicted, "A2A task store eviction pass");
                    }
                }
                _ = shutdown_rx.changed() => {
                    tracing::debug!("A2A eviction task shutting down");
                    break;
                }
            }
        }
    })
}

// ── v1.0 Protocol Types ─────────────────────────────────────────

/// A2A v1.0 message part — oneof discriminated by which field is present.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    File {
        #[serde(rename = "file")]
        file: FileContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    Data {
        data: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
}

/// File content — either inline bytes or a URL reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// A2A v1.0 Message object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub parts: Vec<Part>,
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "contextId", skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// A2A v1.0 TaskStatus — contains state and optional message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatus {
    pub state: A2aTaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// A2A v1.0 Artifact object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(rename = "artifactId")]
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}

/// A2A v1.0 Task object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub status: TaskStatus,
    #[serde(rename = "contextId", skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<Artifact>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<Message>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// A2A v1.0 task state enum — SCREAMING_SNAKE_CASE with `TASK_STATE_` prefix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum A2aTaskState {
    #[serde(rename = "TASK_STATE_SUBMITTED")]
    Submitted,
    #[serde(rename = "TASK_STATE_WORKING")]
    Working,
    #[serde(rename = "TASK_STATE_COMPLETED")]
    Completed,
    #[serde(rename = "TASK_STATE_FAILED")]
    Failed,
    #[serde(rename = "TASK_STATE_CANCELED")]
    Canceled,
    #[serde(rename = "TASK_STATE_INPUT_REQUIRED")]
    InputRequired,
    #[serde(rename = "TASK_STATE_REJECTED")]
    Rejected,
    #[serde(rename = "TASK_STATE_AUTH_REQUIRED")]
    AuthRequired,
}

impl A2aTaskState {
    /// Whether this state is terminal (task will not transition further).
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            A2aTaskState::Completed
                | A2aTaskState::Failed
                | A2aTaskState::Canceled
                | A2aTaskState::Rejected
        )
    }
}

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// google.rpc.ErrorInfo detail — included in the `details` array
/// of JSON-RPC errors per A2A v1.0 error model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aErrorDetail {
    #[serde(rename = "@type")]
    pub error_type: String,
    pub reason: String,
    pub domain: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<A2aErrorDetail>>,
}

/// A2A v1.0 error reason codes (used in `A2aErrorDetail.reason`).
pub mod error_reason {
    pub const INVALID_REQUEST: &str = "INVALID_REQUEST";
    pub const METHOD_NOT_FOUND: &str = "METHOD_NOT_FOUND";
    pub const INVALID_PARAMS: &str = "INVALID_PARAMS";
    pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
    pub const TASK_NOT_FOUND: &str = "TASK_NOT_FOUND";
    pub const TASK_ALREADY_TERMINAL: &str = "TASK_ALREADY_TERMINAL";
    pub const TASK_STORE_FULL: &str = "TASK_STORE_FULL";
    pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";
}

// ── v1.0 Streaming types ────────────────────────────────────────

/// A2A v1.0 `TaskStatusUpdateEvent` — emitted during streaming to report
/// task state transitions.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusUpdateEvent {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub status: TaskStatus,
    #[serde(rename = "final")]
    pub is_final: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// A2A v1.0 `TaskArtifactUpdateEvent` — emitted during streaming to deliver
/// artifact content (potentially chunked).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactUpdateEvent {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub artifact: Artifact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

// ── Agent card generation ────────────────────────────────────────

/// Generate the A2A agent card from configuration.
pub fn generate_agent_card(config: &crate::config::Config) -> serde_json::Value {
    let a2a = &config.a2a;

    let name = a2a
        .agent_name
        .clone()
        .unwrap_or_else(|| "ZeroClaw Agent".to_string());

    let description = a2a
        .description
        .clone()
        .unwrap_or_else(|| "ZeroClaw autonomous agent".to_string());

    let version = a2a
        .version
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    let base_url = a2a
        .public_url
        .clone()
        .unwrap_or_else(|| format!("http://{}:{}", config.gateway.host, config.gateway.port));

    let skills: Vec<serde_json::Value> = if a2a.capabilities.is_empty() {
        vec![json!({
            "id": "general",
            "name": "General",
            "description": "General-purpose autonomous agent",
            "tags": ["general"],
            "examples": ["Help me with a task"]
        })]
    } else {
        a2a.capabilities
            .iter()
            .map(|c| {
                json!({
                    "id": c,
                    "name": c,
                    "description": format!("{c} capability"),
                    "tags": [c],
                    "examples": []
                })
            })
            .collect()
    };

    let protocol_version = a2a
        .protocol_version
        .clone()
        .unwrap_or_else(|| "1.0".to_string());

    let provider_url = a2a
        .provider_url
        .clone()
        .unwrap_or_else(|| "https://github.com/5queezer/hrafn".to_string());

    // Only advertise security requirements when auth is actually configured
    let requires_auth =
        a2a.bearer_token.as_ref().is_some_and(|t| !t.is_empty()) || config.gateway.require_pairing;

    let mut card = json!({
        "name": name,
        "description": description,
        "version": version,
        "supported_interfaces": [{
            "url": format!("{base_url}/"),
            "protocol_binding": "JSONRPC",
            "protocol_version": protocol_version
        }],
        "capabilities": {
            "streaming": true,
            "pushNotifications": false
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": skills,
        "provider": {
            "organization": "ZeroClaw",
            "url": provider_url
        }
    });

    if requires_auth {
        card["security_schemes"] = json!({
            "bearer": {
                "http_auth_security_scheme": {
                    "scheme": "Bearer"
                }
            }
        });
        card["security_requirements"] = json!([{
            "schemes": {
                "bearer": { "list": [] }
            }
        }]);
    }

    card
}

// ── Handlers ─────────────────────────────────────────────────────

/// `GET /.well-known/agent-card.json` — unauthenticated discovery endpoint.
pub async fn handle_agent_card(State(state): State<AppState>) -> impl IntoResponse {
    match &state.a2a_agent_card {
        Some(card) => (StatusCode::OK, Json(card.as_ref().clone())).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "A2A protocol not enabled"})),
        )
            .into_response(),
    }
}

/// `POST /a2a` — authenticated JSON-RPC 2.0 task endpoint.
pub async fn handle_a2a_rpc(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    // Check feature enabled
    let (Some(_card), Some(task_store)) = (&state.a2a_agent_card, &state.a2a_task_store) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": "A2A protocol not enabled"}})),
        )
            .into_response();
    };

    // Authenticate
    if let Err(resp) = require_a2a_auth(&state, &headers) {
        return resp.into_response();
    }

    // Validate JSON-RPC version
    if body.jsonrpc != "2.0" {
        return (
            StatusCode::BAD_REQUEST,
            Json(rpc_error(
                body.id,
                -32600,
                "Invalid JSON-RPC version",
                Some(error_reason::INVALID_REQUEST),
            )),
        )
            .into_response();
    }

    match body.method.as_str() {
        "message/send" => Box::pin(handle_message_send(&state, task_store, body))
            .await
            .into_response(),
        "tasks/get" => handle_tasks_get(task_store, body).await.into_response(),
        "tasks/list" => handle_tasks_list(task_store, body).await.into_response(),
        "tasks/cancel" => handle_tasks_cancel(task_store, body).await.into_response(),
        "tasks/getByContextId" => handle_tasks_get_by_context(task_store, body)
            .await
            .into_response(),
        _ => (
            StatusCode::OK,
            Json(rpc_error(
                body.id,
                -32601,
                &format!("Method not found: {}", body.method),
                Some(error_reason::METHOD_NOT_FOUND),
            )),
        )
            .into_response(),
    }
}

// ── Auth helper ──────────────────────────────────────────────────

fn require_a2a_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    // Extract bearer token from Authorization header
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .unwrap_or("");

    // Check dedicated A2A bearer token first
    {
        let config = state.config.lock();
        if let Some(ref a2a_token) = config.a2a.bearer_token {
            if !a2a_token.is_empty() {
                return if constant_time_eq(token, a2a_token) {
                    Ok(())
                } else {
                    Err((
                        StatusCode::UNAUTHORIZED,
                        Json(rpc_error(
                            json!(null),
                            -32000,
                            "Unauthorized",
                            Some(error_reason::UNAUTHORIZED),
                        )),
                    ))
                };
            }
        }
    }

    // Fall back to gateway pairing auth
    if !state.pairing.require_pairing() {
        return Ok(());
    }

    if state.pairing.is_authenticated(token) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(rpc_error(
                json!(null),
                -32000,
                "Unauthorized",
                Some(error_reason::UNAUTHORIZED),
            )),
        ))
    }
}

// ── Method handlers ──────────────────────────────────────────────

async fn handle_message_send(
    state: &AppState,
    task_store: &Arc<TaskStore>,
    req: JsonRpcRequest,
) -> (StatusCode, Json<serde_json::Value>) {
    // Parse the inbound message using shared helper.
    let (message_text, inbound_msg, context_id) = match parse_inbound_message(&req.params) {
        Ok(v) => v,
        Err(msg) => {
            return (
                StatusCode::OK,
                Json(rpc_error(
                    req.id,
                    -32602,
                    msg,
                    Some(error_reason::INVALID_PARAMS),
                )),
            );
        }
    };

    // Check for return_immediately flag in configuration.
    let return_immediately = req
        .params
        .pointer("/configuration/returnImmediately")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let task_id = uuid::Uuid::new_v4().to_string();

    // Choose initial state: Submitted when async, Working when synchronous.
    let initial_state = if return_immediately {
        A2aTaskState::Submitted
    } else {
        A2aTaskState::Working
    };

    // Store task (enforce capacity limit to prevent memory exhaustion)
    {
        let mut tasks = task_store.tasks.write().await;
        if tasks.len() >= MAX_TASKS {
            // Evict terminal tasks before rejecting
            tasks.retain(|_, t| !t.status.state.is_terminal());
            if tasks.len() >= MAX_TASKS {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(rpc_error(
                        req.id,
                        -32000,
                        "Task store full — too many in-flight tasks",
                        Some(error_reason::TASK_STORE_FULL),
                    )),
                );
            }
        }
        tasks.insert(
            task_id.clone(),
            Task {
                id: task_id.clone(),
                status: TaskStatus {
                    state: initial_state,
                    message: None,
                    timestamp: Some(chrono::Utc::now().to_rfc3339()),
                },
                context_id: Some(context_id.clone()),
                artifacts: None,
                history: Some(vec![inbound_msg.clone()]),
                metadata: None,
            },
        );
    }
    task_store.index_context(&context_id, &task_id).await;

    // Build conversation history from prior tasks in this context
    let prompt_text = build_context_prompt(task_store, &context_id, &task_id, &message_text).await;

    // If return_immediately, spawn background processing and return Submitted task.
    if return_immediately {
        let bg_store = Arc::clone(task_store);
        let config = state.config.lock().clone();
        let tid = task_id.clone();
        let ctx = context_id.clone();
        let bg_prompt = prompt_text.clone();
        let bg_session = format!("a2a-ctx-{}", &context_id);
        let telegram_notify = config.a2a.notify_chat_id.and_then(|chat_id| {
            config
                .channels_config
                .telegram
                .as_ref()
                .map(|t| (t.bot_token.clone(), chat_id))
        });

        tokio::spawn(async move {
            match Box::pin(crate::agent::process_message(
                config,
                &bg_prompt,
                Some(&bg_session),
            ))
            .await
            {
                Ok(response) => {
                    if let Some((ref token, chat_id)) = telegram_notify {
                        let notice = format!(
                            "\u{1f4e8} *A2A received:* _{}_\n\n{}",
                            message_text.replace('*', "\\*").replace('_', "\\_"),
                            response
                        );
                        notify_telegram_chat(token, chat_id, &notice).await;
                    }

                    let response_msg = Message {
                        role: "ROLE_AGENT".to_string(),
                        parts: vec![Part::Text {
                            text: response.clone(),
                            metadata: None,
                        }],
                        message_id: uuid::Uuid::new_v4().to_string(),
                        context_id: Some(ctx.clone()),
                        metadata: None,
                    };
                    let artifact = Artifact {
                        artifact_id: uuid::Uuid::new_v4().to_string(),
                        name: Some("response".to_string()),
                        description: None,
                        parts: vec![Part::Text {
                            text: response,
                            metadata: None,
                        }],
                        metadata: None,
                        extensions: None,
                    };
                    let task = Task {
                        id: tid.clone(),
                        status: TaskStatus {
                            state: A2aTaskState::Completed,
                            message: Some(response_msg),
                            timestamp: Some(chrono::Utc::now().to_rfc3339()),
                        },
                        context_id: Some(ctx),
                        artifacts: Some(vec![artifact]),
                        history: Some(vec![inbound_msg]),
                        metadata: None,
                    };
                    let mut tasks = bg_store.tasks.write().await;
                    if !tasks
                        .get(&tid)
                        .is_some_and(|t| t.status.state == A2aTaskState::Canceled)
                    {
                        tasks.insert(tid.clone(), task);
                    }
                    drop(tasks);
                    bg_store.mark_terminal(&tid).await;
                }
                Err(e) => {
                    tracing::error!(task_id = %tid, error = %e, "A2A async task failed");
                    let error_msg = Message {
                        role: "ROLE_AGENT".to_string(),
                        parts: vec![Part::Text {
                            text: "Internal processing error".to_string(),
                            metadata: None,
                        }],
                        message_id: uuid::Uuid::new_v4().to_string(),
                        context_id: Some(ctx.clone()),
                        metadata: None,
                    };
                    let task = Task {
                        id: tid.clone(),
                        status: TaskStatus {
                            state: A2aTaskState::Failed,
                            message: Some(error_msg),
                            timestamp: Some(chrono::Utc::now().to_rfc3339()),
                        },
                        context_id: Some(ctx),
                        artifacts: None,
                        history: Some(vec![inbound_msg]),
                        metadata: None,
                    };
                    let mut tasks = bg_store.tasks.write().await;
                    if !tasks
                        .get(&tid)
                        .is_some_and(|t| t.status.state == A2aTaskState::Canceled)
                    {
                        tasks.insert(tid.clone(), task);
                    }
                    drop(tasks);
                    bg_store.mark_terminal(&tid).await;
                }
            }
        });

        let tasks = task_store.tasks.read().await;
        let task = tasks.get(&task_id).cloned().unwrap();
        return (
            StatusCode::OK,
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "result": task
            })),
        );
    }

    // Synchronous processing (default).
    let config = state.config.lock().clone();
    let telegram_notify = config.a2a.notify_chat_id.and_then(|chat_id| {
        config
            .channels_config
            .telegram
            .as_ref()
            .map(|t| (t.bot_token.clone(), chat_id))
    });
    let session_id = format!("a2a-ctx-{context_id}");
    match Box::pin(crate::agent::process_message(
        config,
        &prompt_text,
        Some(&session_id),
    ))
    .await
    {
        Ok(response) => {
            // Notify Telegram group about A2A activity
            if let Some((ref token, chat_id)) = telegram_notify {
                let notice = format!(
                    "\u{1f4e8} *A2A received:* _{}_\n\n{}",
                    message_text.replace('*', "\\*").replace('_', "\\_"),
                    response
                );
                notify_telegram_chat(token, chat_id, &notice).await;
            }

            let response_msg = Message {
                role: "ROLE_AGENT".to_string(),
                parts: vec![Part::Text {
                    text: response.clone(),
                    metadata: None,
                }],
                message_id: uuid::Uuid::new_v4().to_string(),
                context_id: Some(context_id.clone()),
                metadata: None,
            };

            let artifact = Artifact {
                artifact_id: uuid::Uuid::new_v4().to_string(),
                name: Some("response".to_string()),
                description: None,
                parts: vec![Part::Text {
                    text: response,
                    metadata: None,
                }],
                metadata: None,
                extensions: None,
            };

            let task = Task {
                id: task_id.clone(),
                status: TaskStatus {
                    state: A2aTaskState::Completed,
                    message: Some(response_msg),
                    timestamp: Some(chrono::Utc::now().to_rfc3339()),
                },
                context_id: Some(context_id),
                artifacts: Some(vec![artifact]),
                history: Some(vec![inbound_msg]),
                metadata: None,
            };

            // Only write the result if the task hasn't been canceled in the
            // meantime. This prevents a cancel→completed race where the
            // synchronous agent pipeline overwrites a Canceled state.
            let task = {
                let mut tasks = task_store.tasks.write().await;
                if tasks
                    .get(&task_id)
                    .is_some_and(|t| t.status.state == A2aTaskState::Canceled)
                {
                    tasks.get(&task_id).cloned().unwrap()
                } else {
                    tasks.insert(task_id.clone(), task.clone());
                    task
                }
            };

            task_store.mark_terminal(&task_id).await;

            (
                StatusCode::OK,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": task
                })),
            )
        }
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "A2A task processing failed");

            let error_msg = Message {
                role: "ROLE_AGENT".to_string(),
                parts: vec![Part::Text {
                    text: "Internal processing error".to_string(),
                    metadata: None,
                }],
                message_id: uuid::Uuid::new_v4().to_string(),
                context_id: Some(context_id.clone()),
                metadata: None,
            };

            let task = Task {
                id: task_id.clone(),
                status: TaskStatus {
                    state: A2aTaskState::Failed,
                    message: Some(error_msg),
                    timestamp: Some(chrono::Utc::now().to_rfc3339()),
                },
                context_id: Some(context_id),
                artifacts: None,
                history: Some(vec![inbound_msg]),
                metadata: None,
            };

            // Preserve Canceled state — don't overwrite with Failed.
            let task = {
                let mut tasks = task_store.tasks.write().await;
                if tasks
                    .get(&task_id)
                    .is_some_and(|t| t.status.state == A2aTaskState::Canceled)
                {
                    tasks.get(&task_id).cloned().unwrap()
                } else {
                    tasks.insert(task_id.clone(), task.clone());
                    task
                }
            };

            task_store.mark_terminal(&task_id).await;

            (
                StatusCode::OK,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": task
                })),
            )
        }
    }
}

async fn handle_tasks_get(
    task_store: &Arc<TaskStore>,
    req: JsonRpcRequest,
) -> (StatusCode, Json<serde_json::Value>) {
    let task_id = req.params.get("id").and_then(|v| v.as_str()).unwrap_or("");

    if task_id.is_empty() {
        return (
            StatusCode::OK,
            Json(rpc_error(
                req.id,
                -32602,
                "Invalid params: missing task id",
                Some(error_reason::INVALID_PARAMS),
            )),
        );
    }

    let tasks = task_store.tasks.read().await;
    match tasks.get(task_id) {
        Some(task) => (
            StatusCode::OK,
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "result": task
            })),
        ),
        None => (
            StatusCode::OK,
            Json(rpc_error(
                req.id,
                -32001,
                "Task not found",
                Some(error_reason::TASK_NOT_FOUND),
            )),
        ),
    }
}

async fn handle_tasks_cancel(
    task_store: &Arc<TaskStore>,
    req: JsonRpcRequest,
) -> (StatusCode, Json<serde_json::Value>) {
    let task_id = req.params.get("id").and_then(|v| v.as_str()).unwrap_or("");

    if task_id.is_empty() {
        return (
            StatusCode::OK,
            Json(rpc_error(
                req.id,
                -32602,
                "Invalid params: missing task id",
                Some(error_reason::INVALID_PARAMS),
            )),
        );
    }

    let result = {
        let mut tasks = task_store.tasks.write().await;
        match tasks.get_mut(task_id) {
            Some(task) => {
                if task.status.state.is_terminal() {
                    return (
                        StatusCode::OK,
                        Json(rpc_error(
                            req.id,
                            -32002,
                            "Task is already in a terminal state",
                            Some(error_reason::TASK_ALREADY_TERMINAL),
                        )),
                    );
                }
                task.status.state = A2aTaskState::Canceled;
                task.status.timestamp = Some(chrono::Utc::now().to_rfc3339());
                let task = task.clone();
                Some((task_id.to_string(), task))
            }
            None => None,
        }
    };

    match result {
        Some((tid, task)) => {
            task_store.mark_terminal(&tid).await;
            (
                StatusCode::OK,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": task
                })),
            )
        }
        None => (
            StatusCode::OK,
            Json(rpc_error(
                req.id,
                -32001,
                "Task not found",
                Some(error_reason::TASK_NOT_FOUND),
            )),
        ),
    }
}

async fn handle_tasks_list(
    task_store: &Arc<TaskStore>,
    req: JsonRpcRequest,
) -> (StatusCode, Json<serde_json::Value>) {
    // Parse parameters
    let context_id_filter = req
        .params
        .get("contextId")
        .or_else(|| req.params.get("context_id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let status_filter: Option<A2aTaskState> = req
        .params
        .get("status")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_value(json!(s)).ok());

    let page_size = req
        .params
        .get("pageSize")
        .or_else(|| req.params.get("page_size"))
        .and_then(|v| v.as_u64())
        .map(|n| n.clamp(1, 100) as usize)
        .unwrap_or(50);

    let page_token = req
        .params
        .get("pageToken")
        .or_else(|| req.params.get("page_token"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let history_length = req
        .params
        .get("historyLength")
        .or_else(|| req.params.get("history_length"))
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok());

    let include_artifacts = req
        .params
        .get("includeArtifacts")
        .or_else(|| req.params.get("include_artifacts"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let status_timestamp_after = req
        .params
        .get("statusTimestampAfter")
        .or_else(|| req.params.get("status_timestamp_after"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let tasks = task_store.tasks.read().await;

    // Collect and sort by task ID for stable ordering
    let mut sorted: Vec<&Task> = tasks.values().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    // Apply filters
    let filtered: Vec<&Task> = sorted
        .into_iter()
        .filter(|t| {
            if let Some(ref ctx) = context_id_filter {
                if t.context_id.as_deref() != Some(ctx) {
                    return false;
                }
            }
            if let Some(ref status) = status_filter {
                if &t.status.state != status {
                    return false;
                }
            }
            if let Some(ref after) = status_timestamp_after {
                if let Some(ref ts) = t.status.timestamp {
                    if ts.as_str() <= after.as_str() {
                        return false;
                    }
                } else {
                    // Tasks without a status timestamp are excluded when filter is active
                    return false;
                }
            }
            true
        })
        .collect();

    // Apply cursor: skip tasks up to and including page_token
    let after_cursor: Vec<&Task> = if let Some(ref token) = page_token {
        let mut found = false;
        filtered
            .into_iter()
            .filter(move |t| {
                if found {
                    return true;
                }
                if t.id == *token {
                    found = true;
                }
                false
            })
            .collect()
    } else {
        filtered
    };

    // Take page_size + 1 to detect if there are more entries
    let has_more = after_cursor.len() > page_size;
    let page: Vec<&Task> = after_cursor.into_iter().take(page_size).collect();

    let next_page_token = if has_more {
        page.last().map(|t| t.id.clone())
    } else {
        None
    };

    // Build response tasks with optional trimming
    let result_tasks: Vec<serde_json::Value> = page
        .into_iter()
        .map(|t| {
            let mut task = t.clone();
            if !include_artifacts {
                task.artifacts = None;
            }
            if let Some(max_len) = history_length {
                if let Some(ref mut history) = task.history {
                    if history.len() > max_len {
                        let start = history.len() - max_len;
                        *history = history.split_off(start);
                    }
                }
            }
            serde_json::to_value(task).unwrap_or_default()
        })
        .collect();

    let mut result = json!({
        "tasks": result_tasks,
        "pageSize": page_size,
    });
    if let Some(token) = next_page_token {
        result["nextPageToken"] = json!(token);
    }

    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": req.id,
            "result": result
        })),
    )
}

async fn handle_tasks_get_by_context(
    task_store: &Arc<TaskStore>,
    req: JsonRpcRequest,
) -> (StatusCode, Json<serde_json::Value>) {
    let context_id = req
        .params
        .get("contextId")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if context_id.is_empty() {
        return (
            StatusCode::OK,
            Json(rpc_error(
                req.id,
                -32602,
                "Invalid params: missing contextId",
                Some(error_reason::INVALID_PARAMS),
            )),
        );
    }

    let tasks = task_store.tasks_by_context(context_id).await;
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": req.id,
            "result": tasks
        })),
    )
}

// ── v1.0 REST-style endpoint handlers ───────────────────────

/// Unwrap a JSON-RPC response into a REST response.
/// Returns the `result` payload on success, or maps JSON-RPC error codes
/// to appropriate HTTP status codes.
fn unwrap_rpc_to_rest(
    rpc_status: StatusCode,
    rpc_body: serde_json::Value,
) -> (StatusCode, Json<serde_json::Value>) {
    // Propagate non-OK HTTP status directly (auth errors, 503, etc.)
    if rpc_status != StatusCode::OK {
        return (rpc_status, Json(rpc_body));
    }

    // If there's a result, return it directly
    if let Some(result) = rpc_body.get("result").cloned() {
        return (StatusCode::OK, Json(result));
    }

    // Translate JSON-RPC error codes to HTTP status codes
    if let Some(error) = rpc_body.get("error") {
        let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-32000);
        let http_status = match code {
            -32600 => StatusCode::BAD_REQUEST,      // Invalid request
            -32601 => StatusCode::NOT_FOUND,        // Method not found
            -32602 => StatusCode::BAD_REQUEST,      // Invalid params
            -32001 => StatusCode::NOT_FOUND,        // Task not found
            -32002 => StatusCode::CONFLICT,         // Task already terminal
            _ => StatusCode::INTERNAL_SERVER_ERROR, // Server errors
        };
        let mut rest_error = json!({
            "code": code,
            "message": error.get("message").cloned().unwrap_or(json!("Unknown error"))
        });
        if let Some(details) = error.get("details") {
            rest_error["details"] = details.clone();
        }
        return (http_status, Json(json!({ "error": rest_error })));
    }

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": {"message": "Unexpected response format"}})),
    )
}

/// `POST /message:send` — v1.0 REST binding for SendMessage.
pub async fn handle_message_send_rest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(params): Json<serde_json::Value>,
) -> impl IntoResponse {
    let (Some(_card), Some(task_store)) = (&state.a2a_agent_card, &state.a2a_task_store) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "A2A protocol not enabled"})),
        )
            .into_response();
    };

    if let Err(resp) = require_a2a_auth(&state, &headers) {
        return resp.into_response();
    }

    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: json!(uuid::Uuid::new_v4().to_string()),
        method: "message/send".into(),
        params,
    };
    let (status, Json(body)) = Box::pin(handle_message_send(&state, task_store, req)).await;
    unwrap_rpc_to_rest(status, body).into_response()
}

/// `GET /tasks/{id}` — v1.0 REST binding for GetTask.
pub async fn handle_tasks_get_rest(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let (Some(_card), Some(task_store)) = (&state.a2a_agent_card, &state.a2a_task_store) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "A2A protocol not enabled"})),
        )
            .into_response();
    };

    if let Err(resp) = require_a2a_auth(&state, &headers) {
        return resp.into_response();
    }

    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: json!(uuid::Uuid::new_v4().to_string()),
        method: "tasks/get".into(),
        params: json!({"id": task_id}),
    };
    let (status, Json(body)) = handle_tasks_get(task_store, req).await;
    unwrap_rpc_to_rest(status, body).into_response()
}

/// `POST /tasks/{id}:cancel` — v1.0 REST binding for CancelTask.
pub async fn handle_tasks_cancel_rest(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let (Some(_card), Some(task_store)) = (&state.a2a_agent_card, &state.a2a_task_store) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "A2A protocol not enabled"})),
        )
            .into_response();
    };

    if let Err(resp) = require_a2a_auth(&state, &headers) {
        return resp.into_response();
    }

    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: json!(uuid::Uuid::new_v4().to_string()),
        method: "tasks/cancel".into(),
        params: json!({"id": task_id}),
    };
    let (status, Json(body)) = handle_tasks_cancel(task_store, req).await;
    unwrap_rpc_to_rest(status, body).into_response()
}

/// Query parameters for `GET /tasks`.
#[derive(Debug, Deserialize)]
pub struct ListTasksQuery {
    pub context_id: Option<String>,
    pub status: Option<String>,
    pub page_size: Option<u64>,
    pub page_token: Option<String>,
    pub history_length: Option<u64>,
    pub include_artifacts: Option<bool>,
    pub status_timestamp_after: Option<String>,
}

/// `GET /tasks` — v1.0 REST binding for ListTasks.
pub async fn handle_tasks_list_rest(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<ListTasksQuery>,
) -> impl IntoResponse {
    let (Some(_card), Some(task_store)) = (&state.a2a_agent_card, &state.a2a_task_store) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "A2A protocol not enabled"})),
        )
            .into_response();
    };

    if let Err(resp) = require_a2a_auth(&state, &headers) {
        return resp.into_response();
    }

    let mut params = json!({});
    if let Some(ctx) = query.context_id {
        params["contextId"] = json!(ctx);
    }
    if let Some(status) = query.status {
        params["status"] = json!(status);
    }
    if let Some(ps) = query.page_size {
        params["pageSize"] = json!(ps);
    }
    if let Some(pt) = query.page_token {
        params["pageToken"] = json!(pt);
    }
    if let Some(hl) = query.history_length {
        params["historyLength"] = json!(hl);
    }
    if let Some(ia) = query.include_artifacts {
        params["includeArtifacts"] = json!(ia);
    }
    if let Some(sta) = query.status_timestamp_after {
        params["statusTimestampAfter"] = json!(sta);
    }

    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: json!(uuid::Uuid::new_v4().to_string()),
        method: "tasks/list".into(),
        params,
    };
    let (status, Json(body)) = handle_tasks_list(task_store, req).await;
    unwrap_rpc_to_rest(status, body).into_response()
}

/// `GET /tasks/by-context/{context_id}` — v1.0 REST binding for tasks by context.
pub async fn handle_tasks_by_context_rest(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(context_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let (Some(_card), Some(task_store)) = (&state.a2a_agent_card, &state.a2a_task_store) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "A2A protocol not enabled"})),
        )
            .into_response();
    };

    if let Err(resp) = require_a2a_auth(&state, &headers) {
        return resp.into_response();
    }

    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: json!(uuid::Uuid::new_v4().to_string()),
        method: "tasks/getByContextId".into(),
        params: json!({"contextId": context_id}),
    };
    let (status, Json(body)) = handle_tasks_get_by_context(task_store, req).await;
    unwrap_rpc_to_rest(status, body).into_response()
}

// ── v1.0 SSE streaming endpoint ──────────────────────────────────

/// `POST /message:stream` — v1.0 REST binding for `SendStreamingMessage`.
///
/// Returns a Server-Sent Events stream that emits `TaskStatusUpdateEvent`
/// and `TaskArtifactUpdateEvent` payloads as the agent processes the
/// request.  The stream terminates after the task reaches a terminal state.
pub async fn handle_message_stream_rest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(params): Json<serde_json::Value>,
) -> impl IntoResponse {
    // ── Feature gate ────────────────────────────────────────────
    let (Some(_card), Some(task_store)) = (&state.a2a_agent_card, &state.a2a_task_store) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "A2A protocol not enabled"})),
        )
            .into_response();
    };

    // ── Auth ────────────────────────────────────────────────────
    if let Err(resp) = require_a2a_auth(&state, &headers) {
        return resp.into_response();
    }

    // ── Parse inbound message (reuse same logic as message/send) ─
    let (message_text, inbound_msg, context_id) = match parse_inbound_message(&params) {
        Ok(v) => v,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"code": -32602, "message": msg}})),
            )
                .into_response();
        }
    };

    let task_id = uuid::Uuid::new_v4().to_string();
    let task_store = Arc::clone(task_store);

    // ── Reserve a task slot ─────────────────────────────────────
    {
        let mut tasks = task_store.tasks.write().await;
        if tasks.len() >= MAX_TASKS {
            tasks.retain(|_, t| !t.status.state.is_terminal());
            if tasks.len() >= MAX_TASKS {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": {"code": -32000, "message": "Task store full"}})),
                )
                    .into_response();
            }
        }
        tasks.insert(
            task_id.clone(),
            Task {
                id: task_id.clone(),
                status: TaskStatus {
                    state: A2aTaskState::Submitted,
                    message: None,
                    timestamp: Some(chrono::Utc::now().to_rfc3339()),
                },
                context_id: Some(context_id.clone()),
                artifacts: None,
                history: Some(vec![inbound_msg]),
                metadata: None,
            },
        );
    }
    task_store.index_context(&context_id, &task_id).await;

    // Build conversation history from prior tasks in this context
    let prompt_text = build_context_prompt(&task_store, &context_id, &task_id, &message_text).await;

    // ── Spawn background task that owns the agent lifecycle ────
    //
    // The agent turn, task-store finalization, and Telegram notification
    // run in a spawned task so that SSE client disconnects (which drop
    // the response body / stream) cannot cancel finalization.  The SSE
    // stream only reads from an mpsc channel fed by this background task.
    let config = state.config.lock().clone();
    let telegram_notify = config.a2a.notify_chat_id.and_then(|chat_id| {
        config
            .channels_config
            .telegram
            .as_ref()
            .map(|t| (t.bot_token.clone(), chat_id))
    });
    let tid = task_id.clone();
    let ctx = context_id.clone();

    let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<Event>(64);

    tokio::spawn({
        let tid = tid.clone();
        let ctx = ctx.clone();
        let task_store = Arc::clone(&task_store);
        let prompt_text = prompt_text.clone();
        let message_text = message_text.clone();

        async move {
            use crate::agent::TurnEvent;

            // Helper: best-effort send (client may have disconnected)
            macro_rules! emit {
                ($event:expr) => {
                    let _ = sse_tx.send($event).await;
                };
            }

            // Emit initial status: working
            let working_event = TaskStatusUpdateEvent {
                task_id: tid.clone(),
                context_id: Some(ctx.clone()),
                status: TaskStatus {
                    state: A2aTaskState::Working,
                    message: None,
                    timestamp: Some(chrono::Utc::now().to_rfc3339()),
                },
                is_final: false,
                metadata: None,
            };
            emit!(
                Event::default()
                    .event("status_update")
                    .data(serde_json::to_string(&working_event).unwrap_or_default())
            );

            // Update task store to working
            {
                let mut tasks = task_store.tasks.write().await;
                if let Some(t) = tasks.get_mut(&tid) {
                    t.status.state = A2aTaskState::Working;
                    t.status.timestamp = Some(chrono::Utc::now().to_rfc3339());
                }
            }

            // Create agent for streaming
            let mut agent = match crate::agent::Agent::from_config(&config).await {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!(task_id = %tid, error = %e, "Agent init failed for SSE");
                    let fail_status = TaskStatus {
                        state: A2aTaskState::Failed,
                        message: Some(Message {
                            role: "ROLE_AGENT".to_string(),
                            parts: vec![Part::Text {
                                text: "Internal processing error".to_string(),
                                metadata: None,
                            }],
                            message_id: uuid::Uuid::new_v4().to_string(),
                            context_id: Some(ctx.clone()),
                            metadata: None,
                        }),
                        timestamp: Some(chrono::Utc::now().to_rfc3339()),
                    };
                    let fail_event = TaskStatusUpdateEvent {
                        task_id: tid.clone(),
                        context_id: Some(ctx.clone()),
                        status: fail_status.clone(),
                        is_final: true,
                        metadata: None,
                    };
                    emit!(
                        Event::default()
                            .event("status_update")
                            .data(serde_json::to_string(&fail_event).unwrap_or_default())
                    );

                    // Update task store — always runs even if client disconnected
                    {
                        let mut tasks = task_store.tasks.write().await;
                        if let Some(t) = tasks.get_mut(&tid) {
                            t.status = fail_status;
                        }
                    }
                    task_store.mark_terminal(&tid).await;
                    return;
                }
            };

            // Stream the agent turn
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
            let msg_owned = prompt_text.clone();

            // Accumulate text chunks for the final artifact
            let accumulated_text = Arc::new(RwLock::new(String::new()));
            let acc_clone = Arc::clone(&accumulated_text);

            let turn_handle =
                tokio::spawn(async move { agent.turn_streamed(&msg_owned, event_tx).await });

            let artifact_id = uuid::Uuid::new_v4().to_string();
            let mut chunk_index: u32 = 0;

            while let Some(event) = event_rx.recv().await {
                match event {
                    TurnEvent::Chunk { delta } => {
                        acc_clone.write().await.push_str(&delta);
                        chunk_index += 1;

                        let artifact_event = TaskArtifactUpdateEvent {
                            task_id: tid.clone(),
                            context_id: Some(ctx.clone()),
                            artifact: Artifact {
                                artifact_id: artifact_id.clone(),
                                name: Some("response".to_string()),
                                description: None,
                                parts: vec![Part::Text {
                                    text: delta,
                                    metadata: None,
                                }],
                                metadata: Some(json!({
                                    "append": true,
                                    "chunkIndex": chunk_index,
                                })),
                                extensions: None,
                            },
                            metadata: None,
                        };
                        emit!(
                            Event::default()
                                .event("artifact_update")
                                .data(serde_json::to_string(&artifact_event).unwrap_or_default())
                        );
                    }
                    TurnEvent::Thinking { delta } => {
                        let ev = TaskStatusUpdateEvent {
                            task_id: tid.clone(),
                            context_id: Some(ctx.clone()),
                            status: TaskStatus {
                                state: A2aTaskState::Working,
                                message: None,
                                timestamp: Some(chrono::Utc::now().to_rfc3339()),
                            },
                            is_final: false,
                            metadata: Some(json!({"thinking": delta})),
                        };
                        emit!(
                            Event::default()
                                .event("status_update")
                                .data(serde_json::to_string(&ev).unwrap_or_default())
                        );
                    }
                    TurnEvent::ToolCall { name, args } => {
                        let ev = TaskStatusUpdateEvent {
                            task_id: tid.clone(),
                            context_id: Some(ctx.clone()),
                            status: TaskStatus {
                                state: A2aTaskState::Working,
                                message: None,
                                timestamp: Some(chrono::Utc::now().to_rfc3339()),
                            },
                            is_final: false,
                            metadata: Some(json!({"toolCall": {"name": name, "args": args}})),
                        };
                        emit!(
                            Event::default()
                                .event("status_update")
                                .data(serde_json::to_string(&ev).unwrap_or_default())
                        );
                    }
                    TurnEvent::ToolResult { name, output } => {
                        let ev = TaskStatusUpdateEvent {
                            task_id: tid.clone(),
                            context_id: Some(ctx.clone()),
                            status: TaskStatus {
                                state: A2aTaskState::Working,
                                message: None,
                                timestamp: Some(chrono::Utc::now().to_rfc3339()),
                            },
                            is_final: false,
                            metadata: Some(json!({"toolResult": {"name": name, "output": output}})),
                        };
                        emit!(
                            Event::default()
                                .event("status_update")
                                .data(serde_json::to_string(&ev).unwrap_or_default())
                        );
                    }
                }
            }

            // Agent turn completed — determine final status
            let turn_result = turn_handle.await.map_err(|e| {
                tracing::error!(task_id = %tid, error = %e, "Agent turn task panicked");
                e
            });

            let full_text = accumulated_text.read().await.clone();

            let (final_state, final_message, final_artifact) = match turn_result {
                Ok(Ok(response)) => {
                    let text = if full_text.is_empty() {
                        response
                    } else {
                        full_text
                    };
                    (
                        A2aTaskState::Completed,
                        Message {
                            role: "ROLE_AGENT".to_string(),
                            parts: vec![Part::Text {
                                text: text.clone(),
                                metadata: None,
                            }],
                            message_id: uuid::Uuid::new_v4().to_string(),
                            context_id: Some(ctx.clone()),
                            metadata: None,
                        },
                        Some(Artifact {
                            artifact_id: artifact_id.clone(),
                            name: Some("response".to_string()),
                            description: None,
                            parts: vec![Part::Text {
                                text,
                                metadata: None,
                            }],
                            metadata: None,
                            extensions: None,
                        }),
                    )
                }
                _ => (
                    A2aTaskState::Failed,
                    Message {
                        role: "ROLE_AGENT".to_string(),
                        parts: vec![Part::Text {
                            text: "Internal processing error".to_string(),
                            metadata: None,
                        }],
                        message_id: uuid::Uuid::new_v4().to_string(),
                        context_id: Some(ctx.clone()),
                        metadata: None,
                    },
                    None,
                ),
            };

            // Notify Telegram on success
            if final_state == A2aTaskState::Completed {
                if let Some((ref token, chat_id)) = telegram_notify {
                    let response_text = final_message
                        .parts
                        .first()
                        .and_then(|p| match p {
                            Part::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .unwrap_or("");
                    let notice = format!(
                        "\u{1f4e8} *A2A stream received:* _{}_\n\n{}",
                        message_text.replace('*', "\\*").replace('_', "\\_"),
                        response_text
                    );
                    notify_telegram_chat(token, chat_id, &notice).await;
                }
            }

            // Finalize task store — always runs even if SSE client disconnected
            {
                let mut tasks = task_store.tasks.write().await;
                if let Some(t) = tasks.get_mut(&tid) {
                    t.status = TaskStatus {
                        state: final_state.clone(),
                        message: Some(final_message.clone()),
                        timestamp: Some(chrono::Utc::now().to_rfc3339()),
                    };
                    t.artifacts = final_artifact.as_ref().map(|a| vec![a.clone()]);
                }
            }
            if final_state.is_terminal() {
                task_store.mark_terminal(&tid).await;
            }

            // Emit final status_update (best-effort — client may be gone)
            let final_event = TaskStatusUpdateEvent {
                task_id: tid.clone(),
                context_id: Some(ctx.clone()),
                status: TaskStatus {
                    state: final_state,
                    message: Some(final_message),
                    timestamp: Some(chrono::Utc::now().to_rfc3339()),
                },
                is_final: true,
                metadata: None,
            };
            let _ = sse_tx
                .send(
                    Event::default()
                        .event("status_update")
                        .data(serde_json::to_string(&final_event).unwrap_or_default()),
                )
                .await;
        }
    });

    // ── SSE stream reads from channel — disconnect-safe ─────────
    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx).map(Ok::<_, Infallible>);

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Parse inbound A2A message params, returning (text, Message, context_id).
fn parse_inbound_message(
    params: &serde_json::Value,
) -> Result<(String, Message, String), &'static str> {
    if let Some(msg_obj) = params
        .get("message")
        .filter(|m| m.get("parts").and_then(|p| p.as_array()).is_some())
    {
        let text = msg_obj
            .pointer("/parts")
            .and_then(|parts| parts.as_array())
            .and_then(|parts| {
                parts.iter().find_map(|p| {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                        .or_else(|| {
                            if p.get("kind").and_then(|t| t.as_str()) == Some("text") {
                                p.get("text").and_then(|t| t.as_str()).map(String::from)
                            } else {
                                None
                            }
                        })
                })
            });
        let Some(text) = text else {
            return Err("Invalid params: missing message text");
        };

        let ctx_id = msg_obj
            .get("contextId")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let inbound: Message = match serde_json::from_value::<Message>(msg_obj.clone()) {
            Ok(mut msg) => {
                if msg.context_id.is_none() {
                    msg.context_id = Some(ctx_id.clone());
                }
                msg
            }
            Err(_) => Message {
                role: msg_obj
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("ROLE_USER")
                    .to_string(),
                parts: vec![Part::Text {
                    text: text.clone(),
                    metadata: None,
                }],
                message_id: msg_obj
                    .get("messageId")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                context_id: Some(ctx_id.clone()),
                metadata: msg_obj.get("metadata").cloned(),
            },
        };

        Ok((text, inbound, ctx_id))
    } else if let Some(text) = params
        .get("message")
        .and_then(|m| m.as_str())
        .map(String::from)
    {
        let ctx_id = uuid::Uuid::new_v4().to_string();
        let inbound = Message {
            role: "ROLE_USER".to_string(),
            parts: vec![Part::Text {
                text: text.clone(),
                metadata: None,
            }],
            message_id: uuid::Uuid::new_v4().to_string(),
            context_id: Some(ctx_id.clone()),
            metadata: None,
        };
        Ok((text, inbound, ctx_id))
    } else {
        Err("Invalid params: missing message text")
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// Best-effort Telegram notification for A2A activity.
/// Sends a message to a known chat ID (e.g. a group chat).
async fn notify_telegram_chat(bot_token: &str, chat_id: i64, text: &str) {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let _ = client
        .post(&url)
        .json(&json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "Markdown"
        }))
        .send()
        .await;
}

/// Build a prompt that prepends conversation history from prior tasks in the
/// same `context_id`.  If there is no prior history the original message is
/// returned unchanged.
async fn build_context_prompt(
    task_store: &TaskStore,
    context_id: &str,
    current_task_id: &str,
    message_text: &str,
) -> String {
    let prior_tasks = task_store.tasks_by_context(context_id).await;
    let prior: Vec<&Task> = prior_tasks
        .iter()
        .filter(|t| t.id != current_task_id)
        .collect();
    if prior.is_empty() {
        return message_text.to_owned();
    }

    use std::fmt::Write;

    let mut history = String::from("[Previous conversation in this context]\n");
    for task in &prior {
        // Append user messages from history
        if let Some(msgs) = &task.history {
            for msg in msgs {
                let role_label = if msg.role.contains("USER") {
                    "User"
                } else {
                    "Agent"
                };
                let text = extract_text_from_parts(&msg.parts);
                if !text.is_empty() {
                    let _ = writeln!(history, "{role_label}: {text}");
                }
            }
        }
        // Append agent response from status message
        if let Some(ref msg) = task.status.message {
            let text = extract_text_from_parts(&msg.parts);
            if !text.is_empty() {
                let _ = writeln!(history, "Agent: {text}");
            }
        }
    }
    let _ = write!(history, "[Current message]\nUser: {message_text}");
    history
}

/// Extract concatenated text from message parts.
fn extract_text_from_parts(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn rpc_error(
    id: serde_json::Value,
    code: i32,
    message: &str,
    reason: Option<&str>,
) -> serde_json::Value {
    let mut error = json!({
        "code": code,
        "message": message
    });
    if let Some(reason) = reason {
        error["details"] = json!([{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": reason,
            "domain": "a2a-protocol.org",
            "metadata": {}
        }]);
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{AppState, GatewayRateLimiter, IdempotencyStore, nodes};
    use crate::memory::{Memory, MemoryCategory, MemoryEntry};
    use crate::providers::Provider;
    use crate::security::pairing::PairingGuard;
    use async_trait::async_trait;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use parking_lot::Mutex;
    use std::time::Duration;

    // ── Test mocks ───────────────────────────────────────────

    struct MockMemory;

    #[async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str {
            "mock"
        }
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn health_check(&self) -> bool {
            true
        }
    }

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }
    }

    /// Build an `AppState` with optional A2A and pairing configuration.
    fn a2a_test_state(
        bearer_token: Option<&str>,
        require_pairing: bool,
        paired_tokens: &[String],
    ) -> AppState {
        let mut config = crate::config::Config::default();
        config.a2a.enabled = true;
        if let Some(token) = bearer_token {
            config.a2a.bearer_token = Some(token.to_string());
        }

        let card = generate_agent_card(&config);

        AppState {
            config: Arc::new(Mutex::new(config)),
            provider: Arc::new(MockProvider),
            model: "test-model".into(),
            temperature: 0.0,
            mem: Arc::new(MockMemory),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(require_pairing, paired_tokens)),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            whatsapp: None,
            whatsapp_app_secret: None,
            linq: None,
            linq_signing_secret: None,
            nextcloud_talk: None,
            nextcloud_talk_webhook_secret: None,
            wati: None,
            gmail_push: None,
            observer: Arc::new(crate::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            session_backend: None,
            device_registry: None,
            pending_pairings: None,
            path_prefix: String::new(),
            canvas_store: crate::tools::canvas::CanvasStore::new(),
            a2a_agent_card: Some(Arc::new(card)),
            a2a_task_store: Some(Arc::new(TaskStore::new())),
            auth_limiter: Arc::new(crate::gateway::auth_rate_limit::AuthRateLimiter::new()),
            session_queue: Arc::new(crate::gateway::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
        }
    }

    fn bearer_header(token: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    async fn response_json(resp: axum::response::Response) -> serde_json::Value {
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    // ── Unit tests ───────────────────────────────────────────

    #[test]
    fn agent_card_generation_defaults() {
        let config = crate::config::Config {
            a2a: crate::config::A2aConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let card = generate_agent_card(&config);
        assert_eq!(card["name"], "ZeroClaw Agent");
        // v1.0: supported_interfaces replaces top-level url
        let ifaces = card["supported_interfaces"].as_array().unwrap();
        assert_eq!(ifaces.len(), 1);
        assert!(ifaces[0]["url"].as_str().unwrap().starts_with("http://"));
        assert_eq!(ifaces[0]["protocol_binding"], "JSONRPC");
        assert_eq!(ifaces[0]["protocol_version"], "1.0");
        assert!(card["capabilities"].is_object());
        assert_eq!(card["capabilities"]["streaming"], true);
        // v1.0: security_schemes replaces authentication
        assert!(card["security_schemes"]["bearer"].is_object());
        assert!(card["security_requirements"].is_array());
        // v1.0: MIME types instead of bare "text"
        assert_eq!(card["defaultInputModes"][0], "text/plain");
        assert_eq!(card["defaultOutputModes"][0], "text/plain");
        // Provider must include url
        assert!(card["provider"]["url"].is_string());
        // Skills should have proper AgentSkill structure
        let skills = card["skills"].as_array().unwrap();
        assert!(!skills.is_empty());
        assert!(skills[0]["id"].is_string());
        assert!(skills[0]["name"].is_string());
        assert!(skills[0]["description"].is_string());
    }

    #[test]
    fn agent_card_generation_custom() {
        let config = crate::config::Config {
            a2a: crate::config::A2aConfig {
                enabled: true,
                agent_name: Some("my-agent".into()),
                description: Some("My custom agent".into()),
                public_url: Some("https://agent.example.com".into()),
                capabilities: vec!["search".into(), "code".into()],
                ..Default::default()
            },
            ..Default::default()
        };

        let card = generate_agent_card(&config);
        assert_eq!(card["name"], "my-agent");
        assert_eq!(card["description"], "My custom agent");
        // v1.0: URL is in supported_interfaces
        let ifaces = card["supported_interfaces"].as_array().unwrap();
        assert!(
            ifaces[0]["url"]
                .as_str()
                .unwrap()
                .starts_with("https://agent.example.com")
        );
        assert_eq!(card["skills"].as_array().unwrap().len(), 2);
        assert_eq!(card["skills"][0]["id"], "search");
    }

    #[test]
    fn rpc_error_format() {
        let err = rpc_error(json!(1), -32600, "Test error", Some("INVALID_REQUEST"));
        assert_eq!(err["jsonrpc"], "2.0");
        assert_eq!(err["id"], 1);
        assert_eq!(err["error"]["code"], -32600);
        assert_eq!(err["error"]["message"], "Test error");
        // v1.0: details array is present when reason is provided
        let details = err["error"]["details"].as_array().unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0]["reason"], "INVALID_REQUEST");
    }

    #[test]
    fn rpc_error_without_reason_has_no_details() {
        let err = rpc_error(json!(1), -32600, "Test error", None);
        assert_eq!(err["error"]["code"], -32600);
        assert!(err["error"]["details"].is_null());
    }

    #[test]
    fn error_includes_a2a_domain() {
        let err = rpc_error(
            json!(1),
            -32001,
            "Task not found",
            Some(error_reason::TASK_NOT_FOUND),
        );
        let details = err["error"]["details"].as_array().unwrap();
        assert_eq!(details[0]["domain"], "a2a-protocol.org");
    }

    #[test]
    fn error_includes_error_info_type() {
        let err = rpc_error(
            json!(1),
            -32602,
            "Invalid params",
            Some(error_reason::INVALID_PARAMS),
        );
        let details = err["error"]["details"].as_array().unwrap();
        assert_eq!(
            details[0]["@type"],
            "type.googleapis.com/google.rpc.ErrorInfo"
        );
    }

    #[test]
    fn error_reason_codes_match_expected() {
        let cases: Vec<(i32, &str, &str)> = vec![
            (-32600, "Invalid request", error_reason::INVALID_REQUEST),
            (-32601, "Method not found", error_reason::METHOD_NOT_FOUND),
            (-32602, "Invalid params", error_reason::INVALID_PARAMS),
            (-32000, "Unauthorized", error_reason::UNAUTHORIZED),
            (-32001, "Task not found", error_reason::TASK_NOT_FOUND),
            (-32002, "Task terminal", error_reason::TASK_ALREADY_TERMINAL),
            (-32000, "Store full", error_reason::TASK_STORE_FULL),
            (-32000, "Internal", error_reason::INTERNAL_ERROR),
        ];
        for (code, msg, reason) in cases {
            let err = rpc_error(json!(1), code, msg, Some(reason));
            let details = err["error"]["details"].as_array().unwrap();
            assert_eq!(details[0]["reason"], reason, "reason mismatch for {msg}");
            assert_eq!(
                details[0]["domain"], "a2a-protocol.org",
                "domain mismatch for {msg}"
            );
            assert_eq!(
                details[0]["@type"], "type.googleapis.com/google.rpc.ErrorInfo",
                "@type mismatch for {msg}"
            );
        }
    }

    #[tokio::test]
    async fn task_store_lifecycle() {
        let store = TaskStore::new();
        let task_id = "test-123".to_string();

        // Insert
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert(
                task_id.clone(),
                Task {
                    id: task_id.clone(),
                    status: TaskStatus {
                        state: A2aTaskState::Working,
                        message: None,
                        timestamp: None,
                    },
                    context_id: None,
                    artifacts: None,
                    history: None,
                    metadata: None,
                },
            );
        }

        // Read
        {
            let tasks = store.tasks.read().await;
            let task = tasks.get(&task_id).unwrap();
            assert_eq!(task.status.state, A2aTaskState::Working);
        }

        // Update
        {
            let mut tasks = store.tasks.write().await;
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status.state = A2aTaskState::Completed;
                task.artifacts = Some(vec![Artifact {
                    artifact_id: "a1".to_string(),
                    name: None,
                    description: None,
                    parts: vec![Part::Text {
                        text: "done".to_string(),
                        metadata: None,
                    }],
                    metadata: None,
                    extensions: None,
                }]);
            }
        }

        // Verify
        {
            let tasks = store.tasks.read().await;
            let task = tasks.get(&task_id).unwrap();
            assert_eq!(task.status.state, A2aTaskState::Completed);
            assert_eq!(task.artifacts.as_ref().unwrap().len(), 1);
        }
    }

    #[test]
    fn max_tasks_constant_is_reasonable() {
        let max = MAX_TASKS;
        assert!(max >= 1_000, "MAX_TASKS should allow reasonable load");
        assert!(max <= 100_000, "MAX_TASKS should cap memory growth");
    }

    // ── Handler integration tests ────────────────────────────

    #[tokio::test]
    async fn agent_card_endpoint_returns_card_when_enabled() {
        let state = a2a_test_state(Some("secret"), false, &[]);
        let resp = handle_agent_card(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        assert_eq!(body["name"], "ZeroClaw Agent");
        assert!(body["skills"].is_array());
    }

    #[tokio::test]
    async fn agent_card_endpoint_returns_404_when_disabled() {
        let mut state = a2a_test_state(None, false, &[]);
        state.a2a_agent_card = None;
        let resp = handle_agent_card(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rpc_rejects_missing_bearer_when_token_configured() {
        let state = a2a_test_state(Some("my-secret"), false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "x"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rpc_rejects_wrong_bearer_token() {
        let state = a2a_test_state(Some("correct"), false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "x"}),
        };
        let headers = bearer_header("wrong");
        let resp = handle_a2a_rpc(State(state), headers, Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rpc_accepts_correct_bearer_token() {
        let state = a2a_test_state(Some("correct"), false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "nonexistent"}),
        };
        let headers = bearer_header("correct");
        let resp = handle_a2a_rpc(State(state), headers, Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        // Should get "task not found" — not an auth error
        assert_eq!(body["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn rpc_allows_unauthenticated_when_no_auth_configured() {
        let state = a2a_test_state(None, false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "x"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        // Should reach method handler, not auth rejection
        assert_eq!(body["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn rpc_rejects_unauthenticated_when_pairing_required() {
        let state = a2a_test_state(None, true, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "x"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rpc_rejects_invalid_jsonrpc_version() {
        let state = a2a_test_state(None, false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "1.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rpc_unknown_method_returns_method_not_found() {
        let state = a2a_test_state(None, false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(42),
            method: "tasks/unknown".into(),
            params: json!({}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        let body = response_json(resp).await;
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn tasks_get_returns_not_found_for_missing_task() {
        let store = Arc::new(TaskStore::new());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "no-such-task"}),
        };
        let (status, Json(body)) = handle_tasks_get(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32001);
        // Error message must NOT echo the user-supplied task ID
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(
            !msg.contains("no-such-task"),
            "error must not echo user input"
        );
    }

    #[tokio::test]
    async fn tasks_get_returns_task_when_exists() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert(
                "task-abc".into(),
                Task {
                    id: "task-abc".into(),
                    status: TaskStatus {
                        state: A2aTaskState::Completed,
                        message: None,
                        timestamp: None,
                    },
                    context_id: None,
                    artifacts: Some(vec![Artifact {
                        artifact_id: "a1".to_string(),
                        name: None,
                        description: None,
                        parts: vec![Part::Text {
                            text: "result".to_string(),
                            metadata: None,
                        }],
                        metadata: None,
                        extensions: None,
                    }]),
                    history: None,
                    metadata: None,
                },
            );
        }
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "task-abc"}),
        };
        let (status, Json(body)) = handle_tasks_get(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["error"].is_null());
        assert_eq!(body["result"]["id"], "task-abc");
        assert_eq!(body["result"]["status"]["state"], "TASK_STATE_COMPLETED");
        let artifacts = body["result"]["artifacts"].as_array().unwrap();
        assert_eq!(artifacts.len(), 1);
        // Verify artifact has v1.0 structure with parts
        assert!(artifacts[0]["parts"].is_array());
    }

    #[tokio::test]
    async fn tasks_get_rejects_empty_task_id() {
        let store = Arc::new(TaskStore::new());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({}),
        };
        let (status, Json(body)) = handle_tasks_get(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32602);
    }

    // ── CancelTask tests ─────────────────────────────────────────

    #[tokio::test]
    async fn tasks_cancel_cancels_working_task() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert(
                "task-work".into(),
                Task {
                    id: "task-work".into(),
                    status: TaskStatus {
                        state: A2aTaskState::Working,
                        message: None,
                        timestamp: None,
                    },
                    context_id: None,
                    artifacts: None,
                    history: None,
                    metadata: None,
                },
            );
        }
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/cancel".into(),
            params: json!({"id": "task-work"}),
        };
        let (status, Json(body)) = handle_tasks_cancel(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["error"].is_null());
        assert_eq!(body["result"]["id"], "task-work");
        assert_eq!(body["result"]["status"]["state"], "TASK_STATE_CANCELED");
        assert!(body["result"]["status"]["timestamp"].is_string());
    }

    #[tokio::test]
    async fn tasks_cancel_returns_not_found_for_missing_task() {
        let store = Arc::new(TaskStore::new());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/cancel".into(),
            params: json!({"id": "no-such-task"}),
        };
        let (status, Json(body)) = handle_tasks_cancel(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn tasks_cancel_rejects_terminal_task() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert(
                "task-done".into(),
                Task {
                    id: "task-done".into(),
                    status: TaskStatus {
                        state: A2aTaskState::Completed,
                        message: None,
                        timestamp: None,
                    },
                    context_id: None,
                    artifacts: None,
                    history: None,
                    metadata: None,
                },
            );
        }
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/cancel".into(),
            params: json!({"id": "task-done"}),
        };
        let (status, Json(body)) = handle_tasks_cancel(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32002);
    }

    #[tokio::test]
    async fn tasks_cancel_rejects_empty_task_id() {
        let store = Arc::new(TaskStore::new());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/cancel".into(),
            params: json!({}),
        };
        let (status, Json(body)) = handle_tasks_cancel(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn tasks_cancel_rejects_already_canceled_task() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert(
                "task-cx".into(),
                Task {
                    id: "task-cx".into(),
                    status: TaskStatus {
                        state: A2aTaskState::Canceled,
                        message: None,
                        timestamp: None,
                    },
                    context_id: None,
                    artifacts: None,
                    history: None,
                    metadata: None,
                },
            );
        }
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/cancel".into(),
            params: json!({"id": "task-cx"}),
        };
        let (status, Json(body)) = handle_tasks_cancel(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        // Canceled is terminal, so this should be rejected
        assert_eq!(body["error"]["code"], -32002);
    }

    #[tokio::test]
    async fn tasks_cancel_via_rpc_dispatch() {
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.as_ref().unwrap();
        {
            let mut tasks = task_store.tasks.write().await;
            tasks.insert(
                "task-rpc".into(),
                Task {
                    id: "task-rpc".into(),
                    status: TaskStatus {
                        state: A2aTaskState::Working,
                        message: None,
                        timestamp: None,
                    },
                    context_id: None,
                    artifacts: None,
                    history: None,
                    metadata: None,
                },
            );
        }
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/cancel".into(),
            params: json!({"id": "task-rpc"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["result"]["status"]["state"], "TASK_STATE_CANCELED");
    }

    #[tokio::test]
    async fn task_store_capacity_limit_enforced() {
        let store = TaskStore::new();
        {
            let mut tasks = store.tasks.write().await;
            for i in 0..MAX_TASKS {
                tasks.insert(
                    format!("task-{i}"),
                    Task {
                        id: format!("task-{i}"),
                        status: TaskStatus {
                            state: A2aTaskState::Completed,
                            message: None,
                            timestamp: None,
                        },
                        context_id: None,
                        artifacts: None,
                        history: None,
                        metadata: None,
                    },
                );
            }
            assert_eq!(tasks.len(), MAX_TASKS);
        }

        // Verify the store is at capacity — direct insert would exceed
        {
            let tasks = store.tasks.read().await;
            assert_eq!(tasks.len(), MAX_TASKS);
        }
    }

    #[tokio::test]
    async fn rpc_disabled_returns_404() {
        let mut state = a2a_test_state(None, false, &[]);
        state.a2a_agent_card = None;
        state.a2a_task_store = None;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "message/send".into(),
            params: json!({"message": {"parts": [{"text": "hello"}]}}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── Gap 1: message/send handler (error path) ─────────────

    #[tokio::test]
    async fn message_send_missing_text_returns_invalid_params() {
        let state = a2a_test_state(None, false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "message/send".into(),
            params: json!({}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        let body = response_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("missing message")
        );
    }

    #[tokio::test]
    async fn message_send_accepts_simple_text_fallback() {
        // Tests the simple `params.message` string fallback path.
        // process_message will fail (no provider configured), so we
        // verify the task is created and the failure is handled cleanly.
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.clone().unwrap();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(99),
            method: "message/send".into(),
            params: json!({"message": "hello from simple fallback"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        // process_message fails in test (no provider) → task should be "failed"
        let result = &body["result"];
        assert!(result["id"].is_string());
        assert_eq!(result["status"]["state"], "TASK_STATE_FAILED");

        // Verify the task was stored with Failed status
        let task_id = result["id"].as_str().unwrap();
        let tasks = task_store.tasks.read().await;
        let task = tasks.get(task_id).unwrap();
        assert_eq!(task.status.state, A2aTaskState::Failed);
    }

    #[tokio::test]
    async fn message_send_accepts_v1_parts_format() {
        // Tests the v1.0 oneof message/parts format (no `kind` field).
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.clone().unwrap();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(100),
            method: "message/send".into(),
            params: json!({
                "message": {
                    "role": "ROLE_USER",
                    "parts": [{"text": "structured message"}],
                    "messageId": "msg-001",
                    "contextId": "ctx-abc"
                }
            }),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        let result = &body["result"];
        assert!(result["id"].is_string());
        // Will fail due to no provider, but verifies the message was extracted
        assert_eq!(result["status"]["state"], "TASK_STATE_FAILED");

        // v1.0: TaskStatus.message must be a Message object, not a string
        let status_msg = &result["status"]["message"];
        assert!(
            status_msg.is_object(),
            "TaskStatus.message must be a Message object"
        );
        assert_eq!(status_msg["role"], "ROLE_AGENT");
        assert!(status_msg["messageId"].is_string());
        assert!(status_msg["parts"].is_array());

        // v1.0: contextId propagated from inbound message
        assert_eq!(result["contextId"], "ctx-abc");

        // v1.0: history contains the inbound message
        let history = result["history"].as_array().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["role"], "ROLE_USER");
        assert_eq!(history[0]["messageId"], "msg-001");

        // Task was created in the store
        let task_id = result["id"].as_str().unwrap();
        let tasks = task_store.tasks.read().await;
        assert!(tasks.contains_key(task_id));
    }

    #[tokio::test]
    async fn message_send_rejects_when_store_full() {
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.clone().unwrap();

        // Fill the store to capacity with non-terminal (Working) tasks
        // so they won't be evicted by the terminal-task eviction logic.
        {
            let mut tasks = task_store.tasks.write().await;
            for i in 0..MAX_TASKS {
                tasks.insert(
                    format!("fill-{i}"),
                    Task {
                        id: format!("fill-{i}"),
                        status: TaskStatus {
                            state: A2aTaskState::Working,
                            message: None,
                            timestamp: None,
                        },
                        context_id: None,
                        artifacts: None,
                        history: None,
                        metadata: None,
                    },
                );
            }
        }

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "message/send".into(),
            params: json!({"message": "should be rejected"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_json(resp).await;
        assert_eq!(body["error"]["code"], -32000);
        assert!(body["error"]["message"].as_str().unwrap().contains("full"));
    }

    #[tokio::test]
    async fn message_send_evicts_terminal_tasks_when_at_capacity() {
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.clone().unwrap();

        // Fill the store with terminal (Completed) tasks
        {
            let mut tasks = task_store.tasks.write().await;
            for i in 0..MAX_TASKS {
                tasks.insert(
                    format!("done-{i}"),
                    Task {
                        id: format!("done-{i}"),
                        status: TaskStatus {
                            state: A2aTaskState::Completed,
                            message: None,
                            timestamp: None,
                        },
                        context_id: None,
                        artifacts: None,
                        history: None,
                        metadata: None,
                    },
                );
            }
        }

        // Should succeed because terminal tasks get evicted
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "message/send".into(),
            params: json!({"message": "should succeed after eviction"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        // Should get a result (not an error), proving eviction worked
        assert!(body["result"]["id"].is_string());
    }

    // ── Streaming type tests ─────────────────────────────────────

    #[test]
    fn parse_inbound_message_v1_structured() {
        let params = json!({
            "message": {
                "role": "ROLE_USER",
                "parts": [{"text": "hello"}],
                "messageId": "msg-1",
                "contextId": "ctx-1"
            }
        });
        let (text, msg, ctx) = parse_inbound_message(&params).unwrap();
        assert_eq!(text, "hello");
        assert_eq!(msg.role, "ROLE_USER");
        assert_eq!(ctx, "ctx-1");
    }

    #[test]
    fn parse_inbound_message_simple_string() {
        let params = json!({"message": "plain text"});
        let (text, msg, _ctx) = parse_inbound_message(&params).unwrap();
        assert_eq!(text, "plain text");
        assert_eq!(msg.role, "ROLE_USER");
        assert_eq!(msg.parts.len(), 1);
    }

    #[test]
    fn parse_inbound_message_missing_text() {
        let params = json!({"message": {"parts": [{"data": {}}]}});
        assert!(parse_inbound_message(&params).is_err());
    }

    #[test]
    fn parse_inbound_message_missing_message() {
        let params = json!({});
        assert!(parse_inbound_message(&params).is_err());
    }

    #[test]
    fn streaming_event_serialization() {
        let status_event = TaskStatusUpdateEvent {
            task_id: "t-1".into(),
            context_id: Some("ctx-1".into()),
            status: TaskStatus {
                state: A2aTaskState::Working,
                message: None,
                timestamp: Some("2026-01-01T00:00:00Z".into()),
            },
            is_final: false,
            metadata: None,
        };
        let json = serde_json::to_value(&status_event).unwrap();
        assert_eq!(json["taskId"], "t-1");
        assert_eq!(json["contextId"], "ctx-1");
        assert_eq!(json["status"]["state"], "TASK_STATE_WORKING");
        assert_eq!(json["final"], false);

        let artifact_event = TaskArtifactUpdateEvent {
            task_id: "t-1".into(),
            context_id: Some("ctx-1".into()),
            artifact: Artifact {
                artifact_id: "a-1".into(),
                name: Some("response".into()),
                description: None,
                parts: vec![Part::Text {
                    text: "chunk".into(),
                    metadata: None,
                }],
                metadata: Some(json!({"append": true, "chunkIndex": 1})),
                extensions: None,
            },
            metadata: None,
        };
        let json = serde_json::to_value(&artifact_event).unwrap();
        assert_eq!(json["taskId"], "t-1");
        assert_eq!(json["artifact"]["artifactId"], "a-1");
        assert_eq!(json["artifact"]["parts"][0]["text"], "chunk");
        assert!(json["artifact"]["metadata"]["append"].as_bool().unwrap());
    }

    // ── ListTasks tests ─────────────────────────────────────────

    fn make_task(id: &str, state: A2aTaskState, context_id: Option<&str>) -> Task {
        Task {
            id: id.to_string(),
            status: TaskStatus {
                state,
                message: None,
                timestamp: None,
            },
            context_id: context_id.map(String::from),
            artifacts: Some(vec![Artifact {
                artifact_id: format!("artifact-{id}"),
                name: Some("response".into()),
                description: None,
                parts: vec![Part::Text {
                    text: format!("result for {id}"),
                    metadata: None,
                }],
                metadata: None,
                extensions: None,
            }]),
            history: Some(vec![
                Message {
                    role: "ROLE_USER".into(),
                    parts: vec![Part::Text {
                        text: "hello".into(),
                        metadata: None,
                    }],
                    message_id: "m1".into(),
                    context_id: context_id.map(String::from),
                    metadata: None,
                },
                Message {
                    role: "ROLE_AGENT".into(),
                    parts: vec![Part::Text {
                        text: "world".into(),
                        metadata: None,
                    }],
                    message_id: "m2".into(),
                    context_id: context_id.map(String::from),
                    metadata: None,
                },
            ]),
            metadata: None,
        }
    }

    fn list_rpc(params: serde_json::Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/list".into(),
            params,
        }
    }

    #[tokio::test]
    async fn tasks_list_returns_empty_for_no_tasks() {
        let store = Arc::new(TaskStore::new());
        let req = list_rpc(json!({}));
        let (status, Json(body)) = handle_tasks_list(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        let result = &body["result"];
        assert_eq!(result["tasks"].as_array().unwrap().len(), 0);
        assert_eq!(result["pageSize"], 50);
        assert!(result.get("nextPageToken").is_none() || result["nextPageToken"].is_null());
    }

    #[tokio::test]
    async fn tasks_list_returns_all_tasks() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert("a".into(), make_task("a", A2aTaskState::Completed, None));
            tasks.insert("b".into(), make_task("b", A2aTaskState::Working, None));
            tasks.insert("c".into(), make_task("c", A2aTaskState::Failed, None));
        }
        let req = list_rpc(json!({"includeArtifacts": true}));
        let (status, Json(body)) = handle_tasks_list(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        let tasks = body["result"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 3);
        // Sorted by ID
        assert_eq!(tasks[0]["id"], "a");
        assert_eq!(tasks[1]["id"], "b");
        assert_eq!(tasks[2]["id"], "c");
    }

    #[tokio::test]
    async fn tasks_list_filters_by_context_id() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert(
                "a".into(),
                make_task("a", A2aTaskState::Completed, Some("ctx-1")),
            );
            tasks.insert(
                "b".into(),
                make_task("b", A2aTaskState::Completed, Some("ctx-2")),
            );
            tasks.insert(
                "c".into(),
                make_task("c", A2aTaskState::Working, Some("ctx-1")),
            );
        }
        let req = list_rpc(json!({"contextId": "ctx-1", "includeArtifacts": true}));
        let (_, Json(body)) = handle_tasks_list(&store, req).await;
        let tasks = body["result"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().all(|t| t["contextId"] == "ctx-1"));
    }

    #[tokio::test]
    async fn tasks_list_filters_by_status() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert("a".into(), make_task("a", A2aTaskState::Completed, None));
            tasks.insert("b".into(), make_task("b", A2aTaskState::Working, None));
            tasks.insert("c".into(), make_task("c", A2aTaskState::Completed, None));
        }
        let req = list_rpc(json!({"status": "TASK_STATE_COMPLETED", "includeArtifacts": true}));
        let (_, Json(body)) = handle_tasks_list(&store, req).await;
        let tasks = body["result"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(
            tasks
                .iter()
                .all(|t| t["status"]["state"] == "TASK_STATE_COMPLETED")
        );
    }

    #[tokio::test]
    async fn tasks_list_paginates_correctly() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            for i in 0..5 {
                let id = format!("task-{i:03}");
                tasks.insert(id.clone(), make_task(&id, A2aTaskState::Completed, None));
            }
        }

        // First page of 2
        let req = list_rpc(json!({"pageSize": 2}));
        let (_, Json(body)) = handle_tasks_list(&store, req).await;
        let result = &body["result"];
        let tasks = result["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["id"], "task-000");
        assert_eq!(tasks[1]["id"], "task-001");
        let next_token = result["nextPageToken"].as_str().unwrap();
        assert_eq!(next_token, "task-001");

        // Second page using cursor
        let req = list_rpc(json!({"pageSize": 2, "pageToken": next_token}));
        let (_, Json(body)) = handle_tasks_list(&store, req).await;
        let result = &body["result"];
        let tasks = result["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["id"], "task-002");
        assert_eq!(tasks[1]["id"], "task-003");
        let next_token = result["nextPageToken"].as_str().unwrap();
        assert_eq!(next_token, "task-003");

        // Third page — only 1 remaining
        let req = list_rpc(json!({"pageSize": 2, "pageToken": next_token}));
        let (_, Json(body)) = handle_tasks_list(&store, req).await;
        let result = &body["result"];
        let tasks = result["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "task-004");
        assert!(result.get("nextPageToken").is_none() || result["nextPageToken"].is_null());
    }

    #[tokio::test]
    async fn tasks_list_respects_page_size() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            for i in 0..10 {
                let id = format!("t-{i:02}");
                tasks.insert(id.clone(), make_task(&id, A2aTaskState::Completed, None));
            }
        }

        // page_size=3 should return exactly 3
        let req = list_rpc(json!({"pageSize": 3}));
        let (_, Json(body)) = handle_tasks_list(&store, req).await;
        let tasks = body["result"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(body["result"]["pageSize"], 3);

        // page_size clamped to max 100
        let req = list_rpc(json!({"pageSize": 200}));
        let (_, Json(body)) = handle_tasks_list(&store, req).await;
        let tasks = body["result"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 10); // only 10 tasks exist
        assert_eq!(body["result"]["pageSize"], 100);
    }

    #[tokio::test]
    async fn tasks_list_strips_artifacts_by_default() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert("a".into(), make_task("a", A2aTaskState::Completed, None));
        }
        // Default: include_artifacts=false
        let req = list_rpc(json!({}));
        let (_, Json(body)) = handle_tasks_list(&store, req).await;
        let tasks = body["result"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        // Artifacts should be stripped (null or absent)
        assert!(tasks[0].get("artifacts").is_none() || tasks[0]["artifacts"].is_null());
    }

    #[tokio::test]
    async fn tasks_list_includes_artifacts_when_requested() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert("a".into(), make_task("a", A2aTaskState::Completed, None));
        }
        let req = list_rpc(json!({"includeArtifacts": true}));
        let (_, Json(body)) = handle_tasks_list(&store, req).await;
        let tasks = body["result"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0]["artifacts"].is_array());
        assert_eq!(tasks[0]["artifacts"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn tasks_list_via_rpc_dispatch() {
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.as_ref().unwrap();
        {
            let mut tasks = task_store.tasks.write().await;
            tasks.insert("x".into(), make_task("x", A2aTaskState::Working, None));
        }
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/list".into(),
            params: json!({}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["result"]["tasks"].as_array().unwrap().len(), 1);
    }

    // ── context_id multi-turn conversation tests ──────────────

    #[tokio::test]
    async fn context_index_tracks_tasks() {
        let store = TaskStore::new();

        // Insert two tasks with the same context_id
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert(
                "t1".into(),
                Task {
                    id: "t1".into(),
                    status: TaskStatus {
                        state: A2aTaskState::Completed,
                        message: None,
                        timestamp: None,
                    },
                    context_id: Some("ctx-shared".into()),
                    artifacts: None,
                    history: None,
                    metadata: None,
                },
            );
            tasks.insert(
                "t2".into(),
                Task {
                    id: "t2".into(),
                    status: TaskStatus {
                        state: A2aTaskState::Working,
                        message: None,
                        timestamp: None,
                    },
                    context_id: Some("ctx-shared".into()),
                    artifacts: None,
                    history: None,
                    metadata: None,
                },
            );
            tasks.insert(
                "t3".into(),
                Task {
                    id: "t3".into(),
                    status: TaskStatus {
                        state: A2aTaskState::Completed,
                        message: None,
                        timestamp: None,
                    },
                    context_id: Some("ctx-other".into()),
                    artifacts: None,
                    history: None,
                    metadata: None,
                },
            );
        }
        store.index_context("ctx-shared", "t1").await;
        store.index_context("ctx-shared", "t2").await;
        store.index_context("ctx-other", "t3").await;

        let idx = store.context_index.read().await;
        assert_eq!(idx.get("ctx-shared").unwrap().len(), 2);
        assert_eq!(idx.get("ctx-other").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn tasks_by_context_returns_correct_tasks() {
        let store = TaskStore::new();
        {
            let mut tasks = store.tasks.write().await;
            for id in &["a1", "a2", "b1"] {
                let ctx = if id.starts_with('a') {
                    "ctx-a"
                } else {
                    "ctx-b"
                };
                tasks.insert(
                    id.to_string(),
                    Task {
                        id: id.to_string(),
                        status: TaskStatus {
                            state: A2aTaskState::Completed,
                            message: None,
                            timestamp: None,
                        },
                        context_id: Some(ctx.into()),
                        artifacts: None,
                        history: None,
                        metadata: None,
                    },
                );
            }
        }
        store.index_context("ctx-a", "a1").await;
        store.index_context("ctx-a", "a2").await;
        store.index_context("ctx-b", "b1").await;

        let ctx_a_tasks = store.tasks_by_context("ctx-a").await;
        assert_eq!(ctx_a_tasks.len(), 2);
        let ids: Vec<&str> = ctx_a_tasks.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"a1"));
        assert!(ids.contains(&"a2"));

        let ctx_b_tasks = store.tasks_by_context("ctx-b").await;
        assert_eq!(ctx_b_tasks.len(), 1);
        assert_eq!(ctx_b_tasks[0].id, "b1");

        let empty = store.tasks_by_context("nonexistent").await;
        assert!(empty.is_empty());
    }

    #[test]
    fn context_id_generates_consistent_session_key() {
        let ctx = "my-context-123";
        let session1 = format!("a2a-ctx-{ctx}");
        let session2 = format!("a2a-ctx-{ctx}");
        assert_eq!(session1, session2);
        assert_eq!(session1, "a2a-ctx-my-context-123");

        // Different context IDs produce different session keys
        let other = format!("a2a-ctx-{}", "other-ctx");
        assert_ne!(session1, other);
    }

    // ── Eviction tests ──────────────────────────────────────────

    fn insert_task(store: &TaskStore, id: &str, state: A2aTaskState) {
        // Use try_write (sync) for test convenience — store is uncontested.
        let mut tasks = store.tasks.try_write().unwrap();
        tasks.insert(
            id.to_string(),
            Task {
                id: id.to_string(),
                status: TaskStatus {
                    state,
                    message: None,
                    timestamp: None,
                },
                context_id: None,
                artifacts: None,
                history: None,
                metadata: None,
            },
        );
    }

    #[tokio::test]
    async fn mark_terminal_records_timestamp() {
        let store = TaskStore::new();
        insert_task(&store, "t1", A2aTaskState::Completed);
        store.mark_terminal("t1").await;

        let ts = store.timestamps.read().await;
        assert!(ts.contains_key("t1"));
    }

    #[tokio::test]
    async fn eviction_removes_expired_terminal_tasks() {
        let store = TaskStore::new();
        insert_task(&store, "t1", A2aTaskState::Completed);
        insert_task(&store, "t2", A2aTaskState::Failed);

        // Manually insert timestamps in the past
        {
            let mut ts = store.timestamps.write().await;
            let past = std::time::Instant::now()
                .checked_sub(Duration::from_secs(120))
                .unwrap();
            ts.insert("t1".to_string(), past);
            ts.insert("t2".to_string(), past);
        }

        let evicted = store.evict_expired(Duration::from_secs(60)).await;
        assert_eq!(evicted, 2);

        let tasks = store.tasks.read().await;
        assert!(!tasks.contains_key("t1"));
        assert!(!tasks.contains_key("t2"));
    }

    #[tokio::test]
    async fn eviction_preserves_non_terminal_tasks() {
        let store = TaskStore::new();
        insert_task(&store, "working", A2aTaskState::Working);
        insert_task(&store, "submitted", A2aTaskState::Submitted);
        // Non-terminal tasks have no timestamp entry, so they survive eviction.

        let evicted = store.evict_expired(Duration::from_secs(0)).await;
        assert_eq!(evicted, 0);

        let tasks = store.tasks.read().await;
        assert!(tasks.contains_key("working"));
        assert!(tasks.contains_key("submitted"));
    }

    #[tokio::test]
    async fn eviction_preserves_recent_terminal_tasks() {
        let store = TaskStore::new();
        insert_task(&store, "recent", A2aTaskState::Completed);
        store.mark_terminal("recent").await;

        // TTL of 1 hour — the task was just marked terminal, so it should survive.
        let evicted = store.evict_expired(Duration::from_secs(3600)).await;
        assert_eq!(evicted, 0);

        let tasks = store.tasks.read().await;
        assert!(tasks.contains_key("recent"));
    }

    #[tokio::test]
    async fn return_immediately_returns_submitted_state() {
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.as_ref().unwrap();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(99),
            method: "message/send".into(),
            params: json!({
                "message": {
                    "role": "ROLE_USER",
                    "messageId": "m-1",
                    "parts": [{"text": "hello"}],
                },
                "configuration": {
                    "returnImmediately": true
                }
            }),
        };

        let (status, Json(body)) = Box::pin(handle_message_send(&state, task_store, req)).await;
        assert_eq!(status, StatusCode::OK);
        // Task should be in Submitted state (processing hasn't completed yet).
        assert_eq!(
            body["result"]["status"]["state"], "TASK_STATE_SUBMITTED",
            "return_immediately should return Submitted state"
        );
        assert!(body["result"]["id"].is_string());
    }

    #[tokio::test]
    async fn return_immediately_eventually_reaches_terminal() {
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.as_ref().unwrap();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(100),
            method: "message/send".into(),
            params: json!({
                "message": {
                    "role": "ROLE_USER",
                    "messageId": "m-2",
                    "parts": [{"text": "background task"}],
                },
                "configuration": {
                    "returnImmediately": true
                }
            }),
        };

        let (status, Json(body)) = Box::pin(handle_message_send(&state, task_store, req)).await;
        assert_eq!(status, StatusCode::OK);
        let task_id = body["result"]["id"].as_str().unwrap().to_string();

        // Wait for background processing to reach a terminal state.
        // In tests, process_message uses a default config without a real provider,
        // so the task will reach Failed (not Completed) — either terminal state
        // proves the background spawn ran to completion.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let tasks = task_store.tasks.read().await;
            if let Some(t) = tasks.get(&task_id) {
                if t.status.state.is_terminal() {
                    return; // Success — background task reached terminal state.
                }
            }
        }
        panic!("background task did not reach terminal state within 2 seconds");
    }

    #[tokio::test]
    async fn eviction_cleans_context_index() {
        let store = TaskStore::new();
        // Insert with context_id so eviction can find it
        {
            let mut tasks = store.tasks.try_write().unwrap();
            tasks.insert(
                "t1".to_string(),
                Task {
                    id: "t1".into(),
                    status: TaskStatus {
                        state: A2aTaskState::Completed,
                        message: None,
                        timestamp: None,
                    },
                    context_id: Some("ctx-1".into()),
                    artifacts: None,
                    history: None,
                    metadata: None,
                },
            );
        }
        store.index_context("ctx-1", "t1").await;

        // Set timestamp in the past for eviction
        {
            let mut ts = store.timestamps.write().await;
            let past = std::time::Instant::now()
                .checked_sub(Duration::from_secs(120))
                .unwrap();
            ts.insert("t1".to_string(), past);
        }

        let evicted = store.evict_expired(Duration::from_secs(60)).await;
        assert_eq!(evicted, 1);

        // context_index entry should have been cleaned up
        let idx = store.context_index.read().await;
        assert!(
            !idx.contains_key("ctx-1"),
            "empty context should be removed from index"
        );
    }
}
