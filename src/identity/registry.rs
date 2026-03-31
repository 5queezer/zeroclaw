//! Persistent caller registry — tracks caller identity across session boundaries.
//!
//! Addresses the attack surface described in *Agents of Chaos* (arXiv:2602.20021,
//! Case Study #8): without cross-session memory, a fresh session is a full trust
//! reset. This module persists caller trust levels and suspicion flags so they
//! survive across sessions.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Trust level assigned to a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// The workspace owner — full privileges.
    Owner,
    /// Explicitly trusted by the owner.
    Trusted,
    /// Default for unknown callers — restricted privileges.
    Untrusted,
}

impl fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Owner => write!(f, "owner"),
            Self::Trusted => write!(f, "trusted"),
            Self::Untrusted => write!(f, "untrusted"),
        }
    }
}

impl TrustLevel {
    /// Parse from string, case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "owner" => Some(Self::Owner),
            "trusted" => Some(Self::Trusted),
            "untrusted" => Some(Self::Untrusted),
            _ => None,
        }
    }
}

/// A single caller entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerEntry {
    /// Platform-specific caller identifier (e.g. Telegram user ID, Discord user ID).
    pub caller_id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Current trust level.
    pub trust_level: TrustLevel,
    /// Suspicion or status flags (e.g. "spoofing-attempt", "display-name-changed").
    #[serde(default)]
    pub flags: Vec<String>,
    /// Last time this caller was seen.
    pub last_seen: DateTime<Utc>,
}

/// Persistent caller registry backed by a JSON file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerRegistry {
    /// Map from `caller_id` to entry.
    #[serde(flatten)]
    entries: HashMap<String, CallerEntry>,
}

impl CallerRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Load from a JSON file. Returns an empty registry if the file does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let content =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        if content.trim().is_empty() {
            return Ok(Self::new());
        }
        serde_json::from_str(&content)
            .with_context(|| format!("parse caller registry at {}", path.display()))
    }

    /// Save registry to a JSON file, creating parent directories as needed.
    ///
    /// Uses atomic write (tmp + fsync + rename) so an interrupted write cannot
    /// corrupt the registry file.
    ///
    /// **Concurrency note:** concurrent CLI invocations performing read-modify-write
    /// can race. The atomic rename prevents corruption (last writer wins), but a
    /// concurrent update may be silently lost. This is acceptable for interactive
    /// CLI usage where concurrent identity commands are rare.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serialize caller registry")?;

        // Atomic write: tmp → fsync → rename
        let tmp = path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("create temp file {}", tmp.display()))?;
        f.write_all(json.as_bytes())
            .with_context(|| format!("write temp file {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))
    }

    /// Look up a caller. Returns `None` if not registered.
    pub fn get(&self, caller_id: &str) -> Option<&CallerEntry> {
        self.entries.get(caller_id)
    }

    /// Record a caller sighting. Creates entry at `Untrusted` if new, updates
    /// `last_seen` and `display_name` if existing.
    pub fn record_seen(&mut self, caller_id: &str, display_name: &str) -> &CallerEntry {
        let entry = self
            .entries
            .entry(caller_id.to_string())
            .or_insert_with(|| CallerEntry {
                caller_id: caller_id.to_string(),
                display_name: display_name.to_string(),
                trust_level: TrustLevel::Untrusted,
                flags: Vec::new(),
                last_seen: Utc::now(),
            });
        entry.display_name = display_name.to_string();
        entry.last_seen = Utc::now();
        entry
    }

    /// Set the trust level for a caller. Creates entry if missing.
    pub fn set_trust(&mut self, caller_id: &str, level: TrustLevel) {
        let entry = self
            .entries
            .entry(caller_id.to_string())
            .or_insert_with(|| CallerEntry {
                caller_id: caller_id.to_string(),
                display_name: caller_id.to_string(),
                trust_level: level,
                flags: Vec::new(),
                last_seen: Utc::now(),
            });
        entry.trust_level = level;
    }

    /// Add a flag to a caller. No-op if the flag already exists.
    pub fn add_flag(&mut self, caller_id: &str, flag: &str) {
        let entry = self
            .entries
            .entry(caller_id.to_string())
            .or_insert_with(|| CallerEntry {
                caller_id: caller_id.to_string(),
                display_name: caller_id.to_string(),
                trust_level: TrustLevel::Untrusted,
                flags: Vec::new(),
                last_seen: Utc::now(),
            });
        if !entry.flags.iter().any(|f| f == flag) {
            entry.flags.push(flag.to_string());
        }
    }

    /// Remove a flag from a caller. Returns `true` if the flag was present.
    pub fn remove_flag(&mut self, caller_id: &str, flag: &str) -> bool {
        if let Some(entry) = self.entries.get_mut(caller_id) {
            let before = entry.flags.len();
            entry.flags.retain(|f| f != flag);
            entry.flags.len() < before
        } else {
            false
        }
    }

    /// Return all entries, sorted by caller_id for stable output.
    pub fn list(&self) -> Vec<&CallerEntry> {
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_by(|a, b| a.caller_id.cmp(&b.caller_id));
        entries
    }

    /// Number of registered callers.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Default registry file path: `<workspace_dir>/identity/caller_registry.json`
pub fn registry_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("identity").join("caller_registry.json")
}

