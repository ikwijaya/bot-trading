use async_trait::async_trait;
use serde_json::{json, Value};

use crate::core::domain::{
    AiAssessment, AiConfig, DomainError, Position, SignalAction, TechnicalSignal,
};
use crate::core::ports::AiAdvisorPort;

/// Builds a concise structured prompt for an LLM trading advisor.
pub fn build_ai_prompt(
    symbol: &str,
    signal: &TechnicalSignal,
    open_positions: &[Position],
) -> String {
    format!(
        "You are a quantitative Binance Futures risk & signal advisor.\n\
         Symbol: {symbol}\n\
         Last Price: {:.2}\n\
         Fast EMA: {:.2} | Slow EMA: {:.2}\n\
         RSI: {:.1} | ATR: {:.2}\n\
         Technical Signal: {:?} (confidence {:.2}) - {}\n\
         Active Positions Count: {}\n\
         Respond strictly with a JSON object: \
         {{\"action\":\"LONG|SHORT|HOLD|CLOSE\",\"confidence\":0.0-1.0,\"reasoning\":\"short explanation\"}}",
        signal.last_price,
        signal.fast_ema,
        signal.slow_ema,
        signal.rsi,
        signal.atr,
        signal.action,
        signal.confidence,
        signal.reason,
        open_positions.len()
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
}

impl HttpAiAdvisor {
    /// Creates a new [`HttpAiAdvisor`].
    pub fn new(cfg: AiConfig, api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(12))
            .build()
            .unwrap_or_default();
        Self { http, cfg, api_key }
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

        let prompt = build_ai_prompt(symbol, signal, open_positions);
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
        let advisor_no_key = HttpAiAdvisor::new(cfg, None);
        assert_eq!(
            advisor_no_key
                .assess_market("BTCUSDT", &sig, &[])
                .await
                .unwrap(),
            None
        );
    }
}
