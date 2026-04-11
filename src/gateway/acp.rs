//! # ACP (Agent Communication Protocol) — v0.2.0 Implementation
//! Endpoints: /ping, /agents, /runs, /session

use crate::config::{AcpAgentDef, AcpCapability, Config};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::AppState;

// ── Error Model ─────────────────────────────────────────────────

/// ACP error codes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcpErrorCode {
    ServerError,
    InvalidInput,
    NotFound,
    Unauthorized,
}

/// ACP error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpError {
    pub code: AcpErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl IntoResponse for AcpError {
    fn into_response(self) -> axum::response::Response {
        let status = match self.code {
            AcpErrorCode::ServerError => StatusCode::INTERNAL_SERVER_ERROR,
            AcpErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
            AcpErrorCode::NotFound => StatusCode::NOT_FOUND,
            AcpErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        };
        (status, Json(self)).into_response()
    }
}

// ── Content Encoding ────────────────────────────────────────────

/// Encoding for message part content.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContentEncoding {
    #[default]
    Plain,
    Base64,
}

// ── Part Metadata ───────────────────────────────────────────────

/// Citation metadata for a message part.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Trajectory metadata for a message part.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<Value>,
}

/// Tagged metadata for a message part.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PartMetadata {
    #[serde(rename = "citation")]
    Citation(CitationMetadata),
    #[serde(rename = "trajectory")]
    Trajectory(TrajectoryMetadata),
}

// ── Message Types ───────────────────────────────────────────────

fn default_content_type() -> String {
    "text/plain".to_string()
}

/// A single part within an ACP message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default = "default_content_type")]
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_encoding: Option<ContentEncoding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<PartMetadata>,
}

/// An ACP message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub parts: Vec<MessagePart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

// ── Run Types ───────────────────────────────────────────────────

/// Status of an ACP run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Created,
    InProgress,
    Awaiting,
    Cancelling,
    Cancelled,
    Completed,
    Failed,
}

impl RunStatus {
    /// Whether this status represents a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        )
    }
}

/// Mode for run execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    Sync,
    Async,
    Stream,
}

/// An ACP run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub agent_name: String,
    pub session_id: String,
    pub run_id: String,
    pub status: RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub await_request: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AcpError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

/// Request to create a new run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCreateRequest {
    pub agent_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub input: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<RunMode>,
}

/// Request to resume an awaiting run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResumeRequest {
    pub run_id: String,
    pub await_resume: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<RunMode>,
}

fn default_limit() -> usize {
    10
}

/// Query parameters for listing agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsListQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

// ── Event Types (SSE) ───────────────────────────────────────────

/// Server-sent event types for ACP streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "message.created")]
    MessageCreated { message: Message },
    #[serde(rename = "message.part")]
    MessagePart { part: MessagePart },
    #[serde(rename = "message.completed")]
    MessageCompleted { message: Message },
    #[serde(rename = "run.created")]
    RunCreated { run: Run },
    #[serde(rename = "run.in-progress")]
    RunInProgress { run: Run },
    #[serde(rename = "run.awaiting")]
    RunAwaiting { run: Run },
    #[serde(rename = "run.completed")]
    RunCompleted { run: Run },
    #[serde(rename = "run.failed")]
    RunFailed { run: Run },
    #[serde(rename = "run.cancelled")]
    RunCancelled { run: Run },
    #[serde(rename = "error")]
    Error { error: AcpError },
    #[serde(rename = "generic")]
    Generic { data: Value },
}

// ── Response Wrappers ───────────────────────────────────────────

/// Metadata for an agent manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<AcpCapability>,
    #[serde(default)]
    pub framework: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Public agent manifest returned by the agents list endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_content_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_content_types: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AgentMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Response for listing agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsListResponse {
    pub agents: Vec<AgentManifest>,
}

/// Response for listing run events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEventsListResponse {
    pub events: Vec<Event>,
}

/// ACP session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpSession {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<String>,
}

// ── RunStore ────────────────────────────────────────────────────

/// In-memory store for ACP run lifecycle management.
pub struct RunStore {
    runs: RwLock<HashMap<String, Run>>,
    events: RwLock<HashMap<String, Vec<Event>>>,
    session_runs: RwLock<HashMap<String, Vec<String>>>,
    timestamps: RwLock<HashMap<String, std::time::Instant>>,
}

