//! Autonomous skill creation after agent turns.
//!
//! After every agent turn, a lightweight classifier checks whether the workflow
//! should be persisted as a reusable skill. The criteria are:
//! - At least 3 tool calls succeeded
//! - No existing skill covers this pattern (FTS5 search)
//! - The task is repeatable (not a one-off lookup)
//!
//! Security controls include path traversal prevention, disk quota with LRU eviction,
//! and deduplication via FTS5 similarity scoring.

use anyhow::{bail, Result};
use regex::Regex;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tracing::warn;

/// Represents a single agent turn for skill creation analysis.
#[derive(Debug, Clone)]
pub struct AgentTurn {
    /// The user's original message/prompt.
    pub user_message: String,
    /// Tool calls that were executed during this turn.
    pub tool_calls: Vec<ToolCallRecord>,
    /// The assistant's final response text.
    pub assistant_response: String,
}

/// Record of a single tool call execution.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    /// Tool name.
    pub name: String,
    /// Whether the execution succeeded.
    pub success: bool,
    /// Tool arguments as JSON.
    pub args: serde_json::Value,
    /// Tool output.
    pub output: String,
}

/// Configuration for autonomous skill creation.
#[derive(Debug, Clone)]
pub struct SkillCreationConfig {
    /// Directory where skill files are stored.
    pub skills_dir: PathBuf,
    /// Maximum number of skills before LRU eviction.
    pub max_skills: usize,
    /// Minimum number of successful tool calls to trigger skill creation.
    pub min_tool_calls: usize,
    /// FTS5 similarity threshold for deduplication (0.0–1.0).
    pub dedup_threshold: f64,
}

impl Default for SkillCreationConfig {
    fn default() -> Self {
        Self {
            skills_dir: PathBuf::from("skills"),
            max_skills: 500,
            min_tool_calls: 3,
            dedup_threshold: 0.85,
        }
    }
}

/// Manages the skill index in SQLite/FTS5.
pub struct SkillIndex {
    conn: Arc<parking_lot::Mutex<Connection>>,
}

