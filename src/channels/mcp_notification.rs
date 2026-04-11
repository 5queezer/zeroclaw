//! MCP push notification bridge.
//!
//! Converts incoming `notifications/alert_triggered` JSON-RPC messages from an
//! MCP server into [`ChannelMessage`] objects that feed into the agent loop.
//! Also provides a standalone SSE listener that connects to an MCP server's
//! SSE endpoint, parses notifications, and sends them to the channel message bus.

use super::traits::ChannelMessage;
use serde_json::Value;

/// Convert an MCP alert notification into a ChannelMessage for the agent loop.
pub fn alert_notification_to_channel_message(
    notification: &Value,
    reply_target: &str,
) -> Option<ChannelMessage> {
    let params = notification.get("params")?;
    let alert_id = params.get("alert_id")?.as_str()?;
    let symbol = params.get("symbol")?.as_str()?;
    let triggered_at = params
        .get("triggered_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let value = params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let condition = params
        .get("condition")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let condition_type = condition
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown");

    let condition_desc = match condition_type {
        "price_above" => format!(
            "Price above {}",
            condition
                .get("threshold")
                .and_then(|t| t.as_f64())
                .unwrap_or(0.0)
        ),
        "price_below" => format!(
            "Price below {}",
            condition
                .get("threshold")
                .and_then(|t| t.as_f64())
                .unwrap_or(0.0)
        ),
        "indicator_signal" => {
            let indicator = condition
                .get("indicator")
                .and_then(|i| i.as_str())
                .unwrap_or("unknown");
            let signal = condition
                .get("signal")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            format!("{indicator} {signal}")
        }
        _ => format!("{condition:?}"),
    };

    let content = format!(
        "[handler:alert_response]\n\
         Alert triggered: {alert_id} on {symbol}\n\
         Condition: {condition_desc}\n\
         Value: {value:.6}\n\
         Time: {triggered_at}"
    );

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Some(ChannelMessage {
        id: format!("alert_{alert_id}"),
        sender: "chartgen".to_string(),
        reply_target: reply_target.to_string(),
        content,
        channel: "mcp_notification".to_string(),
        timestamp,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
    })
}

/// Spawn a background task that connects to an MCP server's SSE endpoint,
/// subscribes to push notifications, and forwards `notifications/alert_triggered`
/// messages into the channel message bus.
///
/// Reconnects with exponential backoff (1 s -> 60 s) on disconnect.
#[allow(clippy::implicit_hasher)]
pub fn spawn_notification_listener(
    server_name: String,
    sse_url: String,
    headers: std::collections::HashMap<String, String>,
    reply_target: String,
    symbols: Vec<String>,
    alert_types: Vec<String>,
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff_secs: u64 = 1;
        const MAX_BACKOFF_SECS: u64 = 60;

        loop {
            if tx.is_closed() {
                tracing::info!(
                    "MCP notification listener for `{server_name}` stopping: channel closed"
                );
                break;
            }
            tracing::info!("MCP notification listener connecting to `{server_name}`");
            tracing::debug!("MCP notification listener `{server_name}` endpoint: {sse_url}");
            match run_sse_listener(
                &server_name,
                &sse_url,
                &headers,
                &reply_target,
                &symbols,
                &alert_types,
                &tx,
            )
            .await
            {
                Ok(()) => {
                    tracing::info!(
                        "MCP notification listener for `{server_name}` disconnected cleanly"
                    );
                }
                Err(e) => {
                    tracing::warn!("MCP notification listener for `{server_name}` error: {e:#}");
                }
            }
            if tx.is_closed() {
                tracing::info!(
                    "MCP notification listener for `{server_name}` stopping: channel closed"
                );
                break;
            }
            tracing::info!(
                "MCP notification listener for `{server_name}` reconnecting in {backoff_secs}s"
            );
            tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
        }
    })
}

