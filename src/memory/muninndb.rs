use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use urlencoding::encode as urlencode;

/// MuninnDB cognitive memory backend.
///
/// Connects to a running MuninnDB instance via its REST API.
/// MuninnDB provides semantic search with Hebbian reinforcement,
/// Ebbinghaus decay, and associative recall — all handled server-side.
/// No local embedder required.
pub struct MuninndbMemory {
    client: reqwest::Client,
    base_url: String,
    vault: String,
    api_key: Option<String>,
}

// ── MuninnDB API types ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct WriteRequest {
    concept: String,
    content: String,
    vault: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct WriteResponse {
    id: String,
}

#[derive(Serialize)]
struct ActivateRequest {
    context: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    threshold: Option<f32>,
    max_results: usize,
    vault: String,
}

#[derive(Deserialize)]
struct ActivateResponse {
    #[serde(default)]
    activations: Vec<ActivationItem>,
}

#[derive(Deserialize)]
struct ActivationItem {
    id: String,
    #[serde(default)]
    concept: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    score: f64,
}

#[derive(Deserialize)]
struct ReadResponse {
    id: String,
    #[serde(default)]
    concept: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct ListEngramsResponse {
    #[serde(default)]
    engrams: Vec<ReadResponse>,
    #[serde(default)]
    total: usize,
}

#[derive(Deserialize)]
struct StatsResponse {
    #[serde(default, alias = "total_engrams")]
    engram_count: usize,
}

impl MuninndbMemory {
    pub fn new(url: &str, vault: &str, api_key: Option<String>) -> Self {
        let base_url = url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("muninndb: reqwest client builder failed ({e}), using default");
                reqwest::Client::new()
            });

