//! Deferred MCP tool loading — stubs and activated-tool tracking.
//!
//! When `mcp.deferred_loading` is enabled, MCP tool schemas are NOT eagerly
//! included in the LLM context window. Instead, only lightweight stubs (name +
//! description) are exposed in the system prompt. The LLM must call the built-in
//! `tool_search` tool to fetch full schemas, which moves them into the
//! [`ActivatedToolSet`] for the current conversation.

use std::collections::HashMap;
use std::sync::Arc;

use crate::tools::mcp_client::McpRegistry;
use crate::tools::mcp_protocol::McpToolDef;
use crate::tools::mcp_tool::McpToolWrapper;
use crate::tools::traits::{Tool, ToolSpec};

// ── BM25 Index ──────────────────────────────────────────────────────────

/// Precomputed corpus statistics for BM25 scoring.
struct BM25Index {
    /// Tokenized documents (one per stub, same order as `stubs`).
    docs: Vec<Vec<String>>,
    /// Document lengths (in tokens).
    doc_lengths: Vec<f64>,
    /// Average document length across the corpus.
    avgdl: f64,
    /// Number of documents containing each term.
    doc_freq: HashMap<String, usize>,
    /// Total number of documents.
    n: usize,
}

impl BM25Index {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;

    /// Build the index from stub name+description pairs.
    fn build(stubs: &[DeferredMcpToolStub]) -> Self {
        let n = stubs.len();
        let mut docs = Vec::with_capacity(n);
        let mut doc_lengths = Vec::with_capacity(n);
        let mut doc_freq: HashMap<String, usize> = HashMap::new();

        for stub in stubs {
            let tokens = Self::tokenize(&stub.prefixed_name, &stub.description);
            doc_lengths.push(tokens.len() as f64);

            // Count unique terms per document for doc frequency
            let mut seen = std::collections::HashSet::new();
            for token in &tokens {
                if seen.insert(token.clone()) {
                    *doc_freq.entry(token.clone()).or_insert(0) += 1;
                }
            }
            docs.push(tokens);
        }

        let total_len: f64 = doc_lengths.iter().sum();
        let avgdl = if n > 0 { total_len / n as f64 } else { 1.0 };

        Self {
            docs,
            doc_lengths,
            avgdl,
            doc_freq,
            n,
        }
    }

    /// Tokenize a name and description into lowercase terms, splitting on
    /// whitespace, underscores, and hyphens.
    fn tokenize(name: &str, description: &str) -> Vec<String> {
        let combined = format!("{} {}", name, description);
        combined
            .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect()
    }

    /// Check if a query term matches a document token.
    /// Matches if either starts with the other (handles "file"/"files" variants).
    #[inline]
    fn term_matches(query_term: &str, doc_token: &str) -> bool {
        doc_token.starts_with(query_term) || query_term.starts_with(doc_token)
    }

    /// Precompute IDF values for query terms. Called once per search() to avoid
    /// repeated full-corpus scans inside score().
    fn precompute_idfs(&self, query_terms: &[String]) -> Vec<f64> {
        query_terms
            .iter()
            .map(|qt| {
                let nq = match self.doc_freq.get(qt) {
                    Some(&n) => n as f64,
                    None => self
                        .docs
                        .iter()
                        .filter(|d| d.iter().any(|t| Self::term_matches(qt, t)))
                        .count() as f64,
                };
                ((self.n as f64 - nq + 0.5) / (nq + 0.5) + 1.0).ln()
            })
            .collect()
    }

    /// Score a query against document at `doc_idx`.
    /// Uses prefix matching for term frequency to handle morphological
    /// variants (e.g. "file" matches "files" and vice versa).
    fn score(&self, query_terms: &[String], idfs: &[f64], doc_idx: usize) -> f64 {
        let dl = self.doc_lengths[doc_idx];
        let doc = &self.docs[doc_idx];
        let mut total = 0.0f64;

        for (qt, &idf) in query_terms.iter().zip(idfs) {
            let tf = doc.iter().filter(|t| Self::term_matches(qt, t)).count() as f64;
            if tf == 0.0 {
                continue;
            }
            let numerator = tf * (Self::K1 + 1.0);
            let denominator = tf + Self::K1 * (1.0 - Self::B + Self::B * dl / self.avgdl);
            total += idf * numerator / denominator;
        }

        total
    }
}

// ── DeferredMcpToolStub ──────────────────────────────────────────────────

