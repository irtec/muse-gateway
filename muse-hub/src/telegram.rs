use reqwest::Client;
use std::time::Duration;
use tracing::{error, info, warn};

#[derive(Clone)]
pub struct TelegramNotifier {
    client: Client,
    bot_token: String,
    chat_id: String,
    enabled: bool,
}

impl TelegramNotifier {
    pub fn new(bot_token: String, chat_id: String, enabled: bool) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        Self {
            client,
            bot_token,
            chat_id,
            enabled,
        }
    }

    pub fn disabled() -> Self {
        Self {
            client: Client::default(),
            bot_token: String::new(),
            chat_id: String::new(),
            enabled: false,
        }
    }

    /// Sends a formatted Markdown message to Telegram channel/group
    pub async fn send_alert(&self, text: &str) {
        if !self.enabled || self.bot_token.is_empty() || self.chat_id.is_empty() {
            info!("[Telegram Alert Skipped]: {}", text);
            return;
        }

        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);

        let payload = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "Markdown"
        });

        match self.client.post(&url).json(&payload).send().await {
            Ok(res) => {
                if res.status().is_success() {
                    info!("Telegram alert delivered successfully: {}", text);
                } else {
                    let err_text = res.text().await.unwrap_or_default();
                    warn!("Telegram API error response: {}", err_text);
                }
            }
            Err(e) => {
                error!("Failed to post message to Telegram API: {}", e);
            }
        }
    }
}
