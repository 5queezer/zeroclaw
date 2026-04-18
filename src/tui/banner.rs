use crate::session::SessionMeta;
use crate::tui::ChatMessage;

/// Build the initial sequence of System messages shown on TUI boot.
///
/// For resumed sessions (`prior_msg_count > 0`), swaps the "Welcome..." line
/// for a "Resumed session ..." line. The prior conversation is replayed BELOW
/// these banner entries by the caller.
#[must_use]
pub fn build_banner_messages(meta: &SessionMeta, prior_msg_count: usize) -> Vec<ChatMessage> {
    let version = env!("CARGO_PKG_VERSION");
    let model = meta.model.as_deref().unwrap_or("?");
    let provider = meta.provider.as_deref().unwrap_or("?");
    let cwd = meta.cwd.display();

    let logo = [
        "  \u{2503}\u{250F}\u{2513}  \u{250F}\u{2501}\u{2513}\u{250F}\u{2501}\u{2579}\u{250F}\u{2513}\u{257B}",
        "  \u{2503}\u{2523}\u{252B}  \u{2523}\u{252B}\u{2570}\u{2523}\u{2501}\u{2515}\u{2503}\u{2517}\u{252B}",
        "  \u{2579}\u{2579}\u{2579}  \u{2579}\u{2515}\u{2579}\u{2579}  \u{2579} \u{2579}",
    ];

    let mut out = Vec::new();
    out.push(ChatMessage::System {
        text: format!("{}        Hrafn v{version}", logo[0]),
    });
    out.push(ChatMessage::System {
        text: format!("{}        {provider}/{model}  \u{b7}  {cwd}", logo[1]),
    });
    out.push(ChatMessage::System {
        text: logo[2].to_string(),
    });
    out.push(ChatMessage::System {
        text: String::new(),
    });

    if prior_msg_count > 0 {
        out.push(ChatMessage::System {
            text: format!(
                "  Resumed session {} \u{b7} {prior_msg_count} messages",
                meta.id.as_str()
            ),
        });
    } else {
        out.push(ChatMessage::System {
            text: "  Welcome. Type a message, or /help for commands.".to_string(),
        });
        let session_line = if let Some(t) = &meta.title {
            format!("  Session: {}  \u{b7}  {t}", meta.id.as_str())
        } else {
            format!("  Session: {}", meta.id.as_str())
        };
        out.push(ChatMessage::System { text: session_line });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{MessageCounts, SessionId};
    use std::path::PathBuf;
    use std::time::Duration;

    fn meta(title: Option<&str>) -> SessionMeta {
        SessionMeta {
            id: SessionId::parse("20260417_205355_53b1e8").unwrap(),
            title: title.map(str::to_string),
            title_explicit: title.is_some(),
            cwd: PathBuf::from("/tmp"),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            duration: Duration::ZERO,
            provider: Some("anthropic".into()),
            model: Some("opus".into()),
            counts: MessageCounts::default(),
        }
    }

    fn joined(msgs: &[ChatMessage]) -> String {
        msgs.iter()
            .map(|m| match m {
                ChatMessage::System { text } => text.as_str(),
                _ => "",
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn fresh_session_banner_has_welcome() {
        let msgs = build_banner_messages(&meta(Some("t")), 0);
        let s = joined(&msgs);
        assert!(s.contains("Welcome"));
        assert!(s.contains("Session: 20260417_205355_53b1e8"));
        assert!(s.contains('t'));
    }

    #[test]
    fn resumed_banner_says_resumed() {
        let msgs = build_banner_messages(&meta(None), 12);
        let s = joined(&msgs);
        assert!(s.contains("Resumed session"));
        assert!(s.contains("12 messages"));
        assert!(!s.contains("Welcome"));
    }

    #[test]
    fn banner_includes_provider_model_cwd_and_version() {
        let msgs = build_banner_messages(&meta(None), 0);
        let s = joined(&msgs);
        assert!(s.contains("anthropic/opus"));
        assert!(s.contains("/tmp"));
        assert!(s.contains(env!("CARGO_PKG_VERSION")));
    }
}
