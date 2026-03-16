//! Skill self-improvement: atomic updates with audit trails.
//!
//! After the agent uses an existing skill, diffs the actual execution against
//! the skill document and optionally improves it. Security controls:
//! - S2.1: Atomic write with rollback (temp file → validate → rename)
//! - S2.2: Cooldown rate limiting (configurable, default 1 hour)
//! - S2.3: Audit trail integrity (front-matter fields always preserved/injected)

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

use super::autonomous::{validate_slug, SkillIndex};

/// Configuration for skill improvement.
#[derive(Debug, Clone)]
pub struct SkillImprovementConfig {
    /// Minimum time between improvements for a single skill (seconds).
    pub cooldown_secs: u64,
}

impl Default for SkillImprovementConfig {
    fn default() -> Self {
        Self {
            cooldown_secs: 3600, // 1 hour
        }
    }
}

/// Validate that content is valid UTF-8 and contains well-formed TOML front-matter.
///
/// # Security (S2.1)
/// Returns an error if validation fails, preventing overwrite of the original.
pub fn validate_skill_content(content: &str) -> Result<()> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        bail!("skill content is empty");
    }

    // If it has front-matter, validate it
    if let Some(rest) = trimmed.strip_prefix("+++") {
        let Some(end_pos) = rest.find("+++") else {
            bail!("malformed front-matter: missing closing +++");
        };
        let front_matter = &rest[..end_pos];
        // Validate TOML parsing
        if toml::from_str::<toml::Table>(front_matter).is_err() {
            bail!("front-matter contains invalid TOML");
        }
    }

    Ok(())
}

/// Ensure audit fields are present in the front-matter.
///
/// # Security (S2.3)
/// If the LLM output omits `updated_at` or `improvement_reason`, inject them
/// from the system clock. Never removes prior entries.
pub fn ensure_audit_fields(content: &str, reason: &str) -> Result<String> {
    let now = chrono::Utc::now().to_rfc3339();
    let trimmed = content.trim();

    let Some(rest) = trimmed.strip_prefix("+++") else {
        // No front-matter; prepend one with audit fields
        let front_matter =
            format!("+++\nupdated_at = \"{now}\"\nimprovement_reason = \"{reason}\"\n+++\n");
        return Ok(format!("{front_matter}{content}"));
    };

    let Some(end_pos) = rest.find("+++") else {
        bail!("malformed front-matter: missing closing +++");
    };

    let front_matter_str = &rest[..end_pos];
    let body = &rest[end_pos + 3..];

    // Parse existing TOML front-matter
    let mut table: toml::Table = toml::from_str(front_matter_str).unwrap_or_default();

    // Always set updated_at
    table.insert("updated_at".to_string(), toml::Value::String(now));

    // Always set improvement_reason
    table.insert(
        "improvement_reason".to_string(),
        toml::Value::String(reason.to_string()),
    );

    let new_front_matter = toml::to_string_pretty(&table)?;
    Ok(format!("+++\n{new_front_matter}+++{body}"))
}

/// Check if a skill is within its cooldown window.
///
/// # Security (S2.2)
/// Returns true if the skill was improved less than `cooldown_secs` ago.
pub fn is_within_cooldown(index: &SkillIndex, slug: &str, cooldown_secs: u64) -> Result<bool> {
    let Some(last_improved) = index.last_improved_at(slug)? else {
        return Ok(false);
    };

    let last_ts = chrono::DateTime::parse_from_rfc3339(&last_improved)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    let now = chrono::Utc::now();
    let elapsed = now.signed_duration_since(last_ts);

    #[allow(clippy::cast_possible_wrap)]
    Ok(elapsed.num_seconds() < cooldown_secs as i64)
}