/// A lightweight stub representing a known-but-not-yet-loaded MCP tool.
/// Contains only the prefixed name, a human-readable description, and enough
/// information to construct the full [`McpToolWrapper`] on activation.
#[derive(Debug, Clone)]
pub struct DeferredMcpToolStub {
    /// Prefixed name: `<server_name>__<tool_name>`.
    pub prefixed_name: String,
    /// Human-readable description (extracted from the MCP tool definition).
    pub description: String,
    /// The full tool definition — stored so we can construct a wrapper later.
    def: McpToolDef,
}

impl DeferredMcpToolStub {
    pub fn new(prefixed_name: String, def: McpToolDef) -> Self {
        let description = def
            .description
            .clone()
            .unwrap_or_else(|| "MCP tool".to_string());
        Self {
            prefixed_name,
            description,
            def,
        }
    }

    /// Materialize this stub into a live [`McpToolWrapper`].
    pub fn activate(&self, registry: Arc<McpRegistry>) -> McpToolWrapper {
        McpToolWrapper::new(self.prefixed_name.clone(), self.def.clone(), registry)
    }
}

// ── DeferredMcpToolSet ───────────────────────────────────────────────────

/// Collection of all deferred MCP tool stubs discovered at startup.
/// Provides BM25-ranked keyword search for `tool_search`.
#[derive(Clone)]
pub struct DeferredMcpToolSet {
    /// All stubs — exposed for test construction.
    pub stubs: Vec<DeferredMcpToolStub>,
    /// Shared registry — exposed for test construction.
    pub registry: Arc<McpRegistry>,
    /// Precomputed BM25 index over stub names + descriptions.
    bm25: Arc<BM25Index>,
}

impl DeferredMcpToolSet {
    /// Build the set from a connected [`McpRegistry`], excluding tools whose
    /// prefixed names match any of the `eager_patterns` globs.
    pub async fn from_registry(registry: Arc<McpRegistry>, eager_patterns: &[String]) -> Self {
        let names = registry.tool_names();
        let mut stubs = Vec::with_capacity(names.len());
        for name in names {
            if is_eager_match(&name, eager_patterns) {
                continue;
            }
            if let Some(def) = registry.get_tool_def(&name).await {
                stubs.push(DeferredMcpToolStub::new(name, def));
            }
        }
        let bm25 = Arc::new(BM25Index::build(&stubs));
        Self {
            stubs,
            registry,
            bm25,
        }
    }

    /// Build from pre-constructed stubs (for tests and internal use).
    pub(crate) fn from_stubs(stubs: Vec<DeferredMcpToolStub>, registry: Arc<McpRegistry>) -> Self {
        let bm25 = Arc::new(BM25Index::build(&stubs));
        Self {
            stubs,
            registry,
            bm25,
        }
    }

    /// All stub names (for rendering in the system prompt).
    pub fn stub_names(&self) -> Vec<&str> {
        self.stubs
            .iter()
            .map(|s| s.prefixed_name.as_str())
            .collect()
    }

    /// Number of deferred stubs.
    pub fn len(&self) -> usize {
        self.stubs.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.stubs.is_empty()
    }

    /// Look up stubs by exact name. Used for `select:name1,name2` queries.
    pub fn get_by_name(&self, name: &str) -> Option<&DeferredMcpToolStub> {
        self.stubs.iter().find(|s| s.prefixed_name == name)
    }

    /// BM25 keyword search — returns stubs ranked by Okapi BM25 relevance.
    /// Query is tokenized on whitespace, underscores, and hyphens.
    pub fn search(&self, query: &str, max_results: usize) -> Vec<&DeferredMcpToolStub> {
        let terms: Vec<String> = query
            .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
            .filter(|s| !s.is_empty())
            .map(|t| t.to_ascii_lowercase())
            .collect();
        if terms.is_empty() {
            return self.stubs.iter().take(max_results).collect();
        }

        let idfs = self.bm25.precompute_idfs(&terms);

        let mut scored: Vec<(usize, f64)> = self
            .stubs
            .iter()
            .enumerate()
            .map(|(idx, _)| (idx, self.bm25.score(&terms, &idfs, idx)))
            .filter(|(_, score)| *score > 0.0)
            .collect();

        // Sort by score descending, then by name ascending for deterministic tie-breaking.
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    self.stubs[a.0]
                        .prefixed_name
                        .cmp(&self.stubs[b.0].prefixed_name)
                })
        });
        scored
            .into_iter()
            .take(max_results)
            .map(|(idx, _)| &self.stubs[idx])
            .collect()
    }

    /// Activate a stub by name, returning a boxed [`Tool`].
    pub fn activate(&self, name: &str) -> Option<Box<dyn Tool>> {
        self.get_by_name(name).map(|stub| {
            let wrapper = stub.activate(Arc::clone(&self.registry));
            Box::new(wrapper) as Box<dyn Tool>
        })
    }

    /// Return the full [`ToolSpec`] for a stub (for inclusion in `tool_search` results).
    pub fn tool_spec(&self, name: &str) -> Option<ToolSpec> {
        self.get_by_name(name).map(|stub| {
            let wrapper = stub.activate(Arc::clone(&self.registry));
            wrapper.spec()
        })
    }
}

