use async_trait::async_trait;
use serde_json::{json, Value};

use crate::core::domain::{BotState, CycleReport, DomainError, TelegramConfig};
use crate::core::ports::NotifierPort;

/// Supported interactive Telegram Bot commands for monitoring, AI Q&A, and emergency control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramCommand {
    Status,
    Positions,
    Logs,
    Config,
    ActualSkill,
    Query(String),
    Approve,
    Reject,
    Panic,
    Help,
}

/// Parsed incoming message update from Telegram `getUpdates` API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramUpdate {
    pub update_id: i64,
    pub chat_id: String,
    pub text: String,
}

/// Parses a chat message string into a [`TelegramCommand`], supporting optional `@botname` suffixes
/// and free-text arguments for `/query <text>`.
///
/// # Parameters
/// - `text`: Raw incoming message text from Telegram.
///
/// # Returns
/// `Some(TelegramCommand)` if the message starts with a recognized command, or `None` otherwise.
pub fn parse_telegram_command(text: &str) -> Option<TelegramCommand> {
    let trimmed = text.trim();
    let first_token = trimmed.split_whitespace().next()?.trim();
    if !first_token.starts_with('/') {
        return None;
    }
    let cmd_base = first_token
        .split('@')
        .next()
        .unwrap_or(first_token)
        .to_lowercase();
    let rest_arg = trimmed[first_token.len()..].trim().to_string();

    match cmd_base.as_str() {
        "/status" => Some(TelegramCommand::Status),
        "/positions" | "/pos" => Some(TelegramCommand::Positions),
        "/logs" | "/log" => Some(TelegramCommand::Logs),
        "/config" => Some(TelegramCommand::Config),
        "/actual-skill" | "/actual_skill" | "/skills" => Some(TelegramCommand::ActualSkill),
        "/query" | "/ask" | "/ai" => Some(TelegramCommand::Query(rest_arg)),
        "/approve" | "/buy" => Some(TelegramCommand::Approve),
        "/reject" | "/cancel_order" => Some(TelegramCommand::Reject),
        "/panic" | "/panic_close" => Some(TelegramCommand::Panic),
        "/help" | "/start" => Some(TelegramCommand::Help),
        _ => None,
    }
}

/// Validates whether an incoming Telegram `chat_id` matches the configured authorized `allowed_chat_id`.
pub fn is_authorized_chat(incoming_chat_id: &str, allowed_chat_id: &str) -> bool {
    let inc = incoming_chat_id.trim();
    let allow = allowed_chat_id.trim();
    !inc.is_empty() && !allow.is_empty() && inc == allow
}

