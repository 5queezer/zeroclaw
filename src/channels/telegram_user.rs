//! Telegram user client channel (grammers-based).
//!
//! Watches third-party Telegram channels as a regular user account and
//! forwards incoming signal messages (with image attachments) into the
//! agent loop via [`ChannelMessage`]. Responses are routed through the
//! regular Telegram bot channel by setting `reply_target`.
//!
//! This is a **passive listener** only -- `send()` is a no-op.

use super::media_pipeline::MediaAttachment;
use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use grammers_client::Client;
use grammers_client::client::UpdatesConfiguration;
use grammers_client::sender::SenderPool;
use grammers_client::session::storages::SqliteSession;
use grammers_client::session::types::PeerId;
use grammers_client::update::Update;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// A watched channel with its handler template name.
#[derive(Debug, Clone)]
pub struct WatchedChannel {
    pub channel_username: String,
    pub handler: String,
}

/// Telegram user client that watches third-party channels via grammers.
pub struct TelegramUserChannel {
    api_id: i32,
    api_hash: String,
    phone: String,
    session_file: String,
    watched_channels: Vec<WatchedChannel>,
    reply_via_bot: String,
}

impl TelegramUserChannel {
    /// Create a new `TelegramUserChannel` from config.
    pub fn new(config: &crate::config::TelegramUserConfig) -> Self {
        let watched_channels = config
            .watch
            .iter()
            .map(|w| WatchedChannel {
                channel_username: w.channel.clone(),
                handler: w.handler.clone(),
            })
            .collect();

        Self {
            api_id: config.api_id,
            api_hash: config.api_hash.clone(),
            phone: config.phone.clone(),
            session_file: expand_path(&config.session_file),
            watched_channels,
            reply_via_bot: config.reply_via_bot.clone().unwrap_or_default(),
        }
    }

    /// Download photo bytes from a grammers message into memory.
    async fn download_photo(
        client: &Client,
        message: &grammers_client::message::Message,
    ) -> Option<Vec<u8>> {
        let photo = message.photo()?;
        let mut buf = Vec::new();
        let mut download = client.iter_download(&photo);
        while let Some(chunk) = download.next().await.ok()? {
            buf.extend(chunk);
        }
        if buf.is_empty() { None } else { Some(buf) }
    }
}

/// Expand `~` in paths to the user's home directory.
fn expand_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_home() {
            return format!("{}/{}", home, rest);
        }
    }
    path.to_string()
}