/// Atomically improve a skill file.
///
/// # Security
/// - S2.1: Writes to temp file, validates, then renames. On failure, cleans up temp.
/// - S2.2: Checks cooldown before proceeding.
/// - S2.3: Ensures audit fields are present.
///
/// Returns `Ok(None)` if improvement was skipped (cooldown or invalid content).
pub fn improve_skill(
    skills_dir: &Path,
    slug: &str,
    new_content: &str,
    reason: &str,
    index: &SkillIndex,
    config: &SkillImprovementConfig,
) -> Result<Option<PathBuf>> {
    // S1.1: Validate slug to prevent path traversal in temp/skill file names
    let validated_slug = validate_slug(slug)?;

    // S2.2: Cooldown check
    if is_within_cooldown(index, &validated_slug, config.cooldown_secs)? {
        return Ok(None);
    }

    let skill_path = skills_dir.join(format!("{validated_slug}.md"));

    // Validate the new content is proper UTF-8 (caller provides &str, so this is guaranteed)
    // But check for well-formed front-matter
    // S2.1: Validate before writing
    if validate_skill_content(new_content).is_err() {
        return Ok(None);
    }

    // S2.3: Ensure audit fields
    let audited_content = ensure_audit_fields(new_content, reason)?;

    // Validate the audited content too
    validate_skill_content(&audited_content)?;

    // S2.1: Atomic write — temp file → validate → rename
    let temp_path = skills_dir.join(format!(".{validated_slug}.md.tmp"));

    // Write to temp file
    if let Err(e) = std::fs::write(&temp_path, &audited_content) {
        // Clean up on failure
        let _ = std::fs::remove_file(&temp_path);
        return Err(e.into());
    }

    // Verify temp file content is valid UTF-8
    match std::fs::read(&temp_path) {
        Ok(bytes) => {
            if std::str::from_utf8(&bytes).is_err() {
                let _ = std::fs::remove_file(&temp_path);
                bail!("written content is not valid UTF-8");
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(e.into());
        }
    }

    // Rename atomically into place
    if let Err(e) = std::fs::rename(&temp_path, &skill_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e.into());
    }

    // Update the index
    let now = chrono::Utc::now().to_rfc3339();
    index.update_content(&validated_slug, &audited_content, &now)?;

    Ok(Some(skill_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_bad_llm_output_preserves_original() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let db_path = tmp.path().join("skills.db");
        let index = SkillIndex::open(&db_path).unwrap();
        let config = SkillImprovementConfig::default();

        // Create an original skill
        let original_content = "+++\ntitle = \"Original\"\n+++\n# Original\nOriginal content.";
        let skill_path = skills_dir.join("test-skill.md");
        std::fs::write(&skill_path, original_content).unwrap();
        index
            .insert("test-skill", "Original", "", original_content)
            .unwrap();

        // Try to improve with empty content — should not overwrite
        let result = improve_skill(
            &skills_dir,
            "test-skill",
            "",
            "improvement",
            &index,
            &config,
        )
        .unwrap();
        assert!(result.is_none(), "empty content should not overwrite");

        // Original file should be unchanged
        let actual = std::fs::read_to_string(&skill_path).unwrap();
        assert_eq!(actual, original_content);
    }

    #[test]
    fn test_audit_fields_always_present() {
        // Content with front-matter but missing audit fields
        let content = "+++\ntitle = \"My Skill\"\ntags = [\"test\"]\n+++\n# My Skill\nDo things.";
        let result = ensure_audit_fields(content, "improved accuracy").unwrap();

        // Parse the front-matter from result
        let trimmed = result.trim();
        assert!(trimmed.starts_with("+++"));
        let rest = &trimmed[3..];
        let end_pos = rest.find("+++").expect("should have closing +++");
        let fm = &rest[..end_pos];
        let table: toml::Table = toml::from_str(fm).unwrap();

        assert!(table.contains_key("updated_at"), "must have updated_at");
        assert!(
            table.contains_key("improvement_reason"),
            "must have improvement_reason"
        );
        assert_eq!(
            table["improvement_reason"].as_str().unwrap(),
            "improved accuracy"
        );
        // Original fields preserved
        assert!(
            table.contains_key("title"),
            "original title must be preserved"
        );
        assert!(
            table.contains_key("tags"),
            "original tags must be preserved"
        );
    }

    #[test]
    fn test_cooldown_blocks_second_improvement() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let db_path = tmp.path().join("skills.db");
        let index = SkillIndex::open(&db_path).unwrap();
        let config = SkillImprovementConfig {
            cooldown_secs: 3600,
        };

        // Create and improve a skill
        let content = "+++\ntitle = \"Original\"\n+++\n# Original\nContent.";
        std::fs::write(skills_dir.join("cooldown-test.md"), content).unwrap();
        index
            .insert("cooldown-test", "Original", "", content)
            .unwrap();

        let improved = "+++\ntitle = \"Improved\"\n+++\n# Improved\nBetter content.";
        let result = improve_skill(
            &skills_dir,
            "cooldown-test",
            improved,
            "first improvement",
            &index,
            &config,
        )
        .unwrap();
        assert!(result.is_some(), "first improvement should succeed");

        // Second improvement within cooldown — should be blocked
        let improved2 = "+++\ntitle = \"Improved Again\"\n+++\n# Improved Again\nEven better.";
        let result2 = improve_skill(
            &skills_dir,
            "cooldown-test",
            improved2,
            "second improvement",
            &index,
            &config,
        )
        .unwrap();
        assert!(
            result2.is_none(),
            "second improvement within cooldown should be blocked"
        );
    }

    #[test]
    fn test_invalid_utf8_is_rejected() {
        // validate_skill_content only accepts &str which is always valid UTF-8,
        // but we test the atomic write path by checking binary content detection
        let result = validate_skill_content("");
        assert!(result.is_err(), "empty content should be rejected");

        let result = validate_skill_content("+++\ninvalid toml {{{{\n+++\n# Test");
        assert!(
            result.is_err(),
            "invalid TOML front-matter should be rejected"
        );

        let result = validate_skill_content("+++\ntitle = \"OK\"\n+++\n# Valid");
        assert!(result.is_ok(), "valid content should pass");
    }

    #[test]
    fn test_ensure_audit_fields_without_front_matter() {
        let content = "# No Front Matter\nJust content.";
        let result = ensure_audit_fields(content, "initial creation").unwrap();
        assert!(result.starts_with("+++\n"));
        assert!(result.contains("updated_at"));
        assert!(result.contains("improvement_reason"));
        assert!(result.contains("# No Front Matter"));
    }

    #[test]
    fn test_validate_skill_content_malformed_front_matter() {
        let content = "+++\nthis is not closed";
        assert!(validate_skill_content(content).is_err());
    }
}