/// Parses a Telegram `getUpdates` JSON response into a list of [`TelegramUpdate`] items.
pub fn parse_telegram_updates_json(raw: &Value) -> Vec<TelegramUpdate> {
    let Some(results) = raw["result"].as_array() else {
        return Vec::new();
    };

    let mut updates = Vec::with_capacity(results.len());
    for item in results {
        let Some(update_id) = item["update_id"].as_i64() else {
            continue;
        };
        let msg = &item["message"];
        let chat_id = msg["chat"]["id"]
            .as_i64()
            .map(|id| id.to_string())
            .or_else(|| msg["chat"]["id"].as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let text = msg["text"].as_str().unwrap_or_default().trim().to_string();

        if !chat_id.is_empty() && !text.is_empty() {
            updates.push(TelegramUpdate {
                update_id,
                chat_id,
                text,
            });
        }
    }

    updates
}

/// Formats a Telegram notification message for a completed agent evaluation cycle.
///
/// Returns `Some(message)` if:
/// - `notify_on_trade` is `true` and a position was opened or closed in this cycle, OR
/// - `notify_on_cycle` is `true`.
pub fn format_telegram_cycle_notification(
    report: &CycleReport,
    state: &BotState,
    notify_on_trade: bool,
    notify_on_cycle: bool,
) -> Option<String> {
    let has_trade_event = report.executed_order.is_some()
        || report.pending_order.is_some()
        || report.closed_positions > 0;
    if !((notify_on_trade && has_trade_event) || notify_on_cycle) {
        return None;
    }

    let order_line = if let Some(fill) = &report.executed_order {
        format!(
            "Order: EXECUTED {} {:?} qty={:.3} @ {:.2} (SL={:?}, TP={:?})",
            fill.order_id,
            fill.position_side,
            fill.executed_qty,
            fill.avg_price,
            fill.stop_loss,
            fill.take_profit
        )
    } else if let Some(prop) = &report.pending_order {
        format!(
            "Order: MENUNGGU KONFIRMASI ({:?} {} qty={:.3} / ~{:.2} USDT @ {:.2} | SL={:?}, TP={:?})\n\
             Probabilitas Win: {:.1}% (Break-Even: {:.1}% | EV: {:+.2}R | Half-Kelly: {:.1}%)\n\
             👉 Ketik /approve (atau /buy) untuk EKSEKUSI BELI, atau /reject untuk BATAL.",
            prop.request.position_side,
            prop.request.symbol,
            prop.request.quantity,
            prop.notional_usdt,
            prop.request.entry_price,
            prop.request.stop_loss,
            prop.request.take_profit,
            prop.probability.win_probability * 100.0,
            prop.probability.break_even_win_rate * 100.0,
            prop.probability.expected_value_r,
            prop.probability.half_kelly_fraction * 100.0
        )
    } else {
        "Order: None".to_string()
    };

    Some(format!(
        "[Trade-Bot Alert] {} ({})\n\
         Decision : {:?} (conf {:.2})\n\
         Signal   : {}\n\
         Closed   : {} pos\n\
         {}\n\
         Balance  : {:.2} USDT | Realized PnL: {:+.2} USDT",
        report.symbol,
        state.mode,
        report.decision.action,
        report.decision.confidence,
        report.technical.reason,
        report.closed_positions,
        order_line,
        state.current_balance_usdt,
        state.realized_pnl_usdt
    ))
}

/// Returns the help message listing available interactive Telegram commands.
pub fn format_telegram_help() -> String {
    "=== Trade-Bot Telegram Commands ===\n\
     /status       - Cek status daemon, saldo USDT, PnL, dan sinyal terakhir\n\
     /positions    - Lihat daftar posisi Futures terbuka & riwayat order fill\n\
     /actual-skill - Lihat hasil pembelajaran AI & aturan adaptif (skills.md)\n\
     /query <teks> - Tanya langsung ke AI seputar data trading bot, log, & skills\n\
     /approve      - Setujui & eksekusi order yang sedang ditahan (mode confirm_by_chat)\n\
     /reject       - Batalkan order yang sedang ditahan\n\
     /logs         - Lihat 10 baris log terakhir dari daemon\n\
     /config       - Lihat ringkasan konfigurasi aktif\n\
     /panic        - DARURAT: Hentikan daemon & tutup semua posisi terbuka\n\
     /help         - Tampilkan panduan perintah ini"
        .to_string()
}

/// HTTP adapter for Telegram Bot API (`sendMessage` & `getUpdates`).
#[derive(Clone)]
pub struct TelegramBotAdapter {
    http: reqwest::Client,
    cfg: TelegramConfig,
    bot_token: String,
    chat_id: String,
}

impl TelegramBotAdapter {
    /// Creates a new [`TelegramBotAdapter`] from configuration and resolved credentials.
    pub fn new(cfg: TelegramConfig, bot_token: String, chat_id: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            http,
            cfg,
            bot_token: bot_token.trim().to_string(),
            chat_id: chat_id.trim().to_string(),
        }
    }

    /// Creates a [`TelegramBotAdapter`] reading token and chat ID from environment variables.
    pub fn from_env(cfg: &TelegramConfig) -> Self {
        let token = std::env::var(&cfg.bot_token_env).unwrap_or_default();
        let chat_id = std::env::var(&cfg.chat_id_env).unwrap_or_default();
        Self::new(cfg.clone(), token, chat_id)
    }

    /// Returns `true` if Telegram integration is enabled and credentials are provided.
    pub fn is_ready(&self) -> bool {
        self.cfg.enabled && !self.bot_token.is_empty() && !self.chat_id.is_empty()
    }

    /// Returns the configured authorized chat ID.
    pub fn authorized_chat_id(&self) -> &str {
        &self.chat_id
    }

    fn endpoint(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{}",
            self.cfg.api_base_url.trim_end_matches('/'),
            self.bot_token,
            method
        )
    }

    /// Fetches recent incoming updates from Telegram `getUpdates` with the given offset.
    pub async fn fetch_updates(&self, offset: i64) -> Result<Vec<TelegramUpdate>, DomainError> {
        if !self.is_ready() || !self.cfg.listen_commands {
            return Ok(Vec::new());
        }
        let url = format!("{}?offset={}&timeout=1", self.endpoint("getUpdates"), offset);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DomainError::NotificationError {
                message: format!("Telegram getUpdates error: {}", e),
            })?;

        if !resp.status().is_success() {
            return Err(DomainError::NotificationError {
                message: format!("Telegram getUpdates HTTP {}", resp.status()),
            });
        }

        let body: Value = resp
            .json()
            .await
            .map_err(|e| DomainError::NotificationError {
                message: format!("Telegram getUpdates invalid JSON: {}", e),
            })?;

        Ok(parse_telegram_updates_json(&body))
    }
}