        Self {
            client,
            base_url,
            vault: vault.to_string(),
            api_key,
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.request(method, &url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    /// Map a ZeroClaw MemoryCategory to a MuninnDB tag.
    fn category_tag(category: &MemoryCategory) -> String {
        format!("zc:{category}")
    }

    /// Fetch full engram data by ID (includes tags, timestamps).
    async fn fetch_engram(&self, id: &str) -> Result<Option<ReadResponse>> {
        let path = format!(
            "/api/engrams/{}?vault={}",
            urlencode(id),
            urlencode(&self.vault)
        );
        let resp = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .context("muninndb: failed to fetch engram")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        Ok(Some(resp.json().await?))
    }

    /// Convert a MuninnDB engram to a ZeroClaw MemoryEntry.
    fn to_entry(engram: &ReadResponse, score: Option<f64>) -> MemoryEntry {
        let category = engram
            .tags
            .iter()
            .find(|t| t.starts_with("zc:"))
            .map(|t| t.strip_prefix("zc:").unwrap_or("core"))
            .unwrap_or("core");

        let session_id = engram
            .tags
            .iter()
            .find(|t| t.starts_with("session:"))
            .map(|t| t.strip_prefix("session:").unwrap_or("").to_string());

        // MuninnDB returns nanoseconds from the read endpoint but seconds
        // from the list endpoint. Disambiguate by magnitude.
        let ts = if engram.created_at > 0 {
            let dt = if engram.created_at > 1_000_000_000_000_000 {
                // Nanoseconds (read endpoint)
                chrono::DateTime::from_timestamp_nanos(engram.created_at)
            } else {
                // Seconds (list endpoint)
                chrono::DateTime::from_timestamp(engram.created_at, 0)
                    .unwrap_or_else(chrono::Utc::now)
            };
            dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        } else {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };

        MemoryEntry {
            id: engram.id.clone(),
            key: engram.concept.clone(),
            content: engram.content.clone(),
            category: serde_json::from_value(serde_json::Value::String(category.to_string()))
                .unwrap_or(MemoryCategory::Core),
            timestamp: ts,
            session_id,
            score,
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
        }
    }
}

#[async_trait]
impl Memory for MuninndbMemory {
    fn name(&self) -> &str {
        "muninndb"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let mut tags = vec![Self::category_tag(&category)];
        if let Some(sid) = session_id {
            tags.push(format!("session:{sid}"));
        }

        let body = WriteRequest {
            concept: key.to_string(),
            content: content.to_string(),
            vault: self.vault.clone(),
            tags,
        };

        let resp = self
            .request(reqwest::Method::POST, "/api/engrams")
            .json(&body)
            .send()
            .await
            .context("muninndb: failed to store engram")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("muninndb store failed ({status}): {body}");
        }

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let body = ActivateRequest {
            context: vec![query.to_string()],
            threshold: Some(0.3),
            // Over-fetch to allow for client-side filtering.
            max_results: limit * 2,
            vault: self.vault.clone(),
        };

        let resp = self
            .request(reqwest::Method::POST, "/api/activate")
            .json(&body)
            .send()
            .await
            .context("muninndb: failed to activate")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("muninndb recall failed ({status}): {body}");
        }

        let result: ActivateResponse = resp.json().await?;

        // Fetch full engram data for each activation to get tags/timestamps.
        let mut entries = Vec::with_capacity(result.activations.len());
        for item in &result.activations {
            let engram = self.fetch_engram(&item.id).await?;
            let engram = engram.unwrap_or_else(|| ReadResponse {
                id: item.id.clone(),
                concept: item.concept.clone(),
                content: item.content.clone(),
                created_at: 0,
                tags: vec![],
            });
            entries.push(Self::to_entry(&engram, Some(item.score)));
        }

        // Apply session_id filter.
        if let Some(sid) = session_id {
            entries.retain(|e| e.session_id.as_deref() == Some(sid));
        }

        // Apply time-range filters.
        if let Some(since_str) = since {
            if let Ok(since_ts) = chrono::DateTime::parse_from_rfc3339(since_str) {
                entries.retain(|e| {
                    chrono::DateTime::parse_from_rfc3339(&e.timestamp)
                        .map(|t| t >= since_ts)
                        .unwrap_or(true)
                });
            }
        }
        if let Some(until_str) = until {
            if let Ok(until_ts) = chrono::DateTime::parse_from_rfc3339(until_str) {
                entries.retain(|e| {
                    chrono::DateTime::parse_from_rfc3339(&e.timestamp)
                        .map(|t| t <= until_ts)
                        .unwrap_or(true)
                });
            }
        }

        entries.truncate(limit);
        Ok(entries)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        // MuninnDB uses IDs, not keys. Try to find by concept via semantic
        // search. This is approximate — an exact-match API would be better,
        // but MuninnDB doesn't expose one today.
        let body = ActivateRequest {
            context: vec![key.to_string()],
            threshold: Some(0.8),
            max_results: 5,
            vault: self.vault.clone(),
        };

        let resp = self
            .request(reqwest::Method::POST, "/api/activate")
            .json(&body)
            .send()
            .await
            .context("muninndb: failed to get engram")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let result: ActivateResponse = resp.json().await?;

        // Find an exact concept match among the top results.
        if let Some(item) = result.activations.iter().find(|a| a.concept == key) {
            let engram = self
                .fetch_engram(&item.id)
                .await?
                .unwrap_or_else(|| ReadResponse {
                    id: item.id.clone(),
                    concept: item.concept.clone(),
                    content: item.content.clone(),
                    created_at: 0,
                    tags: vec![],
                });
            return Ok(Some(Self::to_entry(&engram, Some(item.score))));
        }

        Ok(None)
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Build query with optional server-side tag filter for category.
        let mut path = format!("/api/engrams?vault={}&limit=200", urlencode(&self.vault));
        if let Some(cat) = category {
            use std::fmt::Write;
            let _ = write!(path, "&tags={}", urlencode(&Self::category_tag(cat)));
        }

        let resp = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .context("muninndb: failed to list engrams")?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let result: ListEngramsResponse = resp.json().await?;
        let mut entries: Vec<MemoryEntry> = result
            .engrams
            .iter()
            .map(|e| Self::to_entry(e, None))
            .collect();

        // Client-side session filter (not supported server-side).
        if let Some(sid) = session_id {
            entries.retain(|e| e.session_id.as_deref() == Some(sid));
        }

        Ok(entries)
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        // Key is expected to be a MuninnDB ULID.
        let path = format!(
            "/api/engrams/{}?vault={}",
            urlencode(key),
            urlencode(&self.vault)
        );
        let resp = self
            .request(reqwest::Method::DELETE, &path)
            .send()
            .await
            .context("muninndb: failed to forget engram")?;

        Ok(resp.status().is_success())
    }

    async fn count(&self) -> Result<usize> {
        let path = format!("/api/stats?vault={}", urlencode(&self.vault));
        let resp = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .context("muninndb: failed to get stats")?;

        if !resp.status().is_success() {
            return Ok(0);
        }

        let stats: StatsResponse = resp
            .json()
            .await
            .unwrap_or(StatsResponse { engram_count: 0 });
        Ok(stats.engram_count)
    }