// ── Eager tool matching ──────────────────────────────────────────────────

/// Build server-scoped eager patterns from per-server config.
/// Each pattern is prefixed with `server__` so it matches the prefixed tool name.
pub fn build_eager_patterns(servers: &[(String, Vec<String>)]) -> Vec<String> {
    servers
        .iter()
        .flat_map(|(server_name, patterns)| {
            patterns
                .iter()
                .map(move |p| format!("{}__{}", server_name, p))
        })
        .collect()
}

/// Check if a tool's prefixed name matches any of the eager patterns.
/// Supports simple glob matching with `*` wildcards:
/// - `"*"` matches everything
/// - `"*suffix"` matches names ending with `suffix`
/// - `"prefix*"` matches names starting with `prefix`
/// - `"*infix*"` matches names containing `infix`
/// - `"prefix*infix*"` matches names starting with `prefix` and containing `infix` after it
///
/// General rule: split on `*`, check that all literal segments appear in order.
/// The first segment must be a prefix; the last segment must be a suffix;
/// interior segments must appear (in order) anywhere in between.
pub fn is_eager_match(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pat| {
        if !pat.contains('*') {
            return name == pat;
        }
        let parts: Vec<&str> = pat.split('*').collect();
        // All parts empty means the pattern is only `*`s — matches everything.
        if parts.iter().all(|p| p.is_empty()) {
            return true;
        }
        let mut remaining = name;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if i == 0 {
                // First segment must be a prefix
                if !remaining.starts_with(part) {
                    return false;
                }
                remaining = &remaining[part.len()..];
            } else if i == parts.len() - 1 {
                // Last segment must be a suffix
                if !remaining.ends_with(part) {
                    return false;
                }
                // No need to advance — this is the final check.
            } else {
                // Interior segment: find it anywhere in the remainder
                match remaining.find(part) {
                    Some(pos) => remaining = &remaining[pos + part.len()..],
                    None => return false,
                }
            }
        }
        true
    })
}

// ── ActivatedToolSet ─────────────────────────────────────────────────────

/// Per-conversation mutable state tracking which deferred tools have been
/// activated (i.e. their full schemas have been fetched via `tool_search`).
/// The agent loop consults this each iteration to decide which tool_specs
/// to include in the LLM request.
pub struct ActivatedToolSet {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ActivatedToolSet {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn activate(&mut self, name: String, tool: Arc<dyn Tool>) {
        self.tools.insert(name, tool);
    }