fn dirs_home() -> Option<String> {
    directories::UserDirs::new().map(|d| d.home_dir().to_string_lossy().into_owned())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[async_trait]
impl Channel for TelegramUserChannel {
    fn name(&self) -> &str {
        "telegram_user"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        // No-op: responses route through the regular Telegram bot channel.
        tracing::trace!("[telegram_user] send() is no-op; responses route via bot channel");
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        if self.watched_channels.is_empty() {
            tracing::warn!("[telegram_user] No channels configured to watch; exiting listener");
            return Err(anyhow::anyhow!(
                "No channels configured to watch in telegram_user"
            ));
        }

        // Ensure the session directory exists (first-run on a clean machine).
        if let Some(parent) = std::path::Path::new(&self.session_file).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create session directory: {e}"))?;
        }

        // Open or create session
        let session = Arc::new(
            SqliteSession::open(&self.session_file)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to open session file: {e}"))?,
        );

        let SenderPool {
            runner,
            updates,
            handle,
        } = SenderPool::new(Arc::clone(&session), self.api_id);
        let client = Client::new(handle.clone());

        // Spawn the sender pool runner (drives I/O)
        let pool_task = tokio::spawn(runner.run());

        if !client
            .is_authorized()
            .await
            .map_err(|e| anyhow::anyhow!("Authorization check failed: {e}"))?
        {
            handle.quit();
            let _ = pool_task.await;
            tracing::error!(
                "[telegram_user] Not authorized. Run the interactive login flow first \
                 to create a session file at: {}",
                self.session_file
            );
            return Err(anyhow::anyhow!(
                "Telegram user client not authorized. Session file: {}",
                self.session_file
            ));
        }

        // Resolve watched channel entities and build a lookup map.
        let mut watched_ids: HashMap<PeerId, (String, String)> = HashMap::new();

        for wc in &self.watched_channels {
            match client.resolve_username(&wc.channel_username).await {
                Ok(Some(peer)) => {
                    let peer_id = peer.id();
                    watched_ids.insert(peer_id, (wc.channel_username.clone(), wc.handler.clone()));
                    tracing::info!(
                        "[telegram_user] Watching channel @{} (id={:?})",
                        wc.channel_username,
                        peer_id
                    );
                }
                Ok(None) => {
                    tracing::error!(
                        "[telegram_user] Channel @{} not found; skipping",
                        wc.channel_username
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "[telegram_user] Failed to resolve @{}: {e}; skipping",
                        wc.channel_username
                    );
                }
            }
        }

        if watched_ids.is_empty() {
            handle.quit();
            let _ = pool_task.await;
            tracing::warn!(
                "[telegram_user] No watched channels could be resolved; exiting listener"
            );
            return Err(anyhow::anyhow!(
                "No watched channels could be resolved in telegram_user"
            ));
        }

        tracing::info!(
            "[telegram_user] Listening for messages from {} channel(s)",
            watched_ids.len()
        );

        // Stream updates
        let mut update_stream = client
            .stream_updates(updates, UpdatesConfiguration::default())
            .await;

        loop {
            let update = match update_stream.next().await {
                Ok(update) => update,
                Err(e) => {
                    tracing::warn!("[telegram_user] Update stream error: {e}");
                    // Clean up and return error for framework-managed reconnection
                    // with exponential backoff.
                    update_stream.sync_update_state().await;
                    handle.quit();
                    let _ = pool_task.await;
                    return Err(anyhow::anyhow!("Update stream error: {e}"));
                }
            };

            let message = match update {
                Update::NewMessage(msg) if !msg.outgoing() => msg,
                _ => continue,
            };

            // Check if this message is from a watched channel
            let peer_id = message.peer_id();
            let (channel_username, handler) = match watched_ids.get(&peer_id) {
                Some(entry) => entry.clone(),
                None => continue,
            };

            let text = message.text().to_string();

            // Download photo attachment if present
            let mut attachments = Vec::new();
            if message.photo().is_some() {
                match Self::download_photo(&client, &message).await {
                    Some(bytes) => {
                        attachments.push(MediaAttachment {
                            file_name: format!("signal_{}.jpg", unix_timestamp()),
                            data: bytes,
                            mime_type: Some("image/jpeg".to_string()),
                        });
                    }
                    None => {
                        tracing::debug!(
                            "[telegram_user] Photo present but download failed for message in @{}",
                            channel_username
                        );
                    }
                }
            }

            // Build the ChannelMessage with handler metadata in the content
            let content = if handler.is_empty() {
                text
            } else {
                format!("[handler:{handler}]\n{text}")
            };

            let channel_msg = ChannelMessage {
                id: format!("tgu_{}_{}", channel_username, message.id()),
                sender: channel_username.clone(),
                reply_target: self.reply_via_bot.clone(),
                content,
                channel: "telegram_user".to_string(),
                timestamp: unix_timestamp(),
                thread_ts: None,
                interruption_scope_id: None,
                attachments,
            };

            tracing::debug!(
                "[telegram_user] Forwarding message from @{} (id={}, has_photo={}, handler={})",
                channel_username,
                message.id(),
                !channel_msg.attachments.is_empty(),
                handler
            );

            if let Err(e) = tx.send(channel_msg).await {
                tracing::error!("[telegram_user] Channel send failed: {e}; stopping listener");
                break;
            }
        }

        // Save update state and clean up
        update_stream.sync_update_state().await;
        handle.quit();
        let _ = pool_task.await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_path_tilde() {
        let expanded = expand_path("~/.hrafn/test.session");
        assert!(!expanded.starts_with('~'), "tilde should be expanded");
        assert!(expanded.ends_with(".hrafn/test.session"));
    }

    #[test]
    fn expand_path_absolute() {
        let p = "/tmp/test.session";
        assert_eq!(expand_path(p), p);
    }

    #[test]
    fn channel_construction() {
        let config = crate::config::TelegramUserConfig {
            api_id: 12345,
            api_hash: "abc123".to_string(),
            phone: "+1 555 0100".to_string(),
            session_file: "/tmp/test.session".to_string(),
            watch: vec![crate::config::TelegramUserWatchConfig {
                channel: "test_channel".to_string(),
                handler: "trading_signal".to_string(),
            }],
            reply_via_bot: Some("@test_bot".to_string()),
        };

        let channel = TelegramUserChannel::new(&config);
        assert_eq!(channel.name(), "telegram_user");
        assert_eq!(channel.api_id, 12345);
        assert_eq!(channel.api_hash, "abc123");
        assert_eq!(channel.phone, "+1 555 0100");
        assert_eq!(channel.session_file, "/tmp/test.session");
        assert_eq!(channel.watched_channels.len(), 1);
        assert_eq!(channel.watched_channels[0].channel_username, "test_channel");
        assert_eq!(channel.watched_channels[0].handler, "trading_signal");
        assert_eq!(channel.reply_via_bot, "@test_bot");
    }

    #[test]
    fn channel_message_fields() {
        let msg = ChannelMessage {
            id: "tgu_42".to_string(),
            sender: "100eyes_crypto".to_string(),
            reply_target: "@my_bot".to_string(),
            content: "[handler:trading_signal]\nBTC long signal".to_string(),
            channel: "telegram_user".to_string(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![MediaAttachment {
                file_name: "signal_1700000000.jpg".to_string(),
                data: vec![0xFF, 0xD8, 0xFF],
                mime_type: Some("image/jpeg".to_string()),
            }],
        };

        assert_eq!(msg.channel, "telegram_user");
        assert_eq!(msg.sender, "100eyes_crypto");
        assert_eq!(msg.reply_target, "@my_bot");
        assert!(msg.content.contains("[handler:trading_signal]"));
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].mime_type.as_deref(), Some("image/jpeg"));
    }

    #[tokio::test]
    async fn send_is_noop() {
        let config = crate::config::TelegramUserConfig {
            api_id: 1,
            api_hash: "hash".to_string(),
            phone: "+1 555 0100".to_string(),
            session_file: "/tmp/noop.session".to_string(),
            watch: vec![],
            reply_via_bot: None,
        };
        let channel = TelegramUserChannel::new(&config);
        let msg = SendMessage::new("test", "recipient");
        assert!(channel.send(&msg).await.is_ok());
    }
}
