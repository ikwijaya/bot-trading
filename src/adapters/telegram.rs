use async_trait::async_trait;
use serde_json::{json, Value};

use crate::core::domain::{BotState, CycleReport, DomainError, TelegramConfig};
use crate::core::ports::NotifierPort;

/// Supported interactive Telegram Bot commands for monitoring, AI Q&A, config update, and emergency control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramCommand {
    Status,
    Positions,
    Logs,
    Config,
    SetConfig { key: String, value: String },
    ActualSkill,
    Query(String),
    Approve,
    Reject,
    Panic,
    Help,
}

/// Access decision for an incoming Telegram message based on chat type (Private DM vs Group) and sender identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramAccessDecision {
    /// Message is in a Private Chat (DM) from the verified Bot Owner: full access + config changes allowed.
    OwnerPrivate,
    /// Message is in an authorized Group / Supergroup: non-PII read queries allowed; config changes prohibited.
    AuthorizedGroup,
    /// Message is from a non-owner in Private Chat or an unauthorized Group: MUST NOT be answered (silent drop).
    DenySilent,
}

/// Parsed incoming message update from Telegram `getUpdates` API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramUpdate {
    pub update_id: i64,
    pub chat_id: String,
    pub sender_id: String,
    pub is_group: bool,
    pub text: String,
}

/// Parses a chat message string into a [`TelegramCommand`], supporting optional `@botname` suffixes,
/// leading `@botname /command` group mentions, and free-text arguments for `/query <text>`.
///
/// # Parameters
/// - `text`: Raw incoming message text from Telegram (DM or Group/Supergroup).
///
/// # Returns
/// `Some(TelegramCommand)` if the message starts with a recognized command, or `None` otherwise.
pub fn parse_telegram_command(text: &str) -> Option<TelegramCommand> {
    let mut trimmed = text.trim();
    let first = trimmed.split_whitespace().next()?.trim();
    // Support group messages like "@MyTradeBot /status" or "@MyTradeBot /query kenapa hold?"
    if first.starts_with('@') {
        trimmed = trimmed[first.len()..].trim();
    }
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
        "/set-config" | "/set_config" | "/setconfig" => {
            let mut parts = rest_arg.splitn(2, char::is_whitespace);
            let key = parts.next().unwrap_or("").trim().to_string();
            let value = parts.next().unwrap_or("").trim().to_string();
            Some(TelegramCommand::SetConfig { key, value })
        }
        "/actual-skill" | "/actual_skill" | "/skills" => Some(TelegramCommand::ActualSkill),
        "/query" | "/ask" | "/ai" => Some(TelegramCommand::Query(rest_arg)),
        "/approve" | "/buy" => Some(TelegramCommand::Approve),
        "/reject" | "/cancel_order" => Some(TelegramCommand::Reject),
        "/panic" | "/panic_close" => Some(TelegramCommand::Panic),
        "/help" | "/start" => Some(TelegramCommand::Help),
        _ => None,
    }
}