    pub fn is_activated(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Clone the Arc so the caller can drop the mutex guard before awaiting.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Resolve an activated tool by exact name first, then by unique MCP suffix.
    ///
    /// Some providers occasionally strip the `<server>__` prefix when calling a
    /// deferred MCP tool after `tool_search` activation. When the suffix maps to
    /// exactly one activated tool, allow that call to proceed.
    pub fn get_resolved(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(tool) = self.get(name) {
            return Some(tool);
        }
        if name.contains("__") {
            return None;
        }

        let mut resolved = None;
        for (tool_name, tool) in &self.tools {
            let Some((_, suffix)) = tool_name.split_once("__") else {
                continue;
            };
            if suffix != name {
                continue;
            }
            if resolved.is_some() {
                return None;
            }
            resolved = Some(Arc::clone(tool));
        }

        resolved
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for ActivatedToolSet {
    fn default() -> Self {
        Self::new()
    }
}

// ── System prompt helper ─────────────────────────────────────────────────

/// Build the `<available-deferred-tools>` section for the system prompt.
/// Lists only tool names so the LLM knows what is available without
/// consuming context window on full schemas. Includes an instruction
/// block that tells the LLM to call `tool_search` to activate them.
pub fn build_deferred_tools_section(deferred: &DeferredMcpToolSet) -> String {
    if deferred.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("## Deferred Tools\n\n");
    out.push_str(
        "The tools listed below are available but NOT yet loaded. \
         To use any of them you MUST first call the `tool_search` tool \
         to fetch their full schemas. Use `\"select:name1,name2\"` for \
         exact tools or keywords to search. Once activated, the tools \
         become callable for the rest of the conversation.\n\n",
    );
    out.push_str("<available-deferred-tools>\n");
    for stub in &deferred.stubs {
        out.push_str(&stub.prefixed_name);
        out.push_str(" - ");
        out.push_str(&stub.description);
        out.push('\n');
    }
    out.push_str("</available-deferred-tools>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stub(name: &str, desc: &str) -> DeferredMcpToolStub {
        let def = McpToolDef {
            name: name.to_string(),
            description: Some(desc.to_string()),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        };
        DeferredMcpToolStub::new(name.to_string(), def)
    }

    /// Helper to build a test DeferredMcpToolSet with BM25 index.
    fn make_set(stubs: Vec<DeferredMcpToolStub>) -> DeferredMcpToolSet {
        let registry = std::sync::Arc::new(
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(McpRegistry::connect_all(&[]))
                .unwrap(),
        );
        DeferredMcpToolSet::from_stubs(stubs, registry)
    }

    #[test]
    fn stub_uses_description_from_def() {
        let stub = make_stub("fs__read", "Read a file");
        assert_eq!(stub.description, "Read a file");
    }

    #[test]
    fn stub_defaults_description_when_none() {
        let def = McpToolDef {
            name: "mystery".into(),
            description: None,
            input_schema: serde_json::json!({}),
        };
        let stub = DeferredMcpToolStub::new("srv__mystery".into(), def);
        assert_eq!(stub.description, "MCP tool");
    }

    #[test]
    fn activated_set_tracks_activation() {
        use crate::tools::traits::ToolResult;
        use async_trait::async_trait;

        struct FakeTool;
        #[async_trait]
        impl Tool for FakeTool {
            fn name(&self) -> &str {
                "fake"
            }
            fn description(&self) -> &str {
                "fake tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }

        let mut set = ActivatedToolSet::new();
        assert!(!set.is_activated("fake"));
        set.activate("fake".into(), Arc::new(FakeTool));
        assert!(set.is_activated("fake"));
        assert!(set.get("fake").is_some());
        assert_eq!(set.tool_specs().len(), 1);
    }

    #[test]
    fn activated_set_resolves_unique_suffix() {
        use crate::tools::traits::ToolResult;
        use async_trait::async_trait;

        struct FakeTool;
        #[async_trait]
        impl Tool for FakeTool {
            fn name(&self) -> &str {
                "docker-mcp__extract_text"
            }
            fn description(&self) -> &str {
                "fake tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }

        let mut set = ActivatedToolSet::new();
        set.activate("docker-mcp__extract_text".into(), Arc::new(FakeTool));
        assert!(set.get_resolved("extract_text").is_some());
    }

    #[test]
    fn activated_set_rejects_ambiguous_suffix() {
        use crate::tools::traits::ToolResult;
        use async_trait::async_trait;

        struct FakeTool(&'static str);
        #[async_trait]
        impl Tool for FakeTool {
            fn name(&self) -> &str {
                self.0
            }
            fn description(&self) -> &str {
                "fake tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }

        let mut set = ActivatedToolSet::new();
        set.activate(
            "docker-mcp__extract_text".into(),
            Arc::new(FakeTool("docker-mcp__extract_text")),
        );
        set.activate(
            "ocr-mcp__extract_text".into(),
            Arc::new(FakeTool("ocr-mcp__extract_text")),
        );
        assert!(set.get_resolved("extract_text").is_none());
    }

    #[test]
    fn build_deferred_section_empty_when_no_stubs() {
        let set = make_set(vec![]);
        assert!(build_deferred_tools_section(&set).is_empty());
    }

    #[test]
    fn build_deferred_section_lists_names() {
        let set = make_set(vec![
            make_stub("fs__read_file", "Read a file"),
            make_stub("git__status", "Git status"),
        ]);
        let section = build_deferred_tools_section(&set);
        assert!(section.contains("<available-deferred-tools>"));
        assert!(section.contains("fs__read_file - Read a file"));
        assert!(section.contains("git__status - Git status"));
        assert!(section.contains("</available-deferred-tools>"));
    }

    #[test]
    fn build_deferred_section_includes_tool_search_instruction() {
        let set = make_set(vec![make_stub("fs__read_file", "Read a file")]);
        let section = build_deferred_tools_section(&set);
        assert!(
            section.contains("tool_search"),
            "deferred section must instruct the LLM to use tool_search"
        );
        assert!(
            section.contains("## Deferred Tools"),
            "deferred section must include a heading"
        );
    }

    #[test]
    fn build_deferred_section_multiple_servers() {
        let set = make_set(vec![
            make_stub("server_a__list", "List items"),
            make_stub("server_a__create", "Create item"),
            make_stub("server_b__query", "Query records"),
        ]);
        let section = build_deferred_tools_section(&set);
        assert!(section.contains("server_a__list"));
        assert!(section.contains("server_a__create"));
        assert!(section.contains("server_b__query"));
        assert!(
            section.contains("tool_search"),
            "section must mention tool_search for multi-server setups"
        );
    }

    #[test]
    fn keyword_search_ranks_by_bm25() {
        let set = make_set(vec![
            make_stub("fs__read_file", "Read a file from disk"),
            make_stub("fs__write_file", "Write a file to disk"),
            make_stub("git__log", "Show git log"),
        ]);

        // "file read" should rank fs__read_file highest (both terms match)
        let results = set.search("file read", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].prefixed_name, "fs__read_file");
    }

    #[test]
    fn bm25_ranks_rare_term_higher() {
        // "quantum" appears in only one doc, "tool" in all — BM25 should rank
        // the rare-term match higher than common-term matches.
        let set = make_set(vec![
            make_stub("srv__alpha", "A common tool for tasks"),
            make_stub("srv__beta", "Another common tool for tasks"),
            make_stub("srv__gamma", "Quantum physics simulator tool"),
        ]);

        let results = set.search("quantum", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].prefixed_name, "srv__gamma");

        // "tool" matches all 3 but with low IDF
        let results = set.search("tool", 5);
        assert_eq!(results.len(), 3);

        // "quantum tool" should still rank gamma first (rare term dominates)
        let results = set.search("quantum tool", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].prefixed_name, "srv__gamma");
    }

    #[test]
    fn get_by_name_returns_correct_stub() {
        let set = make_set(vec![
            make_stub("a__one", "Tool one"),
            make_stub("b__two", "Tool two"),
        ]);
        assert!(set.get_by_name("a__one").is_some());
        assert!(set.get_by_name("nonexistent").is_none());
    }

    #[test]
    fn search_across_multiple_servers() {
        let set = make_set(vec![
            make_stub("server_a__read_file", "Read a file from disk"),
            make_stub("server_b__read_config", "Read configuration from database"),
        ]);

        // "read" should match stubs from both servers
        let results = set.search("read", 10);
        assert_eq!(results.len(), 2);

        // "file" should match only server_a
        let results = set.search("file", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].prefixed_name, "server_a__read_file");

        // "config database" should rank server_b highest (both terms match)
        let results = set.search("config database", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].prefixed_name, "server_b__read_config");
    }

    #[test]
    fn eager_match_exact() {
        let patterns = vec!["muninn__recall".to_string()];
        assert!(is_eager_match("muninn__recall", &patterns));
        assert!(!is_eager_match("muninn__remember", &patterns));
    }

    #[test]
    fn eager_match_prefix_glob() {
        let patterns = vec!["muninn__*".to_string()];
        assert!(is_eager_match("muninn__recall", &patterns));
        assert!(is_eager_match("muninn__remember", &patterns));
        assert!(!is_eager_match("git__status", &patterns));
    }

    #[test]
    fn eager_match_suffix_glob() {
        let patterns = vec!["*__recall".to_string()];
        assert!(is_eager_match("muninn__recall", &patterns));
        assert!(is_eager_match("other__recall", &patterns));
        assert!(!is_eager_match("muninn__remember", &patterns));
    }

    #[test]
    fn eager_match_infix_glob() {
        let patterns = vec!["*recall*".to_string()];
        assert!(is_eager_match("muninn__muninn_recall", &patterns));
        assert!(is_eager_match("muninn__muninn_recall_tree", &patterns));
        assert!(!is_eager_match("muninn__muninn_remember", &patterns));

        // Server-scoped infix patterns built via build_eager_patterns
        let server_patterns =
            build_eager_patterns(&[("muninn".to_string(), vec!["*recall*".to_string()])]);
        assert_eq!(server_patterns, vec!["muninn__*recall*"]);
        assert!(is_eager_match("muninn__muninn_recall", &server_patterns));
        assert!(is_eager_match(
            "muninn__muninn_recall_tree",
            &server_patterns
        ));
        assert!(!is_eager_match("muninn__muninn_remember", &server_patterns));
    }

    #[test]
    fn eager_match_wildcard_all() {
        let patterns = vec!["*".to_string()];
        assert!(is_eager_match("anything", &patterns));
    }

    #[test]
    fn build_eager_patterns_prefixes_all() {
        let servers = vec![(
            "muninn".to_string(),
            vec!["*recall*".to_string(), "remember".to_string()],
        )];
        let patterns = build_eager_patterns(&servers);
        assert_eq!(patterns, vec!["muninn__*recall*", "muninn__remember"]);
    }
}
