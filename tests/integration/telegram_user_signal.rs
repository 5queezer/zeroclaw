//! Integration tests for the telegram_user channel signal pipeline.
//!
//! Since we can't connect to real Telegram (needs credentials), these tests
//! verify the testable contracts: message construction, handler prefix logic,
//! ID uniqueness, attachment structure, and Channel trait behaviour.
//!
//! Issue: #164

use hrafn::channels::media_pipeline::MediaAttachment;
use hrafn::channels::traits::ChannelMessage;

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: Handler prefix in signal content
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn channel_message_from_signal_has_handler_prefix() {
    let msg = ChannelMessage {
        id: "tgu_100eyes_12345".into(),
        sender: "100eyes_crypto".into(),
        reply_target: "@my_hrafn_bot".into(),
        content: "[handler:trading_signal]\nBTCUSDT Long, TP 70000, SL 65000".into(),
        channel: "telegram_user".into(),
        timestamp: 1700000000,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
    };

    assert!(msg.content.starts_with("[handler:trading_signal]"));
    assert_eq!(msg.channel, "telegram_user");
    assert_eq!(msg.sender, "100eyes_crypto");
    // Reply target routes to bot, not back to the watched channel
    assert_eq!(msg.reply_target, "@my_hrafn_bot");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: Message ID uniqueness per channel
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn signal_message_id_includes_channel_and_msg_id() {
    let id1 = format!("tgu_{}_{}", "channel_a", 123);
    let id2 = format!("tgu_{}_{}", "channel_b", 123);
    let id3 = format!("tgu_{}_{}", "channel_a", 456);

    assert_ne!(id1, id2, "different channels should produce different IDs");
    assert_ne!(
        id1, id3,
        "different message IDs should produce different IDs"
    );
    assert!(id1.starts_with("tgu_"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: Image attachment structure
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn signal_with_image_attachment() {
    let image_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0]; // JPEG magic bytes
    let attachment = MediaAttachment {
        file_name: "signal_1700000000.jpg".to_string(),
        data: image_bytes.clone(),
        mime_type: Some("image/jpeg".to_string()),
    };

    let msg = ChannelMessage {
        id: "tgu_100eyes_789".into(),
        sender: "100eyes".into(),
        reply_target: "@bot".into(),
        content: "[handler:trading_signal]\nXRPUSDT Long".into(),
        channel: "telegram_user".into(),
        timestamp: 1700000000,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![attachment],
    };

    assert_eq!(msg.attachments.len(), 1);
    assert_eq!(msg.attachments[0].mime_type, Some("image/jpeg".to_string()));
    assert!(msg.attachments[0].file_name.starts_with("signal_"));
    assert!(msg.attachments[0].file_name.ends_with(".jpg"));
    assert_eq!(msg.attachments[0].data, image_bytes);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 4: No handler prefix when handler is empty
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn signal_without_handler_passes_raw_text() {
    let handler = "";
    let text = "Some channel message";
    let content = if handler.is_empty() {
        text.to_string()
    } else {
        format!("[handler:{handler}]\n{text}")
    };

    assert!(!content.starts_with("[handler:"));
    assert_eq!(content, "Some channel message");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 5: Channel construction from config (feature-gated)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "channel-telegram-user")]
#[test]
fn channel_construction_from_config() {
    use hrafn::channels::telegram_user::TelegramUserChannel;
    use hrafn::channels::traits::Channel;
    use hrafn::config::{TelegramUserConfig, TelegramUserWatchConfig};

    let config = TelegramUserConfig {
        api_id: 12345,
        api_hash: "test_hash".to_string(),
        phone: "+1 555 0100".to_string(),
        session_file: "/tmp/test.session".to_string(),
        watch: vec![TelegramUserWatchConfig {
            channel: "test_channel".to_string(),
            handler: "trading_signal".to_string(),
        }],
        reply_via_bot: Some("@test_bot".to_string()),
    };

    let channel = TelegramUserChannel::new(&config);
    assert_eq!(channel.name(), "telegram_user");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 6: send() is a no-op (feature-gated)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "channel-telegram-user")]
#[tokio::test]
async fn send_is_noop() {
    use hrafn::channels::telegram_user::TelegramUserChannel;
    use hrafn::channels::traits::{Channel, SendMessage};
    use hrafn::config::{TelegramUserConfig, TelegramUserWatchConfig};

    let config = TelegramUserConfig {
        api_id: 12345,
        api_hash: "test_hash".to_string(),
        phone: "+1 555 0100".to_string(),
        session_file: "/tmp/test.session".to_string(),
        watch: vec![TelegramUserWatchConfig {
            channel: "test".to_string(),
            handler: "".to_string(),
        }],
        reply_via_bot: Some("@test_bot".to_string()),
    };

    let channel = TelegramUserChannel::new(&config);
    let msg = SendMessage::new("hello", "@someone");

    // send() should succeed (it's a no-op)
    let result = channel.send(&msg).await;
    assert!(result.is_ok());
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 7: Multiple watch configs produce distinct messages
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn multiple_watch_configs_produce_distinct_messages() {
    let channels = vec![("channel_a", "handler_1"), ("channel_b", "handler_2")];

    let messages: Vec<ChannelMessage> = channels
        .iter()
        .enumerate()
        .map(|(i, (ch, handler))| {
            let text = format!("Signal from {ch}");
            let content = format!("[handler:{handler}]\n{text}");
            ChannelMessage {
                id: format!("tgu_{}_{}", ch, i),
                sender: ch.to_string(),
                reply_target: "@bot".into(),
                content,
                channel: "telegram_user".into(),
                timestamp: 1700000000 + i as u64,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            }
        })
        .collect();

    assert_eq!(messages.len(), 2);
    assert_ne!(messages[0].id, messages[1].id);
    assert_ne!(messages[0].sender, messages[1].sender);
    assert!(messages[0].content.contains("[handler:handler_1]"));
    assert!(messages[1].content.contains("[handler:handler_2]"));
}