impl SkillIndex {
    /// Open or create the skill index database.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;",
        )?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(parking_lot::Mutex::new(conn)),
        })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS skill_index (
                slug         TEXT PRIMARY KEY,
                title        TEXT NOT NULL,
                tags         TEXT NOT NULL DEFAULT '',
                content      TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                accessed_at  TEXT NOT NULL,
                updated_at   TEXT,
                last_improved_at TEXT
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS skill_fts USING fts5(
                slug, title, tags, content,
                content=skill_index, content_rowid=rowid
            );
            CREATE TRIGGER IF NOT EXISTS skill_ai AFTER INSERT ON skill_index BEGIN
                INSERT INTO skill_fts(rowid, slug, title, tags, content)
                VALUES (new.rowid, new.slug, new.title, new.tags, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS skill_ad AFTER DELETE ON skill_index BEGIN
                INSERT INTO skill_fts(skill_fts, rowid, slug, title, tags, content)
                VALUES ('delete', old.rowid, old.slug, old.title, old.tags, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS skill_au AFTER UPDATE ON skill_index BEGIN
                INSERT INTO skill_fts(skill_fts, rowid, slug, title, tags, content)
                VALUES ('delete', old.rowid, old.slug, old.title, old.tags, old.content);
                INSERT INTO skill_fts(rowid, slug, title, tags, content)
                VALUES (new.rowid, new.slug, new.title, new.tags, new.content);
            END;",
        )?;
        Ok(())
    }

    /// Search FTS5 for skills matching a query. Returns (slug, title, rank).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<(String, String, f64)>> {
        let sanitized = sanitize_fts5_query(query);
        if sanitized.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT slug, title, rank
             FROM skill_fts
             WHERE skill_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![sanitized, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Check if a similar skill already exists (deduplication).
    /// Returns true if a skill with similarity > threshold exists.
    pub fn has_similar(&self, query: &str, threshold: f64) -> Result<bool> {
        let results = self.search(query, 1)?;
        // FTS5 rank is negative (closer to 0 = better match).
        // We normalize: similarity = 1.0 / (1.0 + abs(rank))
        if let Some((_slug, _title, rank)) = results.first() {
            let similarity = 1.0 / (1.0 + rank.abs());
            return Ok(similarity > threshold);
        }
        Ok(false)
    }

    /// Insert a new skill into the index.
    pub fn insert(&self, slug: &str, title: &str, tags: &str, content: &str) -> Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO skill_index (slug, title, tags, content, created_at, accessed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![slug, title, tags, content, now],
        )?;
        Ok(())
    }

    /// Update the accessed_at timestamp for LRU tracking.
    pub fn touch(&self, slug: &str) -> Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE skill_index SET accessed_at = ?1 WHERE slug = ?2",
            params![now, slug],
        )?;
        Ok(())
    }

    /// Count total skills in the index.
    pub fn count(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM skill_index", [], |row| row.get(0))?;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Ok(count as usize)
    }

    /// Evict the least-recently-used skill. Returns the evicted slug if any.
    pub fn evict_lru(&self) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let slug: Option<String> = conn
            .query_row(
                "SELECT slug FROM skill_index ORDER BY accessed_at ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();
        if let Some(ref slug) = slug {
            conn.execute("DELETE FROM skill_index WHERE slug = ?1", params![slug])?;
            warn!(slug = slug.as_str(), "evicted least-recently-used skill");
        }
        Ok(slug)
    }

    /// Update a skill's content and mark as improved.
    pub fn update_content(&self, slug: &str, content: &str, updated_at: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE skill_index SET content = ?1, updated_at = ?2, last_improved_at = ?2 WHERE slug = ?3",
            params![content, updated_at, slug],
        )?;
        Ok(())
    }

    /// Get the last_improved_at timestamp for a skill.
    pub fn last_improved_at(&self, slug: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let ts: Option<String> = conn
            .query_row(
                "SELECT last_improved_at FROM skill_index WHERE slug = ?1",
                params![slug],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        Ok(ts)
    }

    /// Get a skill's content by slug.
    pub fn get_content(&self, slug: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let content: Option<String> = conn
            .query_row(
                "SELECT content FROM skill_index WHERE slug = ?1",
                params![slug],
                |row| row.get(0),
            )
            .ok();
        Ok(content)
    }

    /// List all skills (slug, title, tags).
    pub fn list_all(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare("SELECT slug, title, tags FROM skill_index ORDER BY accessed_at DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
}

/// Sanitize a string for use in FTS5 MATCH queries.
///
/// FTS5 special characters like `+`, `-`, `*`, `"`, `(`, `)` are removed.
/// The query is split into words and joined with spaces (implicit OR).
fn sanitize_fts5_query(raw: &str) -> String {
    let words: Vec<&str> = raw
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty() && w.len() >= 2)
        .take(20) // Limit query complexity
        .collect();
    words.join(" ")
}

/// Validate a slug derived from LLM output.
///
/// # Security (S1.1)
/// - Allowlist regex: `^[a-z0-9_-]{1,64}$`
/// - Rejects `..`, `/`, `\`, null bytes, and unicode overrides.
pub fn validate_slug(raw: &str) -> Result<String> {
    // Check for forbidden characters first
    if raw.contains('\0') {
        bail!("slug contains null byte");
    }
    if raw.contains("..") {
        bail!("slug contains path traversal sequence '..'");
    }
    if raw.contains('/') || raw.contains('\\') {
        bail!("slug contains path separator");
    }

    // Check for unicode override characters (RTL override, etc.)
    for ch in raw.chars() {
        if matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200F}' | '\u{200E}')
        {
            bail!("slug contains unicode directional override");
        }
    }

    let slug = raw.to_ascii_lowercase().trim().to_string();

    static SLUG_RE: OnceLock<Regex> = OnceLock::new();
    let re =
        SLUG_RE.get_or_init(|| Regex::new(r"^[a-z0-9_-]{1,64}$").expect("slug regex is valid"));
    if !re.is_match(&slug) {
        bail!(
            "slug '{}' does not match allowlist pattern ^[a-z0-9_-]{{1,64}}$",
            slug
        );
    }

    Ok(slug)
}