impl RunStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            runs: RwLock::new(HashMap::new()),
            events: RwLock::new(HashMap::new()),
            session_runs: RwLock::new(HashMap::new()),
            timestamps: RwLock::new(HashMap::new()),
        }
    }

    /// Insert a run, index by session if present, create empty event vec.
    pub async fn insert(&self, run: Run) {
        let run_id = run.run_id.clone();
        let session_id = run.session_id.clone();

        self.session_runs
            .write()
            .await
            .entry(session_id)
            .or_default()
            .push(run_id.clone());

        self.events.write().await.insert(run_id.clone(), Vec::new());
        self.runs.write().await.insert(run_id, run);
    }

    /// Get a run by ID (cloned).
    pub async fn get(&self, run_id: &str) -> Option<Run> {
        self.runs.read().await.get(run_id).cloned()
    }

    /// Update a run's status. If terminal, set `finished_at` and record eviction timestamp.
    pub async fn update_status(&self, run_id: &str, status: RunStatus) {
        let mut runs = self.runs.write().await;
        if let Some(run) = runs.get_mut(run_id) {
            let is_terminal = status.is_terminal();
            run.status = status;
            if is_terminal {
                run.finished_at = Some(Utc::now());
                drop(runs);
                self.timestamps
                    .write()
                    .await
                    .insert(run_id.to_string(), std::time::Instant::now());
            }
        }
    }

    /// Set the output messages for a run.
    pub async fn set_output(&self, run_id: &str, output: Vec<Message>) {
        if let Some(run) = self.runs.write().await.get_mut(run_id) {
            run.output = output;
        }
    }

    /// Set an error on a run.
    pub async fn set_error(&self, run_id: &str, error: AcpError) {
        if let Some(run) = self.runs.write().await.get_mut(run_id) {
            run.error = Some(error);
        }
    }

    /// Push an event for a run.
    pub async fn push_event(&self, run_id: &str, event: Event) {
        self.events
            .write()
            .await
            .entry(run_id.to_string())
            .or_default()
            .push(event);
    }

    /// Get all events for a run (cloned).
    pub async fn get_events(&self, run_id: &str) -> Vec<Event> {
        self.events
            .read()
            .await
            .get(run_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get all run IDs associated with a session.
    pub async fn runs_for_session(&self, session_id: &str) -> Vec<String> {
        self.session_runs
            .read()
            .await
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Count non-terminal (active) runs.
    pub async fn active_run_count(&self) -> usize {
        self.runs
            .read()
            .await
            .values()
            .filter(|r| !r.status.is_terminal())
            .count()
    }

    /// Remove terminal runs whose eviction timestamp is older than `ttl`. Returns count removed.
    pub async fn evict_expired(&self, ttl: std::time::Duration) -> usize {
        let now = std::time::Instant::now();
        let expired: Vec<String> = self
            .timestamps
            .read()
            .await
            .iter()
            .filter(|(_, ts)| now.duration_since(**ts) >= ttl)
            .map(|(id, _)| id.clone())
            .collect();

        let count = expired.len();
        if count == 0 {
            return 0;
        }

        let mut runs = self.runs.write().await;
        let mut events = self.events.write().await;
        let mut session_runs = self.session_runs.write().await;
        let mut timestamps = self.timestamps.write().await;

        for id in &expired {
            if let Some(run) = runs.remove(id) {
                if let Some(ids) = session_runs.get_mut(&run.session_id) {
                    ids.retain(|rid| rid != id);
                }
            }
            events.remove(id);
            timestamps.remove(id);
        }

        count
    }
}

// ── Agent Registry ─────────────────────────────────────────────

/// Shared registry of ACP agent definitions.
pub type AgentRegistry = Arc<Vec<AcpAgentDef>>;

/// Convert a config `AcpAgentDef` into an API `AgentManifest`.
pub fn manifest_from_def(def: &AcpAgentDef) -> AgentManifest {
    AgentManifest {
        name: def.name.clone(),
        description: Some(def.description.clone()),
        input_content_types: def.input_content_types.clone(),
        output_content_types: def.output_content_types.clone(),
        metadata: Some(AgentMetadata {
            capabilities: def.capabilities.clone(),
            framework: "hrafn".to_string(),
            tags: vec![],
        }),
        status: Some("ready".to_string()),
    }
}

/// Build the agent registry from config. If no agents are configured, auto-register
/// a default agent using the identity name (or "hrafn").
pub fn build_agent_registry(config: &Config) -> AgentRegistry {
    if !config.acp.agents.is_empty() {
        return Arc::new(config.acp.agents.clone());
    }

    let raw_name = config
        .identity
        .aieos_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("hrafn")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_lowercase();

    Arc::new(vec![AcpAgentDef {
        name: raw_name,
        description: "Default Hrafn agent".to_string(),
        system_prompt: None,
        model: None,
        tools: vec![],
        input_content_types: vec!["text/plain".to_string()],
        output_content_types: vec!["text/plain".to_string()],
        capabilities: vec![],
    }])
}

// ── Discovery Handlers ─────────────────────────────────────────

/// `GET /ping` — liveness probe.
pub async fn handle_ping() -> impl IntoResponse {
    Json(serde_json::json!({}))
}

/// `GET /agents` — paginated agent listing.
pub async fn handle_agents_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AgentsListQuery>,
) -> Result<Json<AgentsListResponse>, AcpError> {
    let acp_config = { state.config.lock().acp.clone() };
    check_bearer_auth(&headers, &acp_config)?;

    let registry = &state.acp_agent_registry;
    let manifests: Vec<AgentManifest> = registry
        .iter()
        .skip(query.offset)
        .take(query.limit)
        .map(manifest_from_def)
        .collect();
    Ok(Json(AgentsListResponse { agents: manifests }))
}

/// `GET /agents/{name}` — single agent lookup.
pub async fn handle_agent_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<AgentManifest>, AcpError> {
    let acp_config = { state.config.lock().acp.clone() };
    check_bearer_auth(&headers, &acp_config)?;

    let registry = &state.acp_agent_registry;
    registry
        .iter()
        .find(|def| def.name == name)
        .map(|def| Json(manifest_from_def(def)))
        .ok_or_else(|| AcpError {
            code: AcpErrorCode::NotFound,
            message: format!("agent '{name}' not found"),
            data: None,
        })
}

// ── Bearer Auth ────────────────────────────────────────────────

/// Validate the `Authorization: Bearer <token>` header against the configured
/// ACP bearer token. Returns `Ok(())` when no token is configured (open access)
/// or when the provided token matches.
fn check_bearer_auth(
    headers: &HeaderMap,
    config: &crate::config::AcpConfig,
) -> Result<(), AcpError> {
    let Some(expected) = &config.bearer_token else {
        return Ok(()); // no auth configured
    };
    let Some(auth) = headers.get(header::AUTHORIZATION) else {
        return Err(AcpError {
            code: AcpErrorCode::Unauthorized,
            message: "Missing Authorization header".into(),
            data: None,
        });
    };
    let auth_str = auth.to_str().unwrap_or("");
    let token = auth_str.strip_prefix("Bearer ").unwrap_or("");
    if !crate::security::pairing::constant_time_eq(token, expected) {
        return Err(AcpError {
            code: AcpErrorCode::Unauthorized,
            message: "Invalid bearer token".into(),
            data: None,
        });
    }
    Ok(())
}

// ── Run Handlers ──────────────────────────────────────────────

/// `POST /runs` — create and execute a new run.
///
/// Dispatches to sync, async, or stream mode based on `mode` in the request
/// (defaults to sync when omitted).
pub async fn handle_run_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RunCreateRequest>,
) -> Result<axum::response::Response, AcpError> {
    // ── Auth ──
    let acp_config = { state.config.lock().acp.clone() };
    check_bearer_auth(&headers, &acp_config)?;

    // ── Validate agent ──
    let registry = &state.acp_agent_registry;
    let agent_def = registry
        .iter()
        .find(|def| def.name == req.agent_name)
        .cloned()
        .ok_or_else(|| AcpError {
            code: AcpErrorCode::NotFound,
            message: format!("agent '{}' not found", req.agent_name),
            data: None,
        })?;

    // ── Concurrency check ──
    let active = state.acp_run_store.active_run_count().await;
    if active >= acp_config.max_concurrent_runs {
        return Err(AcpError {
            code: AcpErrorCode::ServerError,
            message: format!(
                "max concurrent runs ({}) exceeded",
                acp_config.max_concurrent_runs
            ),
            data: None,
        });
    }

    // ── Build run ──
    let run_id = uuid::Uuid::new_v4().to_string();
    let session_id = req
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mode = req.mode.unwrap_or(RunMode::Sync);

    // Extract user message: join all parts' content from all input messages
    let user_message: String = req
        .input
        .iter()
        .flat_map(|msg| msg.parts.iter())
        .filter_map(|part| part.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");

    let run = Run {
        agent_name: req.agent_name.clone(),
        session_id: session_id.clone(),
        run_id: run_id.clone(),
        status: RunStatus::Created,
        await_request: None,
        output: vec![],
        error: None,
        created_at: Some(Utc::now()),
        finished_at: None,
    };

    state.acp_run_store.insert(run.clone()).await;

    match mode {
        RunMode::Sync => handle_run_sync(state, run, agent_def, user_message).await,
        RunMode::Async => handle_run_async(state, run, agent_def, user_message).await,
        RunMode::Stream => handle_run_stream(state, run, agent_def, user_message).await,
    }
}

/// Execute a run synchronously — block until the agent completes.
async fn handle_run_sync(
    state: AppState,
    run: Run,
    agent_def: AcpAgentDef,
    user_message: String,
) -> Result<axum::response::Response, AcpError> {
    let run_id = run.run_id.clone();

    state
        .acp_run_store
        .update_status(&run_id, RunStatus::InProgress)
        .await;

    let result = execute_agent(&state, &agent_def, &user_message).await;

    match result {
        Ok(response_text) => {
            let output = vec![Message {
                role: "agent".into(),
                parts: vec![MessagePart {
                    name: None,
                    content_type: "text/plain".into(),
                    content: Some(response_text),
                    content_encoding: None,
                    content_url: None,
                    metadata: None,
                }],
                created_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
            }];
            state.acp_run_store.set_output(&run_id, output).await;
            state
                .acp_run_store
                .update_status(&run_id, RunStatus::Completed)
                .await;
        }
        Err(e) => {
            let error = AcpError {
                code: AcpErrorCode::ServerError,
                message: format!("{e}"),
                data: None,
            };
            state.acp_run_store.set_error(&run_id, error).await;
            state
                .acp_run_store
                .update_status(&run_id, RunStatus::Failed)
                .await;
        }
    }

    let completed_run = state
        .acp_run_store
        .get(&run_id)
        .await
        .ok_or_else(|| AcpError {
            code: AcpErrorCode::ServerError,
            message: "run disappeared from store".into(),
            data: None,
        })?;

    Ok((StatusCode::OK, Json(completed_run)).into_response())
}

/// Execute a run asynchronously — return 202 immediately, spawn background task.
async fn handle_run_async(
    state: AppState,
    run: Run,
    agent_def: AcpAgentDef,
    user_message: String,
) -> Result<axum::response::Response, AcpError> {
    let run_id = run.run_id.clone();

    // Spawn background execution
    let bg_state = state.clone();
    let bg_run_id = run_id.clone();
    tokio::spawn(async move {
        bg_state
            .acp_run_store
            .update_status(&bg_run_id, RunStatus::InProgress)
            .await;

        let result = execute_agent(&bg_state, &agent_def, &user_message).await;

        match result {
            Ok(response_text) => {
                let output = vec![Message {
                    role: "agent".into(),
                    parts: vec![MessagePart {
                        name: None,
                        content_type: "text/plain".into(),
                        content: Some(response_text),
                        content_encoding: None,
                        content_url: None,
                        metadata: None,
                    }],
                    created_at: Some(Utc::now()),
                    completed_at: Some(Utc::now()),
                }];
                bg_state.acp_run_store.set_output(&bg_run_id, output).await;
                bg_state
                    .acp_run_store
                    .update_status(&bg_run_id, RunStatus::Completed)
                    .await;
            }
            Err(e) => {
                let error = AcpError {
                    code: AcpErrorCode::ServerError,
                    message: format!("{e}"),
                    data: None,
                };
                bg_state.acp_run_store.set_error(&bg_run_id, error).await;
                bg_state
                    .acp_run_store
                    .update_status(&bg_run_id, RunStatus::Failed)
                    .await;
            }
        }
    });

    // Return the run as it was at creation time (status: Created)
    let created_run = state
        .acp_run_store
        .get(&run_id)
        .await
        .ok_or_else(|| AcpError {
            code: AcpErrorCode::ServerError,
            message: "run disappeared from store".into(),
            data: None,
        })?;

    Ok((StatusCode::ACCEPTED, Json(created_run)).into_response())
}

/// Execute a run in stream mode — return SSE event stream.
async fn handle_run_stream(
    state: AppState,
    run: Run,
    agent_def: AcpAgentDef,
    user_message: String,
) -> Result<axum::response::Response, AcpError> {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use std::convert::Infallible;

    let run_id = run.run_id.clone();

    // Channel for SSE events — capacity enough for bursts
    let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<Result<SseEvent, Infallible>>(256);

    // Emit RunCreated
    let _ = sse_tx
        .send(Ok(SseEvent::default().event("run.created").data(
            serde_json::to_string(&Event::RunCreated { run: run.clone() }).unwrap_or_default(),
        )))
        .await;

    // Spawn the streaming agent execution
    let bg_state = state.clone();
    let bg_run_id = run_id.clone();
    tokio::spawn(async move {
        bg_state
            .acp_run_store
            .update_status(&bg_run_id, RunStatus::InProgress)
            .await;

        // Emit RunInProgress
        if let Some(r) = bg_state.acp_run_store.get(&bg_run_id).await {
            let _ = sse_tx
                .send(Ok(SseEvent::default().event("run.in-progress").data(
                    serde_json::to_string(&Event::RunInProgress { run: r }).unwrap_or_default(),
                )))
                .await;
        }

        // Emit MessageCreated
        let msg_created = Message {
            role: "agent".into(),
            parts: vec![],
            created_at: Some(Utc::now()),
            completed_at: None,
        };
        let _ = sse_tx
            .send(Ok(SseEvent::default().event("message.created").data(
                serde_json::to_string(&Event::MessageCreated {
                    message: msg_created,
                })
                .unwrap_or_default(),
            )))
            .await;

        // Execute with streaming
        let result = execute_agent_streamed(&bg_state, &agent_def, &user_message, &sse_tx).await;

        match result {
            Ok(response_text) => {
                // Emit MessageCompleted
                let msg_completed = Message {
                    role: "agent".into(),
                    parts: vec![MessagePart {
                        name: None,
                        content_type: "text/plain".into(),
                        content: Some(response_text.clone()),
                        content_encoding: None,
                        content_url: None,
                        metadata: None,
                    }],
                    created_at: Some(Utc::now()),
                    completed_at: Some(Utc::now()),
                };
                let _ = sse_tx
                    .send(Ok(SseEvent::default().event("message.completed").data(
                        serde_json::to_string(&Event::MessageCompleted {
                            message: msg_completed,
                        })
                        .unwrap_or_default(),
                    )))
                    .await;

                let output = vec![Message {
                    role: "agent".into(),
                    parts: vec![MessagePart {
                        name: None,
                        content_type: "text/plain".into(),
                        content: Some(response_text),
                        content_encoding: None,
                        content_url: None,
                        metadata: None,
                    }],
                    created_at: Some(Utc::now()),
                    completed_at: Some(Utc::now()),
                }];
                bg_state.acp_run_store.set_output(&bg_run_id, output).await;
                bg_state
                    .acp_run_store
                    .update_status(&bg_run_id, RunStatus::Completed)
                    .await;

                // Emit RunCompleted
                if let Some(r) = bg_state.acp_run_store.get(&bg_run_id).await {
                    let _ = sse_tx
                        .send(Ok(SseEvent::default().event("run.completed").data(
                            serde_json::to_string(&Event::RunCompleted { run: r })
                                .unwrap_or_default(),
                        )))
                        .await;
                }
            }
            Err(e) => {
                let error = AcpError {
                    code: AcpErrorCode::ServerError,
                    message: format!("{e}"),
                    data: None,
                };
                bg_state.acp_run_store.set_error(&bg_run_id, error).await;
                bg_state
                    .acp_run_store
                    .update_status(&bg_run_id, RunStatus::Failed)
                    .await;

                // Emit RunFailed
                if let Some(r) = bg_state.acp_run_store.get(&bg_run_id).await {
                    let _ = sse_tx
                        .send(Ok(SseEvent::default().event("run.failed").data(
                            serde_json::to_string(&Event::RunFailed { run: r }).unwrap_or_default(),
                        )))
                        .await;
                }
            }
        }
        // sse_tx drops here, closing the stream
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// Build an `Agent` from a config, applying agent-def overrides, and execute a turn.
async fn execute_agent(
    state: &AppState,
    agent_def: &AcpAgentDef,
    user_message: &str,
) -> anyhow::Result<String> {
    let mut config = state.config.lock().clone();
    apply_agent_def_overrides(&mut config, agent_def);

    let mut agent = crate::agent::Agent::from_config(&config).await?;
    agent.turn(user_message).await
}

/// Build an `Agent` from a config and execute a streamed turn, mapping `TurnEvent`s
/// to ACP SSE events.
async fn execute_agent_streamed(
    state: &AppState,
    agent_def: &AcpAgentDef,
    user_message: &str,
    sse_tx: &tokio::sync::mpsc::Sender<
        Result<axum::response::sse::Event, std::convert::Infallible>,
    >,
) -> anyhow::Result<String> {
    use crate::agent::TurnEvent;

    let mut config = state.config.lock().clone();
    apply_agent_def_overrides(&mut config, agent_def);

    let mut agent = crate::agent::Agent::from_config(&config).await?;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(256);

    // Spawn a forwarder that maps TurnEvents to SSE events
    let fwd_tx = sse_tx.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(turn_event) = event_rx.recv().await {
            let part = match turn_event {
                TurnEvent::Chunk { delta } => MessagePart {
                    name: None,
                    content_type: "text/plain".into(),
                    content: Some(delta),
                    content_encoding: None,
                    content_url: None,
                    metadata: None,
                },
                TurnEvent::Thinking { delta } => MessagePart {
                    name: None,
                    content_type: "text/plain".into(),
                    content: None,
                    content_encoding: None,
                    content_url: None,
                    metadata: Some(PartMetadata::Trajectory(TrajectoryMetadata {
                        message: Some(delta),
                        tool_name: None,
                        tool_input: None,
                        tool_output: None,
                    })),
                },
                TurnEvent::ToolCall { name, args } => MessagePart {
                    name: None,
                    content_type: "text/plain".into(),
                    content: None,
                    content_encoding: None,
                    content_url: None,
                    metadata: Some(PartMetadata::Trajectory(TrajectoryMetadata {
                        message: None,
                        tool_name: Some(name),
                        tool_input: Some(args),
                        tool_output: None,
                    })),
                },
                TurnEvent::ToolResult { name, output } => MessagePart {
                    name: None,
                    content_type: "text/plain".into(),
                    content: None,
                    content_encoding: None,
                    content_url: None,
                    metadata: Some(PartMetadata::Trajectory(TrajectoryMetadata {
                        message: None,
                        tool_name: Some(name),
                        tool_input: None,
                        tool_output: Some(serde_json::Value::String(output)),
                    })),
                },
            };

            let event = Event::MessagePart { part };
            let _ = fwd_tx
                .send(Ok(axum::response::sse::Event::default()
                    .event("message.part")
                    .data(serde_json::to_string(&event).unwrap_or_default())))
                .await;
        }
    });

    let result = agent.turn_streamed(user_message, event_tx).await;

    // Wait for the forwarder to drain remaining events
    let _ = forwarder.await;

    result
}

/// Apply agent-def overrides (system_prompt, model) to a cloned config.
fn apply_agent_def_overrides(config: &mut Config, agent_def: &AcpAgentDef) {
    if let Some(ref sp) = agent_def.system_prompt {
        config.identity.aieos_inline = Some(sp.clone());
    }
    if let Some(ref model) = agent_def.model {
        config.default_model = Some(model.clone());
    }
}

/// `GET /runs/{run_id}` — retrieve run status and output.
pub async fn handle_run_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<Run>, AcpError> {
    let acp_config = { state.config.lock().acp.clone() };
    check_bearer_auth(&headers, &acp_config)?;

    state
        .acp_run_store
        .get(&run_id)
        .await
        .map(Json)
        .ok_or_else(|| AcpError {
            code: AcpErrorCode::NotFound,
            message: format!("run '{run_id}' not found"),
            data: None,
        })
}

/// `GET /runs/{run_id}/events` — retrieve stored events for a run.
pub async fn handle_run_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<RunEventsListResponse>, AcpError> {
    let acp_config = { state.config.lock().acp.clone() };
    check_bearer_auth(&headers, &acp_config)?;

    // Verify run exists
    if state.acp_run_store.get(&run_id).await.is_none() {
        return Err(AcpError {
            code: AcpErrorCode::NotFound,
            message: format!("run '{run_id}' not found"),
            data: None,
        });
    }

    let events = state.acp_run_store.get_events(&run_id).await;
    Ok(Json(RunEventsListResponse { events }))
}

/// `POST /runs/{run_id}/cancel` — cancel an active run.
pub async fn handle_run_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<(StatusCode, Json<Run>), AcpError> {
    let acp_config = { state.config.lock().acp.clone() };
    check_bearer_auth(&headers, &acp_config)?;

    let run = state
        .acp_run_store
        .get(&run_id)
        .await
        .ok_or_else(|| AcpError {
            code: AcpErrorCode::NotFound,
            message: format!("run '{run_id}' not found"),
            data: None,
        })?;

    if run.status.is_terminal() {
        return Err(AcpError {
            code: AcpErrorCode::InvalidInput,
            message: format!(
                "run '{run_id}' is already in terminal state {:?}",
                run.status
            ),
            data: None,
        });
    }

    state
        .acp_run_store
        .update_status(&run_id, RunStatus::Cancelled)
        .await;

    let updated = state
        .acp_run_store
        .get(&run_id)
        .await
        .ok_or_else(|| AcpError {
            code: AcpErrorCode::ServerError,
            message: "run disappeared from store".into(),
            data: None,
        })?;

    Ok((StatusCode::OK, Json(updated)))
}

/// `GET /session/{session_id}` — retrieve session history.
pub async fn handle_session_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<Json<AcpSession>, AcpError> {
    let acp_config = { state.config.lock().acp.clone() };
    check_bearer_auth(&headers, &acp_config)?;

    let run_ids = state.acp_run_store.runs_for_session(&session_id).await;
    let history: Vec<String> = run_ids
        .into_iter()
        .map(|id| format!("/runs/{id}"))
        .collect();

    Ok(Json(AcpSession {
        id: session_id,
        history,
    }))
}

// ── Run Eviction ───────────────────────────────────────────────

/// Spawn a background task that periodically evicts expired terminal runs.
pub fn spawn_run_eviction(run_store: Arc<RunStore>, ttl_secs: u64, interval_secs: u64) {
    tokio::spawn(async move {
        let ttl = std::time::Duration::from_secs(ttl_secs);
        let interval = std::time::Duration::from_secs(interval_secs);
        loop {
            tokio::time::sleep(interval).await;
            let evicted = run_store.evict_expired(ttl).await;
            if evicted > 0 {
                tracing::debug!(evicted, "ACP run eviction sweep");
            }
        }
    });
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn run_status_serializes_kebab_case() {
        let json = serde_json::to_string(&RunStatus::InProgress).unwrap();
        assert_eq!(json, "\"in-progress\"");
    }

    #[test]
    fn run_status_terminal() {
        assert!(RunStatus::Completed.is_terminal());
        assert!(RunStatus::Failed.is_terminal());
        assert!(RunStatus::Cancelled.is_terminal());
        assert!(!RunStatus::Created.is_terminal());
        assert!(!RunStatus::InProgress.is_terminal());
        assert!(!RunStatus::Awaiting.is_terminal());
        assert!(!RunStatus::Cancelling.is_terminal());
    }

    #[test]
    fn message_part_default_content_type() {
        let part: MessagePart = serde_json::from_str(r#"{"content": "hello"}"#).unwrap();
        assert_eq!(part.content_type, "text/plain");
    }

    #[test]
    fn event_serializes_with_type_tag() {
        let run = Run {
            agent_name: "test".into(),
            session_id: Uuid::new_v4().to_string(),
            run_id: Uuid::new_v4().to_string(),
            status: RunStatus::Created,
            await_request: None,
            output: vec![],
            error: None,
            created_at: None,
            finished_at: None,
        };
        let event = Event::RunCreated { run };
        let val: Value = serde_json::to_value(&event).unwrap();
        assert_eq!(val["type"], "run.created");
    }

    #[test]
    fn acp_error_code_serializes_snake_case() {
        let json = serde_json::to_string(&AcpErrorCode::InvalidInput).unwrap();
        assert_eq!(json, "\"invalid_input\"");
    }

    #[test]
    fn trajectory_metadata_tagged() {
        let meta = PartMetadata::Trajectory(TrajectoryMetadata {
            message: Some("step 1".into()),
            tool_name: None,
            tool_input: None,
            tool_output: None,
        });
        let val: Value = serde_json::to_value(&meta).unwrap();
        assert_eq!(val["kind"], "trajectory");
    }

    fn make_run(agent: &str, session: &str) -> Run {
        Run {
            agent_name: agent.into(),
            session_id: session.into(),
            run_id: Uuid::new_v4().to_string(),
            status: RunStatus::Created,
            await_request: None,
            output: vec![],
            error: None,
            created_at: Some(Utc::now()),
            finished_at: None,
        }
    }

    #[tokio::test]
    async fn run_store_insert_and_get() {
        let store = RunStore::new();
        let run = make_run("agent-a", "sess-1");
        let run_id = run.run_id.clone();
        store.insert(run.clone()).await;

        let fetched = store.get(&run_id).await.expect("run should exist");
        assert_eq!(fetched.run_id, run_id);
        assert_eq!(fetched.agent_name, "agent-a");
    }

    #[tokio::test]
    async fn run_store_update_status_terminal() {
        let store = RunStore::new();
        let run = make_run("agent-b", "sess-2");
        let run_id = run.run_id.clone();
        store.insert(run).await;

        store.update_status(&run_id, RunStatus::Completed).await;
        let fetched = store.get(&run_id).await.unwrap();
        assert_eq!(fetched.status, RunStatus::Completed);
        assert!(fetched.finished_at.is_some());
    }

    #[tokio::test]
    async fn run_store_session_tracking() {
        let store = RunStore::new();
        let session = "sess-track";
        let run = make_run("agent-c", session);
        let run_id = run.run_id.clone();
        store.insert(run).await;

        let ids = store.runs_for_session(session).await;
        assert_eq!(ids, vec![run_id]);
    }

    #[tokio::test]
    async fn run_store_active_count() {
        let store = RunStore::new();
        let r1 = make_run("a", "s1");
        let r2 = make_run("a", "s2");
        let r2_id = r2.run_id.clone();
        store.insert(r1).await;
        store.insert(r2).await;
        assert_eq!(store.active_run_count().await, 2);

        store.update_status(&r2_id, RunStatus::Failed).await;
        assert_eq!(store.active_run_count().await, 1);
    }

    #[tokio::test]
    async fn run_store_events() {
        let store = RunStore::new();
        let run = make_run("a", "s");
        let run_id = run.run_id.clone();
        store.insert(run.clone()).await;

        let event = Event::RunCreated { run };
        store.push_event(&run_id, event).await;

        let events = store.get_events(&run_id).await;
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn build_agent_registry_default() {
        let config = Config::default();
        let registry = build_agent_registry(&config);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].name, "hrafn");
    }

    #[test]
    fn manifest_from_def_includes_framework() {
        let def = AcpAgentDef {
            name: "test-agent".into(),
            description: "A test".into(),
            system_prompt: None,
            model: None,
            tools: vec![],
            input_content_types: vec!["text/plain".into()],
            output_content_types: vec!["text/plain".into()],
            capabilities: vec![],
        };
        let manifest = manifest_from_def(&def);
        assert_eq!(manifest.metadata.as_ref().unwrap().framework, "hrafn");
    }

    // ── Bearer auth tests ──────────────────────────────────────

    fn make_acp_config(token: Option<&str>) -> crate::config::AcpConfig {
        crate::config::AcpConfig {
            bearer_token: token.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn bearer_auth_no_token_configured() {
        let headers = HeaderMap::new();
        let config = make_acp_config(None);
        assert!(check_bearer_auth(&headers, &config).is_ok());
    }

    #[test]
    fn bearer_auth_missing_header() {
        let headers = HeaderMap::new();
        let config = make_acp_config(Some("secret-token"));
        let err = check_bearer_auth(&headers, &config).unwrap_err();
        assert_eq!(err.code, AcpErrorCode::Unauthorized);
        assert!(err.message.contains("Missing"));
    }

    #[test]
    fn bearer_auth_wrong_token() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer wrong-token".parse().unwrap());
        let config = make_acp_config(Some("secret-token"));
        let err = check_bearer_auth(&headers, &config).unwrap_err();
        assert_eq!(err.code, AcpErrorCode::Unauthorized);
        assert!(err.message.contains("Invalid"));
    }

    #[test]
    fn bearer_auth_correct_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer secret-token".parse().unwrap(),
        );
        let config = make_acp_config(Some("secret-token"));
        assert!(check_bearer_auth(&headers, &config).is_ok());
    }

    #[test]
    fn bearer_auth_no_bearer_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "secret-token".parse().unwrap());
        let config = make_acp_config(Some("secret-token"));
        // Without "Bearer " prefix, the token will be empty string → should fail
        let err = check_bearer_auth(&headers, &config).unwrap_err();
        assert_eq!(err.code, AcpErrorCode::Unauthorized);
    }

    // ── apply_agent_def_overrides tests ────────────────────────

    #[test]
    fn agent_def_overrides_model_and_prompt() {
        let mut config = Config::default();
        let def = AcpAgentDef {
            name: "test".into(),
            description: "test".into(),
            system_prompt: Some("custom prompt".into()),
            model: Some("gpt-4".into()),
            tools: vec![],
            input_content_types: vec![],
            output_content_types: vec![],
            capabilities: vec![],
        };
        apply_agent_def_overrides(&mut config, &def);
        assert_eq!(config.default_model.as_deref(), Some("gpt-4"));
        assert_eq!(
            config.identity.aieos_inline.as_deref(),
            Some("custom prompt")
        );
    }

    #[test]
    fn agent_def_no_overrides_preserves_config() {
        let mut config = Config::default();
        config.default_model = Some("original-model".into());
        let def = AcpAgentDef {
            name: "test".into(),
            description: "test".into(),
            system_prompt: None,
            model: None,
            tools: vec![],
            input_content_types: vec![],
            output_content_types: vec![],
            capabilities: vec![],
        };
        apply_agent_def_overrides(&mut config, &def);
        assert_eq!(config.default_model.as_deref(), Some("original-model"));
        assert!(config.identity.aieos_inline.is_none());
    }
}