    async fn health_check(&self) -> bool {
        self.request(reqwest::Method::GET, "/api/health")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_tag_formatting() {
        assert_eq!(
            MuninndbMemory::category_tag(&MemoryCategory::Core),
            "zc:core"
        );
        assert_eq!(
            MuninndbMemory::category_tag(&MemoryCategory::Daily),
            "zc:daily"
        );
        assert_eq!(
            MuninndbMemory::category_tag(&MemoryCategory::Custom("project".into())),
            "zc:project"
        );
    }

    #[test]
    fn to_entry_extracts_category_from_tags() {
        let engram = ReadResponse {
            id: "01ABC".into(),
            concept: "test-key".into(),
            content: "test content".into(),
            created_at: 0,
            tags: vec!["zc:daily".into(), "session:s1".into()],
        };

        let entry = MuninndbMemory::to_entry(&engram, Some(0.9));
        assert_eq!(entry.key, "test-key");
        assert_eq!(entry.category, MemoryCategory::Daily);
        assert_eq!(entry.session_id.as_deref(), Some("s1"));
        assert_eq!(entry.score, Some(0.9));
    }

    /// Integration tests against a live MuninnDB instance.
    /// Run with: MUNINNDB_TEST_URL=http://127.0.0.1:18475 cargo test --lib memory::muninndb::tests::e2e -- --nocapture
    mod e2e {
        use super::*;

        fn test_url() -> Option<String> {
            std::env::var("MUNINNDB_TEST_URL").ok()
        }

        #[tokio::test]
        async fn full_lifecycle() {
            let Some(url) = test_url() else {
                eprintln!("MUNINNDB_TEST_URL not set, skipping e2e");
                return;
            };

            let mem = MuninndbMemory::new(&url, "default", None);

            // health
            assert!(mem.health_check().await, "health check failed");
            eprintln!("  ✓ health_check");

            let before = mem.count().await.unwrap();

            // store
            mem.store(
                "zc-e2e-test",
                "Zeroclaw integration test verifying MuninnDB memory backend.",
                MemoryCategory::Daily,
                Some("sess-e2e"),
            )
            .await
            .unwrap();
            eprintln!("  ✓ store");

            // count went up
            let after = mem.count().await.unwrap();
            assert!(
                after > before,
                "count did not increase: {before} -> {after}"
            );
            eprintln!("  ✓ count ({before} -> {after})");

            // list (unfiltered)
            let all = mem.list(None, None).await.unwrap();
            assert!(
                all.iter().any(|e| e.key == "zc-e2e-test"),
                "list missing stored entry"
            );
            eprintln!("  ✓ list (unfiltered, {} entries)", all.len());

            // list (category filter)
            let daily = mem.list(Some(&MemoryCategory::Daily), None).await.unwrap();
            assert!(
                daily.iter().any(|e| e.key == "zc-e2e-test"),
                "list(Daily) missing entry"
            );
            let core = mem.list(Some(&MemoryCategory::Core), None).await.unwrap();
            assert!(
                !core.iter().any(|e| e.key == "zc-e2e-test"),
                "list(Core) should not contain Daily entry"
            );
            eprintln!("  ✓ list (category filter)");

            // list (session filter)
            let sess = mem.list(None, Some("sess-e2e")).await.unwrap();
            assert!(
                sess.iter().any(|e| e.key == "zc-e2e-test"),
                "list(session) missing entry"
            );
            let wrong_sess = mem.list(None, Some("no-such-session")).await.unwrap();
            assert!(
                !wrong_sess.iter().any(|e| e.key == "zc-e2e-test"),
                "list(wrong session) should not contain entry"
            );
            eprintln!("  ✓ list (session filter)");

            // recall (semantic)
            // Give the embedder a moment to process.
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            let recalled = mem
                .recall("zeroclaw integration backend", 10, None, None, None)
                .await
                .unwrap();
            assert!(
                recalled.iter().any(|e| e.key == "zc-e2e-test"),
                "recall did not find entry; got: {:?}",
                recalled.iter().map(|e| &e.key).collect::<Vec<_>>()
            );
            // Verify metadata was fetched (tags -> category/session).
            let hit = recalled.iter().find(|e| e.key == "zc-e2e-test").unwrap();
            assert_eq!(hit.category, MemoryCategory::Daily, "recall lost category");
            assert_eq!(
                hit.session_id.as_deref(),
                Some("sess-e2e"),
                "recall lost session_id"
            );
            assert!(hit.score.is_some(), "recall missing score");
            eprintln!(
                "  ✓ recall (semantic, score={:.3}, category={:?}, session={:?})",
                hit.score.unwrap(),
                hit.category,
                hit.session_id
            );

            // recall (session filter)
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let recalled_sess = mem
                .recall("zeroclaw integration", 10, Some("sess-e2e"), None, None)
                .await
                .unwrap();
            assert!(
                recalled_sess.iter().any(|e| e.key == "zc-e2e-test"),
                "recall(session) missing entry"
            );
            let recalled_wrong = mem
                .recall("zeroclaw integration", 10, Some("other"), None, None)
                .await
                .unwrap();
            assert!(
                !recalled_wrong.iter().any(|e| e.key == "zc-e2e-test"),
                "recall(wrong session) should not contain entry"
            );
            eprintln!("  ✓ recall (session filter)");

            // get (exact key)
            let got = mem.get("zc-e2e-test").await.unwrap();
            assert!(got.is_some(), "get returned None for stored key");
            let got = got.unwrap();
            assert_eq!(got.key, "zc-e2e-test");
            assert_eq!(got.category, MemoryCategory::Daily);
            eprintln!("  ✓ get");

            // get (miss)
            let miss = mem.get("zc-e2e-definitely-not-stored").await.unwrap();
            assert!(miss.is_none(), "get returned Some for missing key");
            eprintln!("  ✓ get (miss)");

            // forget
            let found = all.iter().find(|e| e.key == "zc-e2e-test").unwrap();
            let forgotten = mem.forget(&found.id).await.unwrap();
            assert!(forgotten, "forget returned false");
            eprintln!("  ✓ forget");
        }
    }
}