/// Construct a skill file path and verify it stays inside skills_dir.
///
/// # Security (S1.1)
/// After path construction, asserts the canonical path starts with skills_dir.
pub fn safe_skill_path(skills_dir: &Path, slug: &str) -> Result<PathBuf> {
    let validated_slug = validate_slug(slug)?;
    let path = skills_dir.join(format!("{validated_slug}.md"));

    // Ensure parent directory exists for canonicalization
    std::fs::create_dir_all(skills_dir)?;

    let canonical_skills_dir = skills_dir.canonicalize()?;
    // For new files, we canonicalize the parent and append the filename
    let canonical_path = canonical_skills_dir.join(format!("{validated_slug}.md"));

    if !canonical_path.starts_with(&canonical_skills_dir) {
        bail!(
            "resolved skill path '{}' escapes skills directory '{}'",
            canonical_path.display(),
            canonical_skills_dir.display()
        );
    }

    Ok(path)
}

/// Determine whether the agent turn should trigger skill creation.
///
/// Criteria:
/// - At least `min_tool_calls` tool calls succeeded
/// - No existing skill covers this pattern (FTS5 search)
/// - The task is repeatable (heuristic: not a single lookup/query)
pub fn should_create_skill(
    turn: &AgentTurn,
    index: &SkillIndex,
    config: &SkillCreationConfig,
) -> bool {
    let successful_calls = turn.tool_calls.iter().filter(|c| c.success).count();
    if successful_calls < config.min_tool_calls {
        return false;
    }

    // Check for existing similar skill
    let query = format!("{} {}", turn.user_message, turn.assistant_response);
    if let Ok(true) = index.has_similar(&query, config.dedup_threshold) {
        return false;
    }

    // Heuristic: one-off lookups typically have 0-1 unique tool names
    let unique_tools: std::collections::HashSet<&str> = turn
        .tool_calls
        .iter()
        .filter(|c| c.success)
        .map(|c| c.name.as_str())
        .collect();
    if unique_tools.len() < 2 {
        return false;
    }

    true
}

/// Write a skill markdown file to disk with all security checks.
///
/// # Security
/// - S1.1: Path traversal prevention via `safe_skill_path`
/// - S1.2: Disk quota with LRU eviction
/// - S1.3: Deduplication via FTS5
pub fn write_skill(
    skills_dir: &Path,
    slug: &str,
    content: &str,
    index: &SkillIndex,
    config: &SkillCreationConfig,
) -> Result<Option<PathBuf>> {
    let validated_slug = validate_slug(slug)?;

    // S1.3: Deduplication check
    if index.has_similar(content, config.dedup_threshold)? {
        return Ok(None);
    }

    // S1.2: Quota enforcement with LRU eviction
    let count = index.count()?;
    if count >= config.max_skills {
        if let Some(evicted_slug) = index.evict_lru()? {
            // Remove the evicted skill file
            let evicted_path = skills_dir.join(format!("{evicted_slug}.md"));
            if evicted_path.exists() {
                std::fs::remove_file(&evicted_path)?;
            }
        }
    }

    // S1.1: Safe path construction
    let path = safe_skill_path(skills_dir, &validated_slug)?;

    // Write the file
    std::fs::create_dir_all(skills_dir)?;
    std::fs::write(&path, content)?;

    // Parse front-matter for title and tags
    let (title, tags) = parse_skill_front_matter(content);

    // Insert into index
    index.insert(&validated_slug, &title, &tags, content)?;

    Ok(Some(path))
}