/// Splits a comma- or semicolon-separated `TELEGRAM_CHAT_ID` configuration string into individual chat IDs.
/// Supports both personal DM IDs (e.g. `123456789`) and negative Group/Supergroup IDs (e.g. `-100987654321`).
pub fn parse_allowed_chat_ids(allowed_chat_id: &str) -> Vec<String> {
    allowed_chat_id
        .split([',', ';'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Resolves the Bot Owner's personal Telegram User/Chat ID.
/// Uses `explicit_owner_id` (from `TELEGRAM_OWNER_ID`) if provided; otherwise defaults to the first positive
/// (non-group) ID in `allowed_chat_id`.
pub fn resolve_owner_user_id(explicit_owner_id: &str, allowed_chat_id: &str) -> Option<String> {
    let exp = explicit_owner_id.trim();
    if !exp.is_empty() {
        return Some(exp.to_string());
    }
    parse_allowed_chat_ids(allowed_chat_id)
        .into_iter()
        .find(|id| !id.starts_with('-'))
}

/// Evaluates access permissions for an incoming [`TelegramUpdate`]:
/// - **Private Chat (DM):** Strictly restricted to the Bot Owner (`resolve_owner_user_id`). If anyone else
///   messages the bot privately, returns [`TelegramAccessDecision::DenySilent`] so the bot NEVER replies.
/// - **Group / Supergroup Chat:** Allowed (`AuthorizedGroup`) if the group's negative `chat_id` is in `allowed_chat_id`.
pub fn evaluate_telegram_access(
    update: &TelegramUpdate,
    explicit_owner_id: &str,
    allowed_chat_id: &str,
) -> TelegramAccessDecision {
    let chat_id = update.chat_id.trim();
    if chat_id.is_empty() {
        return TelegramAccessDecision::DenySilent;
    }

    if !update.is_group {
        let Some(owner_id) = resolve_owner_user_id(explicit_owner_id, allowed_chat_id) else {
            return TelegramAccessDecision::DenySilent;
        };
        let sender_id = update.sender_id.trim();
        let is_owner_chat = chat_id == owner_id;
        let is_owner_sender = sender_id.is_empty() || sender_id == owner_id;
        if is_owner_chat && is_owner_sender {
            TelegramAccessDecision::OwnerPrivate
        } else {
            TelegramAccessDecision::DenySilent
        }
    } else if is_authorized_chat(chat_id, allowed_chat_id) {
        TelegramAccessDecision::AuthorizedGroup
    } else {
        TelegramAccessDecision::DenySilent
    }
}

/// Redacts Personally Identifiable Information (PII), personal wallet balances, nominal USDT PnL amounts,
/// OS filesystem user paths, and account/order identifiers when displaying data in a Telegram Group.
pub fn sanitize_group_non_pii(text: &str) -> String {
    let mut sanitized_lines = Vec::new();
    sanitized_lines.push(
        "🛡️ [Mode Grup Non-PII: Data Pribadi/Saldo & Perubahan Config Disembunyikan]".to_string(),
    );

    for line in text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("modal awal")
            || lower.contains("saldo saat ini")
            || lower.contains("pnl terealisasi")
            || lower.contains("pnl harian")
            || lower.contains("balance")
            || lower.contains("wallet")
            || lower.contains("min: $")
            || lower.contains("min order")
            || lower.contains("max order")
        {
            // Keep label if present, redact sensitive financial/personal value
            if let Some((label, _)) = line.split_once(':') {
                sanitized_lines.push(format!("{}: [REDACTED - NON-PII GROUP MODE]", label));
            } else {
                sanitized_lines.push("  • [Data Finansial/Pribadi Disembunyikan — Non-PII]".to_string());
            }
            continue;
        }

        let mut cleaned = line.to_string();
        // Redact Unix/macOS/Windows user home paths (/Users/..., /home/..., C:\Users\...)
        for prefix in ["/Users/", "/home/", "C:\\Users\\"] {
            if let Some(start) = cleaned.find(prefix) {
                let tail = &cleaned[start..];
                let end_offset = tail
                    .find([' ', ')', '"', '\'', '|', ','])
                    .unwrap_or(tail.len());
                cleaned.replace_range(start..start + end_offset, "[REDACTED_LOCAL_PATH]");
            }
        }
        sanitized_lines.push(cleaned);
    }

    sanitized_lines.join("\n")
}

/// Validates whether an incoming Telegram `chat_id` matches any ID in the configured `allowed_chat_id`
/// (supports a single ID or comma-separated list of DM + Group IDs).
pub fn is_authorized_chat(incoming_chat_id: &str, allowed_chat_id: &str) -> bool {
    let inc = incoming_chat_id.trim();
    if inc.is_empty() {
        return false;
    }
    parse_allowed_chat_ids(allowed_chat_id)
        .iter()
        .any(|allowed| allowed == inc)
}

/// Discovered Telegram chat/sender metadata extracted from `getUpdates` for `telegram-whoami`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramDiscoveredChat {
    pub chat_id: String,
    pub chat_type: String,
    pub chat_title: String,
    pub sender_id: String,
    pub sender_name: String,
    pub last_text: String,
}

/// Extracts unique [`TelegramDiscoveredChat`] entries from a raw Telegram `getUpdates` JSON response.
pub fn parse_telegram_discovered_chats(raw: &Value) -> Vec<TelegramDiscoveredChat> {
    let Some(results) = raw["result"].as_array() else {
        return Vec::new();
    };

    let mut discovered: Vec<TelegramDiscoveredChat> = Vec::new();
    for item in results {
        let msg = if item["message"].is_object() {
            &item["message"]
        } else if item["edited_message"].is_object() {
            &item["edited_message"]
        } else if item["my_chat_member"].is_object() {
            &item["my_chat_member"]
        } else {
            &item["channel_post"]
        };

        let chat_id = msg["chat"]["id"]
            .as_i64()
            .map(|id| id.to_string())
            .or_else(|| msg["chat"]["id"].as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        if chat_id.is_empty() {
            continue;
        }

        let chat_type = msg["chat"]["type"]
            .as_str()
            .unwrap_or(if chat_id.starts_with('-') {
                "group"
            } else {
                "private"
            })
            .to_string();
        let chat_title = msg["chat"]["title"]
            .as_str()
            .or_else(|| msg["chat"]["username"].as_str())
            .or_else(|| msg["chat"]["first_name"].as_str())
            .unwrap_or("Unknown")
            .to_string();
        let sender_id = msg["from"]["id"]
            .as_i64()
            .map(|id| id.to_string())
            .or_else(|| msg["from"]["id"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| {
                if !chat_id.starts_with('-') {
                    chat_id.clone()
                } else {
                    "-".to_string()
                }
            });
        let sender_name = msg["from"]["username"]
            .as_str()
            .map(|u| format!("@{}", u))
            .or_else(|| msg["from"]["first_name"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "-".to_string());
        let last_text = msg["text"]
            .as_str()
            .unwrap_or("(member/join event)")
            .trim()
            .to_string();

        if let Some(existing) = discovered.iter_mut().find(|d| d.chat_id == chat_id) {
            existing.last_text = last_text;
            if existing.sender_id == "-" && sender_id != "-" {
                existing.sender_id = sender_id;
                existing.sender_name = sender_name;
            }
        } else {
            discovered.push(TelegramDiscoveredChat {
                chat_id,
                chat_type,
                chat_title,
                sender_id,
                sender_name,
                last_text,
            });
        }
    }

    discovered
}

/// Formats discovered Telegram chats into a user-friendly guide showing Owner ID, Group IDs, and `.env` snippet.
pub fn format_telegram_whoami_report(chats: &[TelegramDiscoveredChat]) -> String {
    if chats.is_empty() {
        return "========================================================================\n\
                🔍 BELUM ADA PESAN TELEGRAM YANG TERDETEKSI DI getUpdates\n\
                ========================================================================\n\
                Langkah Mudah Menampilkan Owner ID & Group ID:\n\
                1. Buka chat pribadi ke Bot Telegram Anda, lalu kirim pesan: /start\n\
                2. Masukkan Bot ke dalam Grup Telegram Anda, lalu kirim pesan di grup: /status\n\
                3. Jalankan ulang perintah ini: ./target/release/trade-bot telegram-whoami\n\
                ========================================================================"
            .to_string();
    }

    let mut out = String::from(
        "========================================================================\n\
         🔍 HASIL DETEKSI OWNER ID & GROUP ID TELEGRAM (telegram-whoami)\n\
         ========================================================================\n",
    );

    let mut owner_id_candidate: Option<String> = None;
    let mut all_chat_ids: Vec<String> = Vec::new();

    out.push_str("\n👤 [1. Daftar Akun Pribadi / Owner ID (Private DM — Angka Positif)]\n");
    let mut found_private = false;
    for c in chats.iter().filter(|c| !c.chat_id.starts_with('-')) {
        found_private = true;
        if owner_id_candidate.is_none() {
            owner_id_candidate = Some(c.chat_id.clone());
        }
        all_chat_ids.push(c.chat_id.clone());
        out.push_str(&format!(
            "  • Owner/User ID : {} (Nama: {} / {}, Pesan terakhir: \"{}\")\n",
            c.chat_id, c.chat_title, c.sender_name, c.last_text
        ));
    }
    if !found_private {
        // Also check if any group message had a positive sender_id
        for c in chats.iter().filter(|c| c.sender_id != "-" && !c.sender_id.starts_with('-')) {
            if owner_id_candidate.is_none() {
                owner_id_candidate = Some(c.sender_id.clone());
            }
            out.push_str(&format!(
                "  • Pengirim di Grup (Calon Owner ID): {} ({})\n",
                c.sender_id, c.sender_name
            ));
        }
        if owner_id_candidate.is_none() {
            out.push_str("  • (Belum ada chat pribadi. Kirim /start secara japri ke bot Anda)\n");
        }
    }

    out.push_str("\n👥 [2. Daftar Grup / Supergroup Telegram (Group ID — Angka Negatif)]\n");
    let mut found_group = false;
    for c in chats.iter().filter(|c| c.chat_id.starts_with('-')) {
        found_group = true;
        all_chat_ids.push(c.chat_id.clone());
        out.push_str(&format!(
            "  • Group ID      : {} (Tipe: {}, Nama Grup: \"{}\", Pengirim terakhir: {} [{}])\n",
            c.chat_id, c.chat_type, c.chat_title, c.sender_name, c.sender_id
        ));
    }
    if !found_group {
        out.push_str(
            "  • (Belum ada pesan dari Grup. Undang bot ke Grup lalu ketik /status di dalam Grup)\n",
        );
    }

    let owner_str = owner_id_candidate.unwrap_or_else(|| "123456789".to_string());
    if !all_chat_ids.contains(&owner_str) {
        all_chat_ids.insert(0, owner_str.clone());
    }
    let chat_ids_joined = all_chat_ids.join(",");

    out.push_str(&format!(
        "\n📋 [3. Salin & Tempel Baris Berikut ke File `.env` Anda]\n\
         ------------------------------------------------------------------------\n\
         TELEGRAM_OWNER_ID={}\n\
         TELEGRAM_CHAT_ID={}\n\
         ------------------------------------------------------------------------\n\
         ========================================================================",
        owner_str, chat_ids_joined
    ));

    out
}

/// Parses a Telegram `getUpdates` JSON response into a list of [`TelegramUpdate`] items.
/// Supports `message`, `edited_message`, and `channel_post` objects from private chats, groups, and supergroups.
pub fn parse_telegram_updates_json(raw: &Value) -> Vec<TelegramUpdate> {
    let Some(results) = raw["result"].as_array() else {
        return Vec::new();
    };

    let mut updates = Vec::with_capacity(results.len());
    for item in results {
        let Some(update_id) = item["update_id"].as_i64() else {
            continue;
        };
        let msg = if item["message"].is_object() {
            &item["message"]
        } else if item["edited_message"].is_object() {
            &item["edited_message"]
        } else {
            &item["channel_post"]
        };
        let chat_id = msg["chat"]["id"]
            .as_i64()
            .map(|id| id.to_string())
            .or_else(|| msg["chat"]["id"].as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let chat_type = msg["chat"]["type"].as_str().unwrap_or("");
        let is_group = chat_id.trim().starts_with('-')
            || matches!(chat_type, "group" | "supergroup" | "channel");
        let sender_id = msg["from"]["id"]
            .as_i64()
            .map(|id| id.to_string())
            .or_else(|| msg["from"]["id"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| {
                if !is_group {
                    chat_id.clone()
                } else {
                    String::new()
                }
            });
        let text = msg["text"].as_str().unwrap_or_default().trim().to_string();

        if !chat_id.is_empty() && !text.is_empty() {
            updates.push(TelegramUpdate {
                update_id,
                chat_id,
                sender_id,
                is_group,
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
     /status             - Cek status daemon, saldo USDT, PnL, dan sinyal terakhir\n\
     /positions          - Lihat daftar posisi Futures terbuka & riwayat order fill\n\
     /actual-skill       - Lihat hasil pembelajaran AI & aturan adaptif (skills.md)\n\
     /query <teks>       - Tanya langsung ke AI seputar data trading bot, log, & skills\n\
     /approve            - Setujui & eksekusi order yang sedang ditahan (mode confirm_by_chat)\n\
     /reject             - Batalkan order yang sedang ditahan\n\
     /logs               - Lihat 10 baris log terakhir dari daemon\n\
     /config             - Lihat ringkasan konfigurasi aktif\n\
     /set-config <k> <v> - Ubah konfigurasi (Hanya Owner di Private Chat; Dilarang di Grup)\n\
     /panic              - DARURAT: Hentikan daemon & tutup semua posisi terbuka\n\
     /help               - Tampilkan panduan perintah ini"
        .to_string()
}

/// HTTP adapter for Telegram Bot API (`sendMessage` & `getUpdates`).
#[derive(Clone)]
pub struct TelegramBotAdapter {
    http: reqwest::Client,
    cfg: TelegramConfig,
    bot_token: String,
    chat_id: String,
    owner_id: String,
}

impl TelegramBotAdapter {
    /// Creates a new [`TelegramBotAdapter`] from configuration and resolved credentials.
    pub fn new(cfg: TelegramConfig, bot_token: String, chat_id: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        let owner_id = resolve_owner_user_id("", &chat_id).unwrap_or_default();
        Self {
            http,
            cfg,
            bot_token: bot_token.trim().to_string(),
            chat_id: chat_id.trim().to_string(),
            owner_id,
        }
    }

    /// Creates a [`TelegramBotAdapter`] reading token, chat ID, and optional `TELEGRAM_OWNER_ID` from environment variables.
    pub fn from_env(cfg: &TelegramConfig) -> Self {
        let token = std::env::var(&cfg.bot_token_env).unwrap_or_default();
        let chat_id = std::env::var(&cfg.chat_id_env).unwrap_or_default();
        let explicit_owner = std::env::var("TELEGRAM_OWNER_ID").unwrap_or_default();
        let mut adapter = Self::new(cfg.clone(), token, chat_id);
        if !explicit_owner.trim().is_empty() {
            adapter.owner_id = explicit_owner.trim().to_string();
        }
        adapter
    }

    /// Returns `true` if Telegram integration is enabled and credentials are provided.
    pub fn is_ready(&self) -> bool {
        self.cfg.enabled && !self.bot_token.is_empty() && !self.chat_id.is_empty()
    }

    /// Returns the configured authorized chat ID list string.
    pub fn authorized_chat_id(&self) -> &str {
        &self.chat_id
    }

    /// Returns the resolved Bot Owner User/Chat ID for Private Chat verification.
    pub fn owner_id(&self) -> &str {
        &self.owner_id
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

    /// Queries `getUpdates` using only `bot_token` (no `TELEGRAM_CHAT_ID` required yet) to discover
    /// the user's personal Owner ID and any Group IDs where the bot has received messages.
    pub async fn discover_chats(&self) -> Result<Vec<TelegramDiscoveredChat>, DomainError> {
        if self.bot_token.is_empty() {
            return Err(DomainError::NotificationError {
                message: format!(
                    "Token Bot Telegram belum diatur: isi {} di file .env terlebih dahulu",
                    self.cfg.bot_token_env
                ),
            });
        }
        let url = format!("{}?timeout=1", self.endpoint("getUpdates"));
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

        Ok(parse_telegram_discovered_chats(&body))
    }

    /// Sends a message to a specific `target_chat_id` (e.g., replying directly to a Group or DM where a command originated).
    pub async fn send_message_to_chat(
        &self,
        target_chat_id: &str,
        message: &str,
    ) -> Result<(), DomainError> {
        if !self.cfg.enabled {
            return Ok(());
        }
        let target = target_chat_id.trim();
        if self.bot_token.is_empty() || target.is_empty() {
            return Err(DomainError::NotificationError {
                message: format!(
                    "Telegram credentials missing: set {} and {} in .env",
                    self.cfg.bot_token_env, self.cfg.chat_id_env
                ),
            });
        }

        let url = self.endpoint("sendMessage");
        let payload = json!({
            "chat_id": target,
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

#[async_trait]
impl NotifierPort for TelegramBotAdapter {
    async fn send_notification(&self, message: &str) -> Result<(), DomainError> {
        if !self.cfg.enabled {
            return Ok(());
        }
        let targets = parse_allowed_chat_ids(&self.chat_id);
        if self.bot_token.is_empty() || targets.is_empty() {
            return Err(DomainError::NotificationError {
                message: format!(
                    "Telegram credentials missing: set {} and {} in .env",
                    self.cfg.bot_token_env, self.cfg.chat_id_env
                ),
            });
        }

        for target in targets {
            self.send_message_to_chat(&target, message).await?;
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
            parse_telegram_command("@MyTradeBot /status"),
            Some(TelegramCommand::Status)
        );
        assert_eq!(
            parse_telegram_command("@MyTradeBot /query analisa btc hari ini"),
            Some(TelegramCommand::Query("analisa btc hari ini".to_string()))
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
        assert_eq!(
            parse_telegram_command("/set-config symbol ETHUSDT"),
            Some(TelegramCommand::SetConfig {
                key: "symbol".to_string(),
                value: "ETHUSDT".to_string()
            })
        );
        assert_eq!(parse_telegram_command("hello world"), None);
        assert_eq!(parse_telegram_command("@MyTradeBot halo biasa"), None);
        assert_eq!(parse_telegram_command("/unknown"), None);
    }

    #[test]
    fn should_enforce_owner_only_in_private_chat_and_non_pii_in_group_chat() {
        let allowed_list = "123456789, -100987654321";

        // 1. Owner in Private Chat -> OwnerPrivate
        let owner_dm = TelegramUpdate {
            update_id: 1,
            chat_id: "123456789".to_string(),
            sender_id: "123456789".to_string(),
            is_group: false,
            text: "/status".to_string(),
        };
        assert_eq!(
            evaluate_telegram_access(&owner_dm, "", allowed_list),
            TelegramAccessDecision::OwnerPrivate
        );

        // 2. Non-Owner asking privately -> DenySilent (MUST NOT ANSWER)
        let stranger_dm = TelegramUpdate {
            update_id: 2,
            chat_id: "999888777".to_string(),
            sender_id: "999888777".to_string(),
            is_group: false,
            text: "/status".to_string(),
        };
        assert_eq!(
            evaluate_telegram_access(&stranger_dm, "", allowed_list),
            TelegramAccessDecision::DenySilent
        );

        // 3. Member asking in Authorized Group -> AuthorizedGroup
        let group_msg = TelegramUpdate {
            update_id: 3,
            chat_id: "-100987654321".to_string(),
            sender_id: "555444333".to_string(),
            is_group: true,
            text: "/status".to_string(),
        };
        assert_eq!(
            evaluate_telegram_access(&group_msg, "", allowed_list),
            TelegramAccessDecision::AuthorizedGroup
        );

        // 4. Unauthorized Group -> DenySilent
        let unauth_group = TelegramUpdate {
            update_id: 4,
            chat_id: "-100111222333".to_string(),
            sender_id: "555444333".to_string(),
            is_group: true,
            text: "/status".to_string(),
        };
        assert_eq!(
            evaluate_telegram_access(&unauth_group, "", allowed_list),
            TelegramAccessDecision::DenySilent
        );

        // 5. Sanitize Non-PII for Group output
        let raw_report = "Symbol: BTCUSDT\n  • Modal Awal       : $1000.00 USDT\n  • Saldo Saat Ini   : $1250.50 USDT\n  • PnL Terealisasi  : +250.50 USDT\nLog path: /Users/irpankusuma/sample-app/trade/.trade-bot/bot.log";
        let sanitized = sanitize_group_non_pii(raw_report);
        assert!(sanitized.contains("Mode Grup Non-PII"));
        assert!(sanitized.contains("BTCUSDT"));
        assert!(!sanitized.contains("1250.50"));
        assert!(!sanitized.contains("250.50"));
        assert!(!sanitized.contains("/Users/irpankusuma"));
        assert!(sanitized.contains("[REDACTED - NON-PII GROUP MODE]"));
        assert!(sanitized.contains("[REDACTED_LOCAL_PATH]"));
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
                        "chat": { "id": -100987654321i64, "type": "supergroup" },
                        "text": "/status@MyTradeBot"
                    }
                },
                {
                    "update_id": 1003,
                    "channel_post": {
                        "chat": { "id": "-100555666" },
                        "text": "/positions"
                    }
                }
            ]
        });

        let updates = parse_telegram_updates_json(&raw);
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[0].update_id, 1001);
        assert_eq!(updates[0].chat_id, "123456789");
        assert_eq!(updates[0].text, "/status");
        assert_eq!(updates[1].chat_id, "-100987654321");
        assert_eq!(updates[2].chat_id, "-100555666");

        let allowed_list = "123456789, -100987654321";
        assert!(is_authorized_chat(&updates[0].chat_id, allowed_list));
        assert!(is_authorized_chat(&updates[1].chat_id, allowed_list));
        assert!(!is_authorized_chat(&updates[2].chat_id, allowed_list));
        assert!(!is_authorized_chat("", allowed_list));
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
        assert!(missing_creds_bot.discover_chats().await.is_err());
    }

    #[test]
    fn should_parse_and_format_discovered_owner_and_group_ids() {
        let empty_report = format_telegram_whoami_report(&[]);
        assert!(empty_report.contains("BELUM ADA PESAN TELEGRAM"));

        let raw = json!({
            "ok": true,
            "result": [
                {
                    "update_id": 2001,
                    "message": {
                        "chat": { "id": 777888999i64, "type": "private", "first_name": "Irpan" },
                        "from": { "id": 777888999i64, "username": "irpan_user" },
                        "text": "/start"
                    }
                },
                {
                    "update_id": 2002,
                    "message": {
                        "chat": { "id": -1001122334455i64, "type": "supergroup", "title": "Crypto Signal Group" },
                        "from": { "id": 777888999i64, "username": "irpan_user" },
                        "text": "/status"
                    }
                }
            ]
        });

        let chats = parse_telegram_discovered_chats(&raw);
        assert_eq!(chats.len(), 2);
        assert_eq!(chats[0].chat_id, "777888999");
        assert_eq!(chats[1].chat_id, "-1001122334455");

        let report = format_telegram_whoami_report(&chats);
        assert!(report.contains("TELEGRAM_OWNER_ID=777888999"));
        assert!(report.contains("TELEGRAM_CHAT_ID=777888999,-1001122334455"));
        assert!(report.contains("Crypto Signal Group"));
    }
}