/// Connect to the SSE endpoint, subscribe, and process events until disconnect.
async fn run_sse_listener(
    server_name: &str,
    sse_url: &str,
    headers: &std::collections::HashMap<String, String>,
    reply_target: &str,
    symbols: &[String],
    alert_types: &[String],
    tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncBufReadExt;

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))?;

    let mut req = client
        .get(sse_url)
        .header("Accept", "text/event-stream")
        .header("Cache-Control", "no-cache");
    for (key, value) in headers {
        req = req.header(key, value);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("SSE connect to `{server_name}` failed: {e}"))?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "SSE connect to `{server_name}` returned HTTP {}",
            resp.status()
        );
    }

    // After connecting, send subscribe_notifications as a POST to the same URL.
    let mut subscribe_params = serde_json::Map::new();
    if !symbols.is_empty() {
        subscribe_params.insert("symbols".to_string(), serde_json::json!(symbols));
    }
    if !alert_types.is_empty() {
        subscribe_params.insert("types".to_string(), serde_json::json!(alert_types));
    }
    let subscribe_req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "subscribe_notifications",
        "params": subscribe_params,
    });

    let mut post_req = client
        .post(sse_url)
        .header("Content-Type", "application/json")
        .json(&subscribe_req);
    for (key, value) in headers {
        post_req = post_req.header(key, value);
    }
    match post_req.send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            tracing::warn!(
                "MCP notification subscribe to `{server_name}` returned HTTP {} (non-fatal)",
                resp.status()
            );
        }
        Err(e) => {
            tracing::warn!(
                "MCP notification subscribe to `{server_name}` failed (non-fatal): {e}"
            );
        }
    }

    tracing::info!("MCP notification listener for `{server_name}` connected, reading events");

    // Read SSE stream
    let stream = resp
        .bytes_stream()
        .map(|item| item.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(stream);
    let mut lines = tokio::io::BufReader::new(reader).lines();

    let mut cur_data: Vec<String> = Vec::new();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                anyhow::bail!("SSE stream read error from `{server_name}`: {e}");
            }
        };
        let line = line.trim_end_matches('\r').to_string();

        if line.is_empty() {
            // End of event — process accumulated data
            if !cur_data.is_empty() {
                let data = cur_data.join("\n");
                cur_data.clear();

                match serde_json::from_str::<Value>(&data) {
                    Ok(msg) => {
                        let method =
                            msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

                        if method == "notifications/alert_triggered" {
                            if let Some(channel_msg) =
                                alert_notification_to_channel_message(&msg, reply_target)
                            {
                                tracing::info!(
                                    "MCP notification from `{server_name}`: alert {}",
                                    channel_msg.id
                                );
                                if tx.send(channel_msg).await.is_err() {
                                    tracing::warn!(
                                        "MCP notification channel for `{server_name}` closed"
                                    );
                                    return Ok(());
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "MCP notification from `{server_name}` had invalid JSON: {e}"
                        );
                    }
                }
            }
            continue;
        }

        if let Some(data_line) = line.strip_prefix("data:") {
            cur_data.push(data_line.trim_start().to_string());
        }
    }

    Ok(())
}

