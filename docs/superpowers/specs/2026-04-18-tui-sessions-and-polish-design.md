# TUI Sessions & Visual Polish — Design Spec

**Date:** 2026-04-18
**Status:** Accepted
**Risk tier:** Medium (`src/tui/**`, new `src/session/**`, small additions in `src/cli.rs` / `src/main.rs` / `src/agent/**`, `Cargo.toml` touch only to enable existing `rusqlite`)
**Supersedes scope:** extends `docs/superpowers/specs/2026-04-12-tui-opencode-parity.md` — that spec's "Session management" non-goal is reversed here.

## Summary

Bundle two feature groups into PR #178 so the TUI stops looking "bare":

1. **Persistent sessions** backed by SQLite: every conversation is captured, can be listed, resumed, titled, and continued with full provider history rehydration (`Agent::seed_history`). CLI gains `--resume`, `-c`, `--list-sessions`, `--delete-session`. TUI gains `Ctrl+R` session picker and `/title`, `/sessions` slash commands. On clean exit, the TUI prints a resume-hint block to stderr.
2. **Visual polish:** startup banner (ASCII logo + version + model + cwd + session id), bottom status bar (model · branch · context% · permission mode · hint), `❯` input prompt placeholder. Pragmatic subset of the Claude-Code aesthetic — we skip PR#/MCP/hook counters which don't map cleanly onto Hrafn's concepts.
3. **Event wiring cleanup:** `src/tui/events.rs::handle_turn_event` / `handle_observer_event` are currently `todo!()`. This spec replaces them with real `TurnEvent` → `App` state mapping, retires the string-tag (`[tool:…]` / `[result:…]`) parsing currently done inline in `src/tui/mod.rs`, and adds one new variant `TurnEvent::TurnEnd` so consumers have an explicit end-of-turn signal instead of inferring it from channel close (see §Agent Changes).

## Non-Goals