/// Parse TOML front-matter from a skill markdown document.
/// Returns (title, comma-separated tags).
pub fn parse_skill_front_matter(content: &str) -> (String, String) {
    let trimmed = content.trim();
    if !trimmed.starts_with("+++") {
        return (String::new(), String::new());
    }

    let rest = &trimmed[3..];
    let end = rest.find("+++");
    let Some(end_pos) = end else {
        return (String::new(), String::new());
    };

    let front_matter = &rest[..end_pos];
    let mut title = String::new();
    let mut tags = String::new();

    match toml::from_str::<toml::Table>(front_matter) {
        Ok(table) => {
            if let Some(t) = table.get("title").and_then(|v| v.as_str()) {
                title = t.to_string();
            }
            if let Some(arr) = table.get("tags").and_then(|v| v.as_array()) {
                let tag_strs: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                tags = tag_strs.join(",");
            }
        }
        Err(_) => {
            // Fall back to line-by-line parsing for simple key = "value" entries
            for line in front_matter.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("title = \"") {
                    if let Some(val) = rest.strip_suffix('"') {
                        title = val.to_string();
                    }
                } else if let Some(rest) = line.strip_prefix("tags = [") {
                    if let Some(arr_str) = rest.strip_suffix(']') {
                        tags = arr_str
                            .split(',')
                            .map(|s| s.trim().trim_matches('"'))
                            .collect::<Vec<_>>()
                            .join(",");
                    }
                }
            }
        }
    }

    (title, tags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_slug_strips_path_traversal() {
        // "../etc/passwd" → rejected
        assert!(validate_slug("../etc/passwd").is_err());
        assert!(validate_slug("..").is_err());
        assert!(validate_slug("foo/../bar").is_err());
        assert!(validate_slug("foo/bar").is_err());
        assert!(validate_slug("foo\\bar").is_err());
        assert!(validate_slug("foo\0bar").is_err());

        // Unicode directional overrides
        assert!(validate_slug("foo\u{202E}bar").is_err());
        assert!(validate_slug("foo\u{200F}bar").is_err());

        // Valid slugs
        assert_eq!(validate_slug("hello-world").unwrap(), "hello-world");
        assert_eq!(validate_slug("my_skill_123").unwrap(), "my_skill_123");
        assert_eq!(validate_slug("a").unwrap(), "a");

        // Too long (>64 chars)
        let long = "a".repeat(65);
        assert!(validate_slug(&long).is_err());

        // Empty
        assert!(validate_slug("").is_err());

        // Uppercase gets lowered
        assert_eq!(validate_slug("MySkill").unwrap(), "myskill");
    }

    #[test]
    fn test_skill_path_always_inside_skills_dir() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        // Normal slug
        let path = safe_skill_path(&skills_dir, "my-skill").unwrap();
        let canonical = path
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join(path.file_name().unwrap());
        assert!(canonical.starts_with(skills_dir.canonicalize().unwrap()));

        // Path traversal attempt should fail at validation
        assert!(safe_skill_path(&skills_dir, "../etc/passwd").is_err());
        assert!(safe_skill_path(&skills_dir, "../../root").is_err());
    }

    #[tokio::test]
    async fn test_quota_evicts_lru_on_overflow() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("skills.db");
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let index = SkillIndex::open(&db_path).unwrap();
        let config = SkillCreationConfig {
            skills_dir: skills_dir.clone(),
            max_skills: 3,
            min_tool_calls: 3,
            dedup_threshold: 0.85,
        };

        // Insert 3 skills
        for i in 0..3 {
            let slug = format!("skill-{i}");
            let content = format!("+++\ntitle = \"Skill {i}\"\ntags = [\"test\"]\n+++\n# Skill {i}\nContent {i} unique words here for differentiation.");
            // Create the file manually
            std::fs::write(skills_dir.join(format!("{slug}.md")), &content).unwrap();
            index
                .insert(&slug, &format!("Skill {i}"), "test", &content)
                .unwrap();
            // Small delay to ensure different accessed_at timestamps
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(index.count().unwrap(), 3);

        // Touch skill-1 and skill-2 to make skill-0 the LRU
        index.touch("skill-1").unwrap();
        index.touch("skill-2").unwrap();

        // Writing a 4th skill should evict skill-0 (the LRU)
        let content = "+++\ntitle = \"Skill 3\"\ntags = [\"new\"]\n+++\n# Skill 3\nBrand new unique content that is totally different.";
        let result = write_skill(&skills_dir, "skill-3", content, &index, &config).unwrap();
        assert!(result.is_some());

        // skill-0 should have been evicted
        assert_eq!(index.count().unwrap(), 3);
        assert!(index.get_content("skill-0").unwrap().is_none());
        assert!(index.get_content("skill-3").unwrap().is_some());
    }

    #[tokio::test]
    async fn test_dedup_rejects_similar_skill() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("skills.db");
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let index = SkillIndex::open(&db_path).unwrap();
        let config = SkillCreationConfig {
            skills_dir: skills_dir.clone(),
            max_skills: 500,
            min_tool_calls: 3,
            dedup_threshold: 0.85,
        };

        // Insert an initial skill
        let content = "+++\ntitle = \"Deploy app\"\ntags = [\"deploy\"]\n+++\n# Deploy Application\nBuild the project and deploy to production using docker compose up.";
        write_skill(&skills_dir, "deploy-app", content, &index, &config)
            .unwrap()
            .unwrap();

        // Try to write a near-identical skill — should be deduplicated
        let similar_content = "+++\ntitle = \"Deploy app\"\ntags = [\"deploy\"]\n+++\n# Deploy Application\nBuild the project and deploy to production using docker compose up.";
        let result = write_skill(
            &skills_dir,
            "deploy-app-v2",
            similar_content,
            &index,
            &config,
        )
        .unwrap();
        assert!(
            result.is_none(),
            "near-identical skill should be deduplicated"
        );
    }

    #[test]
    fn test_parse_skill_front_matter() {
        let content = r#"+++
title = "My Skill"
tags = ["web", "deploy"]
created_at = "2026-03-16T00:00:00Z"
+++
# My Skill
Do things.
"#;
        let (title, tags) = parse_skill_front_matter(content);
        assert_eq!(title, "My Skill");
        assert_eq!(tags, "web,deploy");
    }

    #[test]
    fn test_parse_skill_front_matter_missing() {
        let content = "# No Front Matter\nJust content.";
        let (title, tags) = parse_skill_front_matter(content);
        assert!(title.is_empty());
        assert!(tags.is_empty());
    }

    #[test]
    fn test_should_create_skill_requires_min_calls() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("skills.db");
        let index = SkillIndex::open(&db_path).unwrap();
        let config = SkillCreationConfig::default();

        // Only 2 successful calls — should not create
        let turn = AgentTurn {
            user_message: "build and deploy".into(),
            tool_calls: vec![
                ToolCallRecord {
                    name: "shell".into(),
                    success: true,
                    args: serde_json::json!({}),
                    output: "ok".into(),
                },
                ToolCallRecord {
                    name: "file_write".into(),
                    success: true,
                    args: serde_json::json!({}),
                    output: "ok".into(),
                },
            ],
            assistant_response: "done".into(),
        };
        assert!(!should_create_skill(&turn, &index, &config));
    }

    #[test]
    fn test_should_create_skill_requires_multiple_tool_types() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("skills.db");
        let index = SkillIndex::open(&db_path).unwrap();
        let config = SkillCreationConfig::default();

        // 3 calls but all same tool — heuristic: one-off
        let turn = AgentTurn {
            user_message: "search for files".into(),
            tool_calls: vec![
                ToolCallRecord {
                    name: "shell".into(),
                    success: true,
                    args: serde_json::json!({}),
                    output: "ok".into(),
                },
                ToolCallRecord {
                    name: "shell".into(),
                    success: true,
                    args: serde_json::json!({}),
                    output: "ok".into(),
                },
                ToolCallRecord {
                    name: "shell".into(),
                    success: true,
                    args: serde_json::json!({}),
                    output: "ok".into(),
                },
            ],
            assistant_response: "found files".into(),
        };
        assert!(!should_create_skill(&turn, &index, &config));
    }
}
