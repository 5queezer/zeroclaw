use std::time::Instant;

use crate::agent::TurnEvent;
use crate::tui::{ActiveTool, App, ChatMessage, ToolStatus};

/// Map an agent turn event to App state updates.
pub(crate) fn handle_turn_event(app: &mut App, event: TurnEvent) {
    match event {
        TurnEvent::Chunk { delta } => app.pending_chunk.push_str(&delta),
        TurnEvent::Thinking { .. } => {}
        TurnEvent::ToolCall { name, args } => {
            let args_str = serde_json::to_string_pretty(&args).unwrap_or_default();
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
            app.messages.push(msg);
            if app.auto_scroll {
                app.scroll_offset = u16::MAX;
            }
        }
        TurnEvent::ToolResult { name, output } => {
            update_tool_status(&mut app.messages, &name);
            app.active_tools.retain(|t| t.name != name);
            let msg = ChatMessage::ToolResult { name, output };
            app.messages.push(msg);
            if app.auto_scroll {
                app.scroll_offset = u16::MAX;
            }
        }
        TurnEvent::TurnEnd => {
            if !app.pending_chunk.is_empty() {
                let text = std::mem::take(&mut app.pending_chunk);
                app.push_assistant(text);
            }
            app.spinner = None;
        }
    }
}

fn update_tool_status(messages: &mut [ChatMessage], matching_name: &str) {
    for m in messages.iter_mut().rev() {
        if let ChatMessage::ToolCall { name, status, .. } = m {
            if name == matching_name {
                if let ToolStatus::Running(started) = status {
                    *status = ToolStatus::Done(started.elapsed());
                }
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::App;

    #[test]
    fn chunk_accumulates_into_pending() {
        let mut app = App::new();
        handle_turn_event(
            &mut app,
            TurnEvent::Chunk {
                delta: "hel".into(),
            },
        );
        handle_turn_event(&mut app, TurnEvent::Chunk { delta: "lo".into() });
        assert_eq!(app.pending_chunk, "hello");
    }

    #[test]
    fn turn_end_flushes_assistant_message_and_clears_pending() {
        let mut app = App::new();
        handle_turn_event(&mut app, TurnEvent::Chunk { delta: "hi".into() });
        handle_turn_event(&mut app, TurnEvent::TurnEnd);
        assert!(app.pending_chunk.is_empty());
        assert!(
            matches!(app.messages.last(), Some(ChatMessage::Assistant { text }) if text == "hi")
        );
    }

    #[test]
    fn tool_call_then_result_appends_both() {
        let mut app = App::new();
        handle_turn_event(
            &mut app,
            TurnEvent::ToolCall {
                name: "echo".into(),
                args: serde_json::json!({"msg": "hi"}),
            },
        );
        handle_turn_event(
            &mut app,
            TurnEvent::ToolResult {
                name: "echo".into(),
                output: "hi".into(),
            },
        );
        assert_eq!(app.messages.len(), 2);
        assert!(matches!(&app.messages[0], ChatMessage::ToolCall { name, .. } if name == "echo"));
        assert!(matches!(&app.messages[1], ChatMessage::ToolResult { name, .. } if name == "echo"));
    }

    #[test]
    fn tool_result_updates_matching_call_status_to_done() {
        let mut app = App::new();
        handle_turn_event(
            &mut app,
            TurnEvent::ToolCall {
                name: "shell".into(),
                args: serde_json::Value::Null,
            },
        );
        handle_turn_event(
            &mut app,
            TurnEvent::ToolResult {
                name: "shell".into(),
                output: "ok".into(),
            },
        );
        let matching = app
            .messages
            .iter()
            .find(|m| matches!(m, ChatMessage::ToolCall { name, .. } if name == "shell"));
        assert!(matches!(
            matching,
            Some(ChatMessage::ToolCall {
                status: ToolStatus::Done(_),
                ..
            })
        ));
    }
}