- Session forking / branching (one linear history per session).
- Automatic LLM-generated titles (deferred; user sets via `-c` or `/title`, else first-message fallback).
- Session export / import (JSON dump) — out of scope.
- Editing or deleting individual messages inside a session.
- Cross-machine session sync.
- Mid-session model/provider switching.
- Multi-pane / tabbed sessions (one session per process).
- Hot swapping the loaded session in-place — picker exits the current TUI and re-enters via an in-process relaunch path (see §4).
- Full Claude-Code visual parity (PR# detection, MCP server counts, CLAUDE.md counter, hook counter, usage-window bars).

## Architecture

```
src/session/
  mod.rs        — Session, SessionMeta, SessionId, MessageCounts types + re-exports
  store.rs      — SessionStore (SQLite), CRUD + list + fuzzy
  schema.sql    — embedded via include_str!
  id.rs         — ID generation, parsing (YYYYMMDD_HHMMSS_<6hex>)

src/tui/
  mod.rs        — (edited) App holds Option<SessionHandle>; turn completion triggers persistence
  chat.rs       — (edited) accepts banner-as-system-messages; no structural change
  events.rs     — (rewritten) real TurnEvent/ObserverEvent → App mapping
  banner.rs     — (new) build_banner_messages(meta) → Vec<ChatMessage::System>
  statusbar.rs  — (new) render_status_bar(frame, area, &StatusInfo)
  picker.rs     — (new) session picker overlay + relaunch signal
  input.rs      — (edited) placeholder "❯ "
  command.rs    — (edited) /title, /sessions, /resume registered

src/cli.rs or src/main.rs
                — (edited) new flags --resume, -c, --list-sessions, --delete-session; dispatch
                  logic for continue-or-create fuzzy match

src/agent/agent.rs
                — (edited) TurnEvent gains a `TurnEnd` variant (see §Agent Changes).
                  Emitted once at the end of each streamed turn before the channel
                  is closed. Agent::seed_history(&[ChatMessage]) already exists and
                  is called once at boot when resuming; no change there.
```

### Data Flow — New Session

```
hrafn [-c "Title"]
  → CLI: create SessionStore, call store.create(cwd, title, provider, model) → SessionMeta
  → Agent builder built as today (empty history)
  → TUI boots with App { session: Some(handle), messages: banner_messages(), ... }
  → User submits turn:
      1. append(User)                          — DB write before mpsc send
      2. tx.send(text)                         — agent receives
      3. TurnEvent::Chunk deltas accumulate    — UI only
      4. TurnEvent::ToolCall                   — DB write + UI push
      5. TurnEvent::ToolResult                 — DB write + UI push
      6. stream ends                           — DB write Assistant(accumulated) + UI push
      7. if title is NULL and not explicit:
           set_title(first_user_msg_truncated, explicit=false)
  → User /quit:
      - flush duration
      - exit TUI, print resume hint block to stderr
```

### Data Flow — Resume

```
hrafn --resume <id>   (or --resume alone → most-recent)
hrafn -c "needle"     (fuzzy → most-recent match if any)

  → CLI: store.load(id) → Session { meta, messages }
  → Build provider history from session.messages (filter/convert; see §Turn Persistence)
  → Agent builder: agent.seed_history(&history)
  → TUI boots with App { session: Some(handle), messages: banner + replayed messages, ... }
  → Subsequent turns append as in "New Session" flow.
```

### Data Flow — In-TUI Picker

```
Ctrl+R or /sessions
  → picker opens, calls store.list(limit=100) ORDER BY updated_at DESC
  → fuzzy filter on title+id as user types
  → Enter on a selection:
      app.resume_id = Some(selected.id)
      app.should_quit = true
  → outer main() sees resume_id, re-enters boot path with that session
    (in-process relaunch, no exec)
```

## Storage — SQLite

**Location:** `$XDG_DATA_HOME/hrafn/sessions.db` — fallback `~/.local/share/hrafn/sessions.db`. Created with `0700` directory permissions.

**Dependency note:** `rusqlite = "0.37"` is already present in `Cargo.toml` (used by the memory backend and WhatsApp storage). No new dep.

### Schema v1

```sql
CREATE TABLE IF NOT EXISTS sessions (
    id               TEXT PRIMARY KEY,
    title            TEXT,
    title_explicit   INTEGER NOT NULL DEFAULT 0,
    cwd              TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    duration_ms      INTEGER NOT NULL DEFAULT 0,
    provider         TEXT,
    model            TEXT,
    msg_total        INTEGER NOT NULL DEFAULT 0,
    msg_user         INTEGER NOT NULL DEFAULT 0,
    msg_assistant    INTEGER NOT NULL DEFAULT 0,
    msg_tool_call    INTEGER NOT NULL DEFAULT 0,
    msg_tool_result  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);

CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    payload     TEXT NOT NULL,
    ts          INTEGER NOT NULL,
    UNIQUE(session_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);

CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);
INSERT OR IGNORE INTO schema_version VALUES (1);
```

**Pragmas on open:** `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=2000`.

### Rust Types

```rust
pub struct SessionId(String);  // validated YYYYMMDD_HHMMSS_<6hex>

pub struct MessageCounts {
    pub total: u32,
    pub user: u32,
    pub assistant: u32,
    pub tool_call: u32,
    pub tool_result: u32,
}

pub struct SessionMeta {
    pub id: SessionId,
    pub title: Option<String>,
    pub title_explicit: bool,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub duration: Duration,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub counts: MessageCounts,
}

pub struct StoredMessage {
    pub seq: u32,
    pub ts: DateTime<Utc>,
    pub body: crate::tui::ChatMessage,
}

pub struct Session {
    pub meta: SessionMeta,
    pub messages: Vec<StoredMessage>,
}
```

**Serialization:** `crate::tui::ChatMessage` gains `#[derive(Serialize, Deserialize)]`. The `Instant` fields inside `ToolStatus::Running` are not serializable — at persist time, any `Running(started)` is converted to `Done(started.elapsed())` (Running is only a transient UI state). Note: `ChatMessage::ToolCall.args` is currently `String` in the TUI type, but the upstream `TurnEvent::ToolCall.args` is `serde_json::Value`; the event-handler stringifies it via `serde_json::to_string` before constructing the UI message. No upstream type change needed.

### `SessionStore` API

```rust
impl SessionStore {
    pub fn open(path: &Path) -> Result<Self>;
    pub fn create(
        &self,
        cwd: &Path,
        title_seed: Option<&str>,  // becomes title with title_explicit=true
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<SessionMeta>;
    pub fn load(&self, id: &SessionId) -> Result<Session>;
    pub fn append(&self, id: &SessionId, msg: &ChatMessage) -> Result<()>;
    pub fn set_title(&self, id: &SessionId, title: &str, explicit: bool) -> Result<()>;
    pub fn add_duration(&self, id: &SessionId, d: Duration) -> Result<()>;
    pub fn list(&self, limit: usize) -> Result<Vec<SessionMeta>>;
    pub fn find_by_title_fuzzy(&self, needle: &str) -> Result<Option<SessionMeta>>;
    pub fn most_recent(&self) -> Result<Option<SessionMeta>>;
    pub fn delete(&self, id: &SessionId) -> Result<()>;
}
```

`append` runs inside a single transaction: INSERT INTO messages, UPDATE sessions counter for the matching kind, UPDATE sessions.updated_at. Counters cannot drift from the messages table by construction.

**Concurrency:** single-writer per session enforced by a PID lockfile `sessions.db-hrafn-<id>.lock` alongside the DB. On startup, if a lockfile's PID is stale (not running), it's removed. WAL allows other processes to run `--list-sessions` while a TUI session is active.

## CLI Surface

```
hrafn                         # new session, auto-ID, no title
hrafn -c "Title here"         # continue-or-create: fuzzy-match title; resume most recent
                              #   if any match, else create new with that explicit title
hrafn --resume <id>           # resume specific session
hrafn --resume                # resume most-recent session
hrafn --list-sessions         # table to stdout, no TUI
hrafn --list-sessions --json  # machine-readable
hrafn --delete-session <id>   # prompts; bypass with --yes
```

Flags `--resume` and `-c` are mutually exclusive. Extend the existing `clap`-based command enum; no new subcommand tree.

**`--list-sessions` output:**

```
  ID                        UPDATED      MSGS   USER  TOOL   TITLE
  20260417_205355_53b1e8    1h ago       20     5     12     Running and Testing Inter Agent ACP
  20260416_143201_a1b2c3    yesterday    48     12    30     —
  20260410_090812_deadbe    1w ago       6      2     3      Quick doc fix
```

**Exit banner (stderr, on clean exit):**

```
Resume this session with:
  hrafn --resume 20260417_205355_53b1e8
  hrafn -c "Running and Testing Inter Agent ACP"

Session:    20260417_205355_53b1e8
Title:      Running and Testing Inter Agent ACP
Duration:   1h 55m 16s
Messages:   20 (5 user, 12 tool calls)
```

When the title is not set, the `-c` hint line is omitted and `Title:` shows `Untitled`.

## TUI Changes

### Startup Banner

Emitted as a sequence of `ChatMessage::System` entries — part of the scrollback, not pinned. Scrolls away as the conversation grows.

```
  ┃┏┓  ┏━┓┏━╸┏┓╻
  ┃┣┫  ┣┳┛┣━┛┃┗┫        Hrafn v{version}
  ╹╹╹  ╹┗╸╹  ╹ ╹        {provider}/{model}  ·  {cwd}

  Welcome. Type a message, or /help for commands.
  Session: {id}   (· {title})?
```

On resume, the last line becomes `Resumed session {id} · {N} messages` and the prior conversation is replayed below (new System entry follows, then all persisted messages, then a blank line).

### Bottom Status Bar (new)

Single-row widget at the very bottom of the frame. Layout gets a new trailing `Constraint::Length(1)`.

Content (left-to-right, ` │ ` separated, any unavailable field dropped):

- **Model** — short from `AgentInfo.model` (e.g. `opus-4-7`).
- **Git branch + dirty marker** — plain read of `.git/HEAD` (parse `ref: refs/heads/<name>` → `<name>`, else show short SHA), no new crate. Dirty flag via `std::process::Command::new("git").args(["status", "--porcelain"])` with a 500ms timeout, cached 5s. If `.git/` is absent, the `git` binary is absent, or the shellout fails, skip the whole field silently.
- **Context %** — `(input_tokens + output_tokens) / context_window`. Requires extending `AgentInfo` with `context_window: Option<u32>` populated from provider metadata. Hidden if unknown.
- **Permission mode** — from `SecurityPolicy`; rendered in amber when `bypass`.
- **Hint** — static `/help` for v1.

### Input Placeholder

Change placeholder from `> ` to `❯ ` in `src/tui/input.rs`. Existing user message rendering (already `> `) stays.

### New Slash Commands

| Command | Action |
|---|---|
| `/title <text>` | `store.set_title(id, text, explicit=true)`; feedback as `System{"[title set]"}`. |
| `/sessions` | Open picker overlay (same as `Ctrl+R`). |

### Session Picker (`src/tui/picker.rs`)

- Centered modal, ~80w × 20h, rendered via `Clear` + bordered `Paragraph` (same pattern as `command.rs`).
- Header row: `Sessions` + `_` cursor with fuzzy query buffer below.
- Rows: `{rel_time}  {id_short(10 chars)}  {total}m  {title or "—"}`, most-recent first.
- Navigation: `↑/↓` move selection; `Enter` selects; `Esc` closes without effect.
- Fuzzy filter: case-insensitive substring match on concatenated `{id} {title}`.
- On select: `app.resume_id = Some(id); app.should_quit = true`. Outer `main` re-enters the boot path with that session.

### `events.rs` Rewrite

Currently `todo!()`. Replace with:

```rust
pub(crate) fn handle_turn_event(app: &mut App, event: TurnEvent) {
    match event {
        TurnEvent::Chunk { delta } => app.pending_chunk.push_str(&delta),
        TurnEvent::Thinking { .. } => { /* non-goal: ignore */ }
        TurnEvent::ToolCall { name, args } => {
            let args_str = serde_json::to_string(&args).unwrap_or_default();
            app.active_tools.push(ActiveTool {
                name: name.clone(),
                args: args_str.clone(),
                started: Instant::now(),
            });
            let msg = ChatMessage::ToolCall {
                name,
                args: args_str,
                status: ToolStatus::Running(Instant::now()),
            };
            app.messages.push(msg.clone());
            persist(app, &msg);
        }
        TurnEvent::ToolResult { name, output } => {
            update_tool_status_to_done(&mut app.messages, &name);
            let msg = ChatMessage::ToolResult { name, output };
            app.messages.push(msg.clone());
            persist(app, &msg);
        }
        TurnEvent::TurnEnd => {
            let text = std::mem::take(&mut app.pending_chunk);
            if !text.is_empty() {
                let msg = ChatMessage::Assistant { text };
                app.messages.push(msg.clone());
                persist(app, &msg);
                maybe_set_first_message_title(app);
            }
            app.spinner = None;
        }
    }
}
```

`persist(app, &msg)` calls `store.append(id, msg)` when `app.session` is `Some`. Errors surface as `System{"[persistence error: ...]"}` (see §Errors). The old `[tool:NAME]` / `[result:NAME]` string parsing in `mod.rs` is deleted — the bridge now forwards `TurnEvent`s on a typed channel.

### Keybindings (additions)

| Key | Action |
|-----|--------|
| `Ctrl+R` | Open session picker |
| `/sessions` | Open session picker |
| `/title <x>` | Set title |

All previously-defined keybindings from the `2026-04-12` spec remain.

## Agent Changes

Minimal, one additive variant:

```rust
// src/agent/agent.rs
pub enum TurnEvent {
    Chunk { delta: String },
    Thinking { delta: String },
    ToolCall { name: String, args: serde_json::Value },
    ToolResult { name: String, output: String },
    TurnEnd,   // NEW: sent once per turn before event_tx is dropped
}
```

`Agent::turn_streamed` sends `TurnEnd` at the single exit point of the loop, immediately before returning. Existing consumers that `match` exhaustively pick it up via a `_ => {}` arm (grep shows no exhaustive external matches; the `loop_` runner uses pattern matching with wildcards). The one place that was relying on "channel closes ⇒ turn done" (the TUI bridge in `src/agent/loop_.rs` around line 5180) gets updated to react to `TurnEnd` instead, which is cleaner and lets the TUI know whether the turn ended naturally or via `tx` drop (error path).

No other agent-layer changes. `seed_history` stays as-is.

## Turn Persistence Semantics

| TUI event | DB write |
|---|---|
| User submits non-empty text | `append(User{text})` before `tx.send()` |
| `TurnEvent::ToolCall` | `append(ToolCall{status: Done(0)})` |
| `TurnEvent::ToolResult` | `append(ToolResult)` |
| End-of-stream, non-empty `pending_chunk` | `append(Assistant{text})` |
| `TurnEvent::Thinking` | **not persisted** |
| `/clear` | insert synthetic `System{"[cleared]"}`; do NOT delete rows |
| `/title <x>` | `set_title(explicit=true)` |

**Provider history rehydration on resume:**

```rust
fn to_provider_history(stored: &[StoredMessage]) -> Vec<providers::ChatMessage> {
    stored.iter().filter_map(|m| match &m.body {
        ChatMessage::User { text } => Some(providers::ChatMessage::user(text)),
        ChatMessage::Assistant { text } => Some(providers::ChatMessage::assistant(text)),
        ChatMessage::ToolCall { name, args, .. } => Some(providers::ChatMessage::tool_call(name, args)),
        ChatMessage::ToolResult { name, output } => Some(providers::ChatMessage::tool_result(name, output)),
        ChatMessage::System { .. } => None, // banner/cleared markers are UI-only
    }).collect()
}
```

`Agent::seed_history(&history)` is called once in the builder path before the first new turn.

**Title fallback:** after first turn completes, if `title IS NULL` and `!title_explicit`, set to `first_user_message.lines().next().unwrap_or("").chars().take(60).collect() + "…"` (truncated).

**Duration:** timer = `Instant::now()` at `App::new()`. Flush `store.add_duration(d)` every 10 ticks of idle wakeup (≈1s); final flush on `/quit`. Backgrounding via `Ctrl+Z` is not compensated for — OK for v1.

## Error Handling

| Failure | Behavior |
|---|---|
| DB open fails at startup | Print error to stderr, exit 1, **before** alt-screen enter. No silent in-memory fallback. |
| `append` fails mid-session | Push `System{"[persistence error: <msg>]"}` into UI; retry once on next write; second failure logs via `tracing` and suppresses further retries for the rest of this turn. Session continues operating in memory. |
| `--resume <id>` unknown ID | `error: session not found: <id>` to stderr, exit 2. |
| `-c "needle"` 0 matches | Silent fall-through to new-session-with-explicit-title (documented behavior). |
| `--list-sessions` empty DB | `No sessions yet.` to stdout, exit 0. |
| Schema version newer than binary | `error: unknown schema version N; upgrade hrafn`, exit 3. |
| Corrupt JSON in `messages.payload` | Replace that row in the loaded session with `System{"[corrupt message @ seq N]"}`; keep loading. |
| Lockfile held by live PID | `error: session <id> is locked (another hrafn process is using it)`, exit 4. |
| Stale lockfile (PID dead) | Remove and continue. |

## Testing

- **Unit — `session::id`:** generation uniqueness under parallel calls (same wall-clock second); parse round-trip; invalid formats rejected.
- **Unit — `SessionStore`:** CRUD; counters match message kinds after append; `list` ordering by `updated_at DESC`; `find_by_title_fuzzy` returns most-recent match; `delete` cascades.
- **Unit — `ChatMessage` JSON round-trip:** every variant, including `ToolStatus::Running` → persisted as `Done(elapsed)`.
- **Unit — `to_provider_history`:** `System` filtered out; order preserved.
- **Unit — title fallback:** first-message truncation at 60 chars, multi-line collapse, empty-first-message handled.
- **Integration — round-trip:** create session, append N mixed-kind messages, close, reopen, verify full ordered replay + counters.
- **Integration — `-c` fuzzy:** multiple matching sessions → picks newest; 0 matches → creates with explicit title.
- **Integration — resume seeds provider:** stub provider records the `ChatMessage` list it receives and asserts expected shape after `--resume`.
- **Smoke — shutdown clean:** no temp files; WAL checkpointed; no lockfile remaining.
- **Smoke — crash resilience:** kill TUI mid-turn; next `--resume` succeeds and sees messages up to the last successful `append`.

## Decision Log

| # | Decision | Alternatives | Rationale |
|---|----------|-------------|-----------|
| 1 | Reverse the "session mgmt is a non-goal" of the prior spec; bundle into PR #178 | Land #178 as-is, do sessions in a follow-up | User call; current TUI "looks not good" and session mgmt is the highest-value gap |
| 2 | SQLite (rusqlite) for storage | JSONL per session, single-JSON-per-session | `rusqlite` already in deps; queryable list view; atomic counters; WAL enables concurrent list while session is live |
| 3 | ID format `YYYYMMDD_HHMMSS_<6hex>` | UUIDv7, ULID | Human-readable, sorts by creation time in `ls`, matches the reference paste |
| 4 | UI + provider context on resume (full rehydration) | UI-only replay, selective last-N | "Resume" that breaks follow-up context is a broken abstraction. `Agent::seed_history` already exists — cheap to wire |
| 5 | User-set title with first-message fallback | Auto-LLM-generated title, hybrid | No LLM cost; explicit `/title` and `-c` cover power users; fallback saves the lazy case |
| 6 | Counter breakdown `total (N user, M tool calls)` | Granular breakdown, minimal | Matches reference; tool-call count is the useful signal for "was this a real session" |
| 7 | In-TUI picker relaunches in-process (not `exec`) | Fork+exec; true in-place swap | Keeps agent/channel shutdown clean; true swap is a deep refactor for v1 |
| 8 | Pragmatic visual polish subset, skip PR#/MCP/hook counters | Full Claude-Code parity, minimal | PR#/MCP/hook are Claude-Code concepts that don't map to Hrafn; effort-to-value is poor |
| 9 | Persistence writes inside the TUI path (synchronous in event loop) | Offload to background task via channel | SQLite WAL append is sub-millisecond; async adds complexity and error-flow confusion. Revisit if a write ever blocks the render |
| 10 | Clear `/clear` is UI-only; doesn't delete DB rows | `/clear` wipes session | Users regret destructive defaults; `--delete-session <id>` is the explicit destructive path |
| 11 | Banner rendered as `System` scrollback entries | Fixed pinned region at top | Scrolls naturally; zero extra layout complexity; matches how Claude Code does it |
| 12 | Lockfile by PID for single-writer | OS advisory locks, sqlite `BEGIN IMMEDIATE` | Gives a clear error message rather than a mysterious lock contention; stale-PID cleanup is simple |

## Migration

No existing sessions exist (this is v1). First run creates the DB. No schema migration infrastructure needed yet — `schema_version = 1` is the only recognised version; any other value exits with the upgrade hint.

## Open Items Deferred

- LLM-generated titles (v2).
- Session export/import as JSON (v2).
- Session branching / forking (likely never — would require rethinking the provider-history model).
- Real in-place session swap without relaunch (v2 or v3 — requires agent shutdown/boot choreography).
- Context-%% accuracy across compaction (depends on how `input_tokens` is reported after auto-compact).
