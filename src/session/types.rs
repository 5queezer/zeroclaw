use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::SessionId;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MessageCounts {
    pub total: u32,
    pub user: u32,
    pub assistant: u32,
    pub tool_call: u32,
    pub tool_result: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: SessionId,
    pub title: Option<String>,
    pub title_explicit: bool,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(with = "duration_ms")]
    pub duration: Duration,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub counts: MessageCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub seq: u32,
    pub ts: DateTime<Utc>,
    pub body: crate::tui::ChatMessage,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub meta: SessionMeta,
    pub messages: Vec<StoredMessage>,
}

/// Serde adaptor — store Duration as milliseconds.
mod duration_ms {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms: u64 = Deserialize::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

impl SessionId {
    #[must_use]
    pub fn short(&self) -> &str {
        &self.as_str()[..10.min(self.as_str().len())]
    }
}

impl Serialize for SessionId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        SessionId::parse(&s).map_err(serde::de::Error::custom)
    }
}