/// Load the caller registry from the workspace.
pub fn load_registry(workspace_dir: &Path) -> Result<CallerRegistry> {
    CallerRegistry::load(&registry_path(workspace_dir))
}

/// Save the caller registry to the workspace.
pub fn save_registry(workspace_dir: &Path, registry: &CallerRegistry) -> Result<()> {
    registry.save(&registry_path(workspace_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn empty_registry_from_missing_file() {
        let dir = TempDir::new().unwrap();
        let reg = load_registry(dir.path()).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn record_seen_creates_untrusted_entry() {
        let mut reg = CallerRegistry::new();
        reg.record_seen("user:123", "Alice");
        let entry = reg.get("user:123").unwrap();
        assert_eq!(entry.trust_level, TrustLevel::Untrusted);
        assert_eq!(entry.display_name, "Alice");
    }

    #[test]
    fn set_trust_level() {
        let mut reg = CallerRegistry::new();
        reg.record_seen("user:123", "Alice");
        reg.set_trust("user:123", TrustLevel::Trusted);
        assert_eq!(
            reg.get("user:123").unwrap().trust_level,
            TrustLevel::Trusted
        );
    }

    #[test]
    fn add_and_remove_flags() {
        let mut reg = CallerRegistry::new();
        reg.add_flag("user:123", "spoofing-attempt");
        reg.add_flag("user:123", "spoofing-attempt"); // duplicate is no-op
        let entry = reg.get("user:123").unwrap();
        assert_eq!(entry.flags.len(), 1);
        assert_eq!(entry.flags[0], "spoofing-attempt");

        assert!(reg.remove_flag("user:123", "spoofing-attempt"));
        assert!(!reg.remove_flag("user:123", "spoofing-attempt")); // already removed
        assert!(reg.get("user:123").unwrap().flags.is_empty());
    }

    #[test]
    fn persist_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut reg = CallerRegistry::new();
        reg.record_seen("tg:42", "Bob");
        reg.set_trust("tg:42", TrustLevel::Owner);
        reg.add_flag("tg:42", "verified");
        save_registry(dir.path(), &reg).unwrap();

        let loaded = load_registry(dir.path()).unwrap();
        let entry = loaded.get("tg:42").unwrap();
        assert_eq!(entry.trust_level, TrustLevel::Owner);
        assert_eq!(entry.display_name, "Bob");
        assert_eq!(entry.flags, vec!["verified"]);
    }

    #[test]
    fn list_is_sorted() {
        let mut reg = CallerRegistry::new();
        reg.record_seen("z_user", "Zara");
        reg.record_seen("a_user", "Amy");
        reg.record_seen("m_user", "Max");
        let ids: Vec<_> = reg.list().iter().map(|e| e.caller_id.as_str()).collect();
        assert_eq!(ids, vec!["a_user", "m_user", "z_user"]);
    }

    #[test]
    fn trust_level_display_and_parse() {
        assert_eq!(TrustLevel::Owner.to_string(), "owner");
        assert_eq!(TrustLevel::Trusted.to_string(), "trusted");
        assert_eq!(TrustLevel::Untrusted.to_string(), "untrusted");

        assert_eq!(TrustLevel::parse("Owner"), Some(TrustLevel::Owner));
        assert_eq!(TrustLevel::parse("TRUSTED"), Some(TrustLevel::Trusted));
        assert_eq!(TrustLevel::parse("untrusted"), Some(TrustLevel::Untrusted));
        assert_eq!(TrustLevel::parse("invalid"), None);
    }

    #[test]
    fn record_seen_updates_display_name() {
        let mut reg = CallerRegistry::new();
        reg.record_seen("user:1", "OldName");
        reg.record_seen("user:1", "NewName");
        assert_eq!(reg.get("user:1").unwrap().display_name, "NewName");
    }
}