use tokio_stream::StreamExt;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_notification(condition_type: &str) -> Value {
        let condition = match condition_type {
            "price_above" => json!({"type": "price_above", "threshold": 50000.0}),
            "price_below" => json!({"type": "price_below", "threshold": 28000.0}),
            "indicator_signal" => {
                json!({"type": "indicator_signal", "indicator": "RSI", "signal": "oversold"})
            }
            _ => json!({"type": condition_type}),
        };

        json!({
            "jsonrpc": "2.0",
            "method": "notifications/alert_triggered",
            "params": {
                "alert_id": "alert-001",
                "symbol": "BTC/USDT",
                "triggered_at": "2026-01-15T10:30:00Z",
                "value": 49_999.123_456,
                "condition": condition
            }
        })
    }

    #[test]
    fn parse_valid_notification_produces_correct_fields() {
        let notif = sample_notification("price_above");
        let msg = alert_notification_to_channel_message(&notif, "chat_123").unwrap();

        assert_eq!(msg.id, "alert_alert-001");
        assert_eq!(msg.sender, "chartgen");
        assert_eq!(msg.reply_target, "chat_123");
        assert_eq!(msg.channel, "mcp_notification");
        assert!(msg.content.contains("BTC/USDT"));
        assert!(msg.content.contains("alert-001"));
        assert!(msg.content.contains("49999.123456"));
        assert!(msg.content.contains("2026-01-15T10:30:00Z"));
    }

    #[test]
    fn handler_tag_present_in_content() {
        let notif = sample_notification("price_above");
        let msg = alert_notification_to_channel_message(&notif, "").unwrap();

        assert!(msg.content.starts_with("[handler:alert_response]"));
    }

    #[test]
    fn missing_params_returns_none() {
        let notif = json!({"jsonrpc": "2.0", "method": "notifications/alert_triggered"});
        assert!(alert_notification_to_channel_message(&notif, "").is_none());
    }

    #[test]
    fn missing_alert_id_returns_none() {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/alert_triggered",
            "params": {
                "symbol": "BTC/USDT"
            }
        });
        assert!(alert_notification_to_channel_message(&notif, "").is_none());
    }

    #[test]
    fn missing_symbol_returns_none() {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/alert_triggered",
            "params": {
                "alert_id": "a1"
            }
        });
        assert!(alert_notification_to_channel_message(&notif, "").is_none());
    }

    #[test]
    fn price_above_condition_formatted() {
        let notif = sample_notification("price_above");
        let msg = alert_notification_to_channel_message(&notif, "").unwrap();
        assert!(msg.content.contains("Price above 50000"));
    }

    #[test]
    fn price_below_condition_formatted() {
        let notif = sample_notification("price_below");
        let msg = alert_notification_to_channel_message(&notif, "").unwrap();
        assert!(msg.content.contains("Price below 28000"));
    }

    #[test]
    fn indicator_signal_condition_formatted() {
        let notif = sample_notification("indicator_signal");
        let msg = alert_notification_to_channel_message(&notif, "").unwrap();
        assert!(msg.content.contains("RSI oversold"));
    }

    #[test]
    fn unknown_condition_type_uses_debug_format() {
        let notif = sample_notification("custom_type");
        let msg = alert_notification_to_channel_message(&notif, "").unwrap();
        // Should contain the debug representation of the condition JSON
        assert!(msg.content.contains("custom_type"));
    }

    #[test]
    fn reply_target_passed_through() {
        let notif = sample_notification("price_above");
        let msg = alert_notification_to_channel_message(&notif, "telegram:12345").unwrap();
        assert_eq!(msg.reply_target, "telegram:12345");
    }

    #[test]
    fn empty_reply_target_is_valid() {
        let notif = sample_notification("price_above");
        let msg = alert_notification_to_channel_message(&notif, "").unwrap();
        assert_eq!(msg.reply_target, "");
    }

    #[test]
    fn missing_triggered_at_defaults_to_unknown() {
        let notif = json!({
            "params": {
                "alert_id": "a1",
                "symbol": "ETH/USDT",
                "value": 1.0,
                "condition": {"type": "price_above", "threshold": 3000.0}
            }
        });
        let msg = alert_notification_to_channel_message(&notif, "").unwrap();
        assert!(msg.content.contains("Time: unknown"));
    }

    #[test]
    fn missing_value_defaults_to_zero() {
        let notif = json!({
            "params": {
                "alert_id": "a1",
                "symbol": "ETH/USDT",
                "triggered_at": "2026-01-01T00:00:00Z",
                "condition": {"type": "price_above", "threshold": 3000.0}
            }
        });
        let msg = alert_notification_to_channel_message(&notif, "").unwrap();
        assert!(msg.content.contains("Value: 0.000000"));
    }
}
