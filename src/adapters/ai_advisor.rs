use async_trait::async_trait;
use serde_json::{json, Value};

use crate::core::domain::{
    AiAssessment, AiConfig, BotConfig, BotState, DomainError, Position, SignalAction,
    TechnicalSignal,
};
use crate::core::ports::AiAdvisorPort;

/// Returns the default system prompt for the interactive `/query` trading bot AI assistant when `AI_SYSTEM_PROMPT` is not set.
pub fn default_trading_assistant_system_prompt() -> &'static str {
    "Anda adalah Asisten AI Kuantitatif resmi di dalam aplikasi Trade-Bot (Multi-Exchange Futures Hybrid Trading Bot). \
     Tugas Anda adalah menjawab pertanyaan pengguna dalam Bahasa Indonesia yang jelas, akurat, dan ramah berdasarkan \
     data real-time bot yang diberikan (konfigurasi aktif, saldo USDT, Realized PnL, posisi terbuka, riwayat transaksi, \
     order tertahan, aturan pembelajaran skills.md, serta log aktivitas daemon terakhir). \
     Jangan mengarang angka di luar konteks data trading bot yang dilampirkan."
}

/// Pure function that builds `(system_prompt, user_prompt)` for an interactive `/query <text>` request,
/// injecting the bot's active configuration, live state, open positions, recent fills, learned `skills.md`, and recent logs.
pub fn build_ai_query_messages(
    question: &str,
    custom_system_prompt: Option<&str>,
    cfg: &BotConfig,
    state: &BotState,
    learned_skills: Option<&str>,
    recent_logs: &[String],
) -> (String, String) {
    let sys_prompt = custom_system_prompt
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_trading_assistant_system_prompt())
        .to_string();

    let positions_summary = if state.open_positions.is_empty() {
        "Tidak ada posisi terbuka saat ini.".to_string()
    } else {
        state
            .open_positions
            .iter()
            .map(|p| {
                format!(
                    "- {} {:?} qty={:.3} | Entry={:.2} | Mark={:.2} | uPnL={:+.2} USDT | Lev={}x | SL={:?} | TP={:?}",
                    p.symbol, p.side, p.quantity, p.entry_price, p.mark_price, p.unrealized_pnl, p.leverage, p.stop_loss, p.take_profit
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let recent_fills_summary = if state.recent_fills.is_empty() {
        "Belum ada riwayat order fill.".to_string()
    } else {
        let start = state.recent_fills.len().saturating_sub(5);
        state.recent_fills[start..]
            .iter()
            .map(|f| {
                format!(
                    "- [{}] {} {:?} {:?} qty={:.3} @ {:.2} (Realized PnL={:+.2} USDT)",
                    f.order_id, f.symbol, f.position_side, f.side, f.executed_qty, f.avg_price, f.realized_pnl
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let pending_summary = match &state.pending_order {
        Some(p) => format!(
            "{:?} {} qty={:.3} (~{:.2} USDT) @ {:.2} | WinProb={:.1}% | EV={:+.2}R | Reason={}",
            p.request.position_side,
            p.request.symbol,
            p.request.quantity,
            p.notional_usdt,
            p.request.entry_price,
            p.probability.win_probability * 100.0,
            p.probability.expected_value_r,
            p.reason
        ),
        None => "Tidak ada pending order.".to_string(),
    };

    let last_decision_summary = match &state.last_decision {
        Some(d) => format!(
            "{:?} (confidence {:.2}) — Tech: {} | AI: {:?}",
            d.action, d.confidence, d.technical_reason, d.ai_reason
        ),
        None => "Belum ada keputusan siklus.".to_string(),
    };

    let skills_section = learned_skills
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(Belum ada dokumen skills.md)");

    let logs_section = if recent_logs.is_empty() {
        "(Belum ada baris log daemon)".to_string()
    } else {
        recent_logs.join("\n")
    };

    let user_prompt = format!(
        "=== KONTEKS DATA INTERNAL TRADE-BOT SAAT INI ===\n\
         [1. Konfigurasi Aktif]\n\
         - Exchange Provider : {}\n\
         - Mode Eksekusi     : {}\n\
         - Symbol & Interval : {} ({})\n\
         - Decision Policy   : {}\n\
         - Sizing Mode       : {} (Min: ${:.2} USDT, Max: ${:.2} USDT, Require +EV: {})\n\
         - Leverage          : {}x (Max: {}x) | Risk/Trade: {:.2}%\n\
         - Indikator         : EMA({}/{}) | RSI({}, OB={:.1}, OS={:.1}) | ATR({}, SL={:.1}x, TP={:.1}x)\n\n\
         [2. Status Akun, Saldo & Performa]\n\
         - Status Running    : {} (PID: {:?})\n\
         - Saldo Dompet      : {:.2} USDT (Saldo Awal Hari: {:.2} USDT)\n\
         - Realized PnL      : {:+.2} USDT | Total Transaksi: {}\n\
         - Keputusan Terakhir: {}\n\
         - Pending Order     : {}\n\n\
         [3. Posisi Futures Terbuka]\n\
         {}\n\n\
         [4. 5 Riwayat Transaksi Terakhir]\n\
         {}\n\n\
         [5. Memori Pembelajaran Berkelanjutan (skills.md)]\n\
         {}\n\n\
         [6. Log Aktivitas Daemon Terakhir (bot.log)]\n\
         {}\n\
         ================================================\n\n\
         PERTANYAAN PENGGUNA (/query):\n\
         {}",
        cfg.exchange.provider,
        cfg.mode,
        cfg.symbol,
        cfg.interval,
        cfg.strategy.decision_policy,
        cfg.risk.sizing_mode,
        cfg.risk.min_order_usdt,
        cfg.risk.max_order_usdt,
        cfg.risk.require_positive_ev,
        cfg.risk.leverage,
        cfg.risk.max_leverage,
        cfg.risk.risk_per_trade_pct * 100.0,
        cfg.strategy.fast_ema_period,
        cfg.strategy.slow_ema_period,
        cfg.strategy.rsi_period,
        cfg.strategy.rsi_overbought,
        cfg.strategy.rsi_oversold,
        cfg.strategy.atr_period,
        cfg.risk.atr_sl_multiplier,
        cfg.risk.atr_tp_multiplier,
        state.running,
        state.pid,
        state.current_balance_usdt,
        state.day_start_balance_usdt,
        state.realized_pnl_usdt,
        state.total_trades,
        last_decision_summary,
        pending_summary,
        positions_summary,
        recent_fills_summary,
        skills_section,
        logs_section,
        question.trim()
    );

    (sys_prompt, user_prompt)
}

/// Extracts the plain-text assistant message from an OpenAI-compatible Chat Completions JSON response.
pub fn parse_ai_text_reply_json(raw: &Value) -> Result<String, DomainError> {
    let content = raw["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| DomainError::AiAdvisorError {
            message: "missing choices[0].message.content in LLM response".to_string(),
        })?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(DomainError::AiAdvisorError {
            message: "empty message content returned by LLM".to_string(),
        });
    }
    Ok(trimmed.to_string())
}

/// Builds a concise structured prompt for an LLM trading advisor, including optional learned skills context.
pub fn build_ai_prompt(
    symbol: &str,
    signal: &TechnicalSignal,
    open_positions: &[Position],
) -> String {
    build_ai_prompt_with_skills(symbol, signal, open_positions, None)
}

/// Builds a structured prompt for an LLM trading advisor enriched with continuous-learning `skills.md` memory.
pub fn build_ai_prompt_with_skills(
    symbol: &str,
    signal: &TechnicalSignal,
    open_positions: &[Position],
    learned_skills: Option<&str>,
) -> String {
    let skills_block = match learned_skills.map(str::trim).filter(|s| !s.is_empty()) {
        Some(skills) => format!("\n--- Continuous Learning Context (skills.md) ---\n{}\n-----------------------------------------------\n", skills),
        None => String::new(),
    };
    format!(
        "You are a quantitative Futures risk & signal advisor with continuous memory.\n\
         Symbol: {symbol}\n\
         Last Price: {:.2}\n\
         Fast EMA: {:.2} | Slow EMA: {:.2}\n\
         RSI: {:.1} | ATR: {:.2}\n\
         Technical Signal: {:?} (confidence {:.2}) - {}\n\
         Active Positions Count: {}{}\n\
         Respond strictly with a JSON object: \
         {{\"action\":\"LONG|SHORT|HOLD|CLOSE\",\"confidence\":0.0-1.0,\"reasoning\":\"short explanation incorporating learned skills\"}}",
        signal.last_price,
        signal.fast_ema,
        signal.slow_ema,
        signal.rsi,
        signal.atr,
        signal.action,
        signal.confidence,
        signal.reason,
        open_positions.len(),
        skills_block
    )
}

/// Parses an OpenAI-compatible chat completion JSON response into an [`AiAssessment`].
pub fn parse_ai_response_json(raw: &Value) -> Result<AiAssessment, DomainError> {
    let content = raw["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| DomainError::AiAdvisorError {
            message: "missing choices[0].message.content in LLM response".to_string(),
        })?;

    let cleaned = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let parsed: Value =
        serde_json::from_str(cleaned).map_err(|e| DomainError::AiAdvisorError {
            message: format!("invalid JSON inside LLM message content: {}", e),
        })?;

    let action_str = parsed["action"].as_str().unwrap_or("HOLD").to_uppercase();
    let action = match action_str.as_str() {
        "LONG" => SignalAction::Long,
        "SHORT" => SignalAction::Short,
        "CLOSE" => SignalAction::Close,
        _ => SignalAction::Hold,
    };
    let confidence = parsed["confidence"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0);
    let reasoning = parsed["reasoning"]
        .as_str()
        .unwrap_or("AI assessment completed")
        .to_string();

    Ok(AiAssessment {
        action,
        confidence,
        reasoning,
    })
}

/// Optional HTTP LLM Advisor implementing [`AiAdvisorPort`].
pub struct HttpAiAdvisor {
    http: reqwest::Client,
    cfg: AiConfig,
    api_key: Option<String>,
    learned_skills: Option<String>,
}

impl HttpAiAdvisor {
    /// Creates a new [`HttpAiAdvisor`].
    pub fn new(cfg: AiConfig, api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(12))
            .build()
            .unwrap_or_default();
        Self {
            http,
            cfg,
            api_key,
            learned_skills: None,
        }
    }

    /// Attaches the accumulated `skills.md` continuous-learning context to be injected into LLM prompts.
    pub fn with_learned_skills(mut self, learned_skills: Option<String>) -> Self {
        self.learned_skills = learned_skills;
        self
    }

    /// Sends a direct conversational `/query` request to the configured LLM endpoint with system & user prompts.
    pub async fn query_assistant(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String, DomainError> {
        if !self.cfg.enabled {
            return Err(DomainError::AiAdvisorError {
                message: "Fitur AI belum aktif (`ai.enabled = false`). Ubah `\"ai\": { \"enabled\": true }` di config dan isi `AI_API_KEY` di `.env`.".to_string(),
            });
        }
        let key = match &self.api_key {
            Some(k) if !k.trim().is_empty() => k,
            _ => {
                return Err(DomainError::AiAdvisorError {
                    message: format!(
                        "API Key AI belum ditemukan. Pastikan variabel `{}` sudah diisi di file `.env`.",
                        self.cfg.api_key_env
                    ),
                });
            }
        };

        let payload = json!({
            "model": self.cfg.model,
            "temperature": 0.2,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ]
        });

        let resp = self
            .http
            .post(&self.cfg.endpoint_url)
            .bearer_auth(key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| DomainError::AiAdvisorError {
                message: format!("AI query HTTP request failed: {}", e),
            })?;

        if !resp.status().is_success() {
            return Err(DomainError::AiAdvisorError {
                message: format!("AI query HTTP status {}", resp.status()),
            });
        }

        let body: Value = resp
            .json()
            .await
            .map_err(|e| DomainError::AiAdvisorError {
                message: format!("AI query JSON decode failed: {}", e),
            })?;

        parse_ai_text_reply_json(&body)
    }
}

#[async_trait]
impl AiAdvisorPort for HttpAiAdvisor {
    async fn assess_market(
        &self,
        symbol: &str,
        signal: &TechnicalSignal,
        open_positions: &[Position],
    ) -> Result<Option<AiAssessment>, DomainError> {
        if !self.cfg.enabled {
            return Ok(None);
        }
        let key = match &self.api_key {
            Some(k) if !k.trim().is_empty() => k,
            _ => return Ok(None),
        };

        let prompt = build_ai_prompt_with_skills(
            symbol,
            signal,
            open_positions,
            self.learned_skills.as_deref(),
        );
        let payload = json!({
            "model": self.cfg.model,
            "temperature": 0.1,
            "messages": [
                {"role": "user", "content": prompt}
            ]
        });

        let resp = self
            .http
            .post(&self.cfg.endpoint_url)
            .bearer_auth(key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| DomainError::AiAdvisorError {
                message: format!("AI HTTP request failed: {}", e),
            })?;

        if !resp.status().is_success() {
            return Err(DomainError::AiAdvisorError {
                message: format!("AI HTTP status {}", resp.status()),
            });
        }

        let body: Value = resp
            .json()
            .await
            .map_err(|e| DomainError::AiAdvisorError {
                message: format!("AI JSON decode failed: {}", e),
            })?;

        parse_ai_response_json(&body).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_build_ai_prompt_and_parse_response_json() {
        let sig = TechnicalSignal {
            action: SignalAction::Long,
            fast_ema: 65_200.0,
            slow_ema: 64_900.0,
            rsi: 58.5,
            atr: 210.0,
            last_price: 65_300.0,
            confidence: 0.72,
            reason: "Bullish".to_string(),
        };
        let prompt = build_ai_prompt("BTCUSDT", &sig, &[]);
        assert!(prompt.contains("BTCUSDT"));
        assert!(prompt.contains("65300.00"));

        let prompt_with_skills =
            build_ai_prompt_with_skills("BTCUSDT", &sig, &[], Some("Rule: Avoid RSI > 72"));
        assert!(prompt_with_skills.contains("Continuous Learning Context"));
        assert!(prompt_with_skills.contains("Avoid RSI > 72"));

        let response = json!({
            "choices": [
                {
                    "message": {
                        "content": "```json\n{\"action\":\"LONG\",\"confidence\":0.84,\"reasoning\":\"Trend confirmed\"}\n```"
                    }
                }
            ]
        });
        let assessment = parse_ai_response_json(&response).unwrap();
        assert_eq!(assessment.action, SignalAction::Long);
        assert!((assessment.confidence - 0.84).abs() < 1e-6);
        assert_eq!(assessment.reasoning, "Trend confirmed");
    }

    #[tokio::test]
    async fn should_return_none_when_ai_advisor_disabled_or_no_key() {
        let mut cfg = AiConfig::default();
        cfg.enabled = false;
        let advisor = HttpAiAdvisor::new(cfg.clone(), None);
        let sig = TechnicalSignal {
            action: SignalAction::Hold,
            fast_ema: 1.0,
            slow_ema: 1.0,
            rsi: 50.0,
            atr: 1.0,
            last_price: 1.0,
            confidence: 0.5,
            reason: "Flat".to_string(),
        };
        assert_eq!(
            advisor.assess_market("BTCUSDT", &sig, &[]).await.unwrap(),
            None
        );

        cfg.enabled = true;
        let advisor_no_key = HttpAiAdvisor::new(cfg.clone(), None);
        assert_eq!(
            advisor_no_key
                .assess_market("BTCUSDT", &sig, &[])
                .await
                .unwrap(),
            None
        );
        assert!(advisor_no_key
            .query_assistant("sys", "user")
            .await
            .is_err());

        cfg.enabled = false;
        let advisor_disabled = HttpAiAdvisor::new(cfg, Some("sk-test".to_string()));
        assert!(advisor_disabled
            .query_assistant("sys", "user")
            .await
            .is_err());
    }

    #[test]
    fn should_build_ai_query_messages_and_parse_text_reply() {
        let cfg = BotConfig::default();
        let state = BotState::new(&cfg);
        let logs = vec!["[2026-11-15] Symbol=BTCUSDT | Decision=Long".to_string()];

        let (sys_default, user_prompt) = build_ai_query_messages(
            "Kenapa belum ada posisi terbuka?",
            None,
            &cfg,
            &state,
            Some("# Learned Rule 1"),
            &logs,
        );
        assert!(sys_default.contains("Asisten AI Kuantitatif"));
        assert!(user_prompt.contains("Kenapa belum ada posisi terbuka?"));
        assert!(user_prompt.contains("# Learned Rule 1"));
        assert!(user_prompt.contains("Decision=Long"));

        let (sys_custom, _) = build_ai_query_messages(
            "Halo AI",
            Some("Custom Bot System Prompt"),
            &cfg,
            &state,
            None,
            &[],
        );
        assert_eq!(sys_custom, "Custom Bot System Prompt");

        let reply_json = json!({
            "choices": [
                {
                    "message": {
                        "content": "Saat ini bot masih menunggu sinyal dengan Expected Value positif."
                    }
                }
            ]
        });
        let parsed = parse_ai_text_reply_json(&reply_json).unwrap();
        assert!(parsed.contains("Expected Value positif"));
        assert!(parse_ai_text_reply_json(&json!({})).is_err());
    }
}