#[async_trait]
impl NotifierPort for TelegramBotAdapter {
    async fn send_notification(&self, message: &str) -> Result<(), DomainError> {
        if !self.cfg.enabled {
            return Ok(());
        }
        if self.bot_token.is_empty() || self.chat_id.is_empty() {
            return Err(DomainError::NotificationError {
                message: format!(
                    "Telegram credentials missing: set {} and {} in .env",
                    self.cfg.bot_token_env, self.cfg.chat_id_env
                ),
            });
        }

        let url = self.endpoint("sendMessage");
        let payload = json!({
            "chat_id": self.chat_id,
            "text": message
        });

        let resp = self
            .http
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| DomainError::NotificationError {
                message: format!("Telegram sendMessage network error: {}", e),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            return Err(DomainError::NotificationError {
                message: format!("Telegram sendMessage HTTP {}: {}", status, err_text),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::{
        AgentDecision, BotConfig, OrderFill, OrderSide, PositionSide, SignalAction, TechnicalSignal,
    };

    #[test]
    fn should_parse_telegram_commands_with_and_without_bot_mention() {
        assert_eq!(
            parse_telegram_command("/status"),
            Some(TelegramCommand::Status)
        );
        assert_eq!(
            parse_telegram_command("/status@MyTradeBot arg1"),
            Some(TelegramCommand::Status)
        );
        assert_eq!(
            parse_telegram_command("/positions"),
            Some(TelegramCommand::Positions)
        );
        assert_eq!(
            parse_telegram_command("/pos"),
            Some(TelegramCommand::Positions)
        );
        assert_eq!(
            parse_telegram_command("/logs"),
            Some(TelegramCommand::Logs)
        );
        assert_eq!(
            parse_telegram_command("/config"),
            Some(TelegramCommand::Config)
        );
        assert_eq!(
            parse_telegram_command("/actual-skill"),
            Some(TelegramCommand::ActualSkill)
        );
        assert_eq!(
            parse_telegram_command("/actual_skill"),
            Some(TelegramCommand::ActualSkill)
        );
        assert_eq!(
            parse_telegram_command("/skills"),
            Some(TelegramCommand::ActualSkill)
        );
        assert_eq!(
            parse_telegram_command("/query kenapa belum beli?"),
            Some(TelegramCommand::Query("kenapa belum beli?".to_string()))
        );
        assert_eq!(
            parse_telegram_command("/ask@MyTradeBot analisa log terakhir"),
            Some(TelegramCommand::Query("analisa log terakhir".to_string()))
        );
        assert_eq!(
            parse_telegram_command("/query"),
            Some(TelegramCommand::Query("".to_string()))
        );
        assert_eq!(
            parse_telegram_command("/approve"),
            Some(TelegramCommand::Approve)
        );
        assert_eq!(
            parse_telegram_command("/buy"),
            Some(TelegramCommand::Approve)
        );
        assert_eq!(
            parse_telegram_command("/reject"),
            Some(TelegramCommand::Reject)
        );
        assert_eq!(
            parse_telegram_command("/cancel_order"),
            Some(TelegramCommand::Reject)
        );
        assert_eq!(
            parse_telegram_command("/panic"),
            Some(TelegramCommand::Panic)
        );
        assert_eq!(
            parse_telegram_command("/start"),
            Some(TelegramCommand::Help)
        );
        assert_eq!(
            parse_telegram_command("/help"),
            Some(TelegramCommand::Help)
        );
        assert_eq!(parse_telegram_command("hello world"), None);
        assert_eq!(parse_telegram_command("/unknown"), None);
    }

    #[test]
    fn should_parse_telegram_updates_json_and_enforce_chat_authorization() {
        let raw = json!({
            "ok": true,
            "result": [
                {
                    "update_id": 1001,
                    "message": {
                        "chat": { "id": 123456789i64 },
                        "text": "/status"
                    }
                },
                {
                    "update_id": 1002,
                    "message": {
                        "chat": { "id": "999999" },
                        "text": "/panic"
                    }
                }
            ]
        });

        let updates = parse_telegram_updates_json(&raw);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].update_id, 1001);
        assert_eq!(updates[0].chat_id, "123456789");
        assert_eq!(updates[0].text, "/status");

        assert!(is_authorized_chat(&updates[0].chat_id, "123456789"));
        assert!(!is_authorized_chat(&updates[1].chat_id, "123456789"));
        assert!(!is_authorized_chat("", "123456789"));
    }

    #[test]
    fn should_format_telegram_cycle_notifications_and_help_text() {
        let cfg = BotConfig::default();
        let state = BotState::new(&cfg);
        let mut report = CycleReport {
            timestamp_ms: 1_700_000_000_000,
            symbol: "BTCUSDT".to_string(),
            technical: TechnicalSignal {
                action: SignalAction::Hold,
                fast_ema: 65_000.0,
                slow_ema: 65_000.0,
                rsi: 50.0,
                atr: 100.0,
                last_price: 65_000.0,
                confidence: 0.40,
                reason: "Neutral".to_string(),
            },
            ai: None,
            decision: AgentDecision {
                action: SignalAction::Hold,
                confidence: 0.40,
                technical_reason: "Neutral".to_string(),
                ai_reason: None,
            },
            executed_order: None,
            pending_order: None,
            probability: None,
            closed_positions: 0,
        };

        // No trade and notify_on_cycle=false -> None
        assert!(format_telegram_cycle_notification(&report, &state, true, false).is_none());

        // notify_on_cycle=true -> Some
        let msg_cycle = format_telegram_cycle_notification(&report, &state, true, true).unwrap();
        assert!(msg_cycle.contains("[Trade-Bot Alert] BTCUSDT"));

        // With executed order and notify_on_trade=true -> Some
        report.executed_order = Some(OrderFill {
            order_id: "ord-99".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            position_side: PositionSide::Long,
            executed_qty: 0.01,
            avg_price: 65_000.0,
            stop_loss: Some(64_500.0),
            take_profit: Some(66_000.0),
            realized_pnl: 0.0,
            timestamp_ms: 1_700_000_000_000,
        });
        let msg_trade = format_telegram_cycle_notification(&report, &state, true, false).unwrap();
        assert!(msg_trade.contains("ord-99"));

        let help = format_telegram_help();
        assert!(help.contains("/status"));
        assert!(help.contains("/panic"));
    }

    #[tokio::test]
    async fn should_skip_network_when_telegram_disabled_or_missing_credentials() {
        let mut cfg = TelegramConfig::default();
        cfg.enabled = false;
        let disabled_bot = TelegramBotAdapter::new(cfg.clone(), "".to_string(), "".to_string());
        assert!(!disabled_bot.is_ready());
        assert!(disabled_bot.send_notification("hello").await.is_ok());
        assert!(disabled_bot.fetch_updates(0).await.unwrap().is_empty());

        cfg.enabled = true;
        let missing_creds_bot = TelegramBotAdapter::new(cfg, "".to_string(), "".to_string());
        assert!(missing_creds_bot.send_notification("hello").await.is_err());
    }
}
