use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

use crate::core::domain::{
    AccountSnapshot, Candle, DomainError, ExchangeCredentialsConfig, ExecutionMode, OrderFill,
    OrderSide, Position, PositionSide, TradeOrderRequest,
};
use crate::core::ports::{ClockPort, ExchangePort};

type HmacSha256 = Hmac<Sha256>;

/// Generates a lowercase hex-encoded HMAC-SHA256 signature for a Binance API query string.
///
/// # Parameters
/// - `query`: URL query string without leading `?`.
/// - `secret`: Binance API secret key.
///
/// # Returns
/// 64-character lowercase hexadecimal HMAC-SHA256 digest.
pub fn sign_binance_query(query: &str, secret: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any size");
    mac.update(query.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Resolves the Binance USDⓈ-M Futures base URL for the given [`ExecutionMode`].
pub fn resolve_binance_base_url(
    mode: ExecutionMode,
    creds_cfg: &ExchangeCredentialsConfig,
) -> String {
    match mode {
        ExecutionMode::Live => creds_cfg.live_base_url.trim_end_matches('/').to_string(),
        ExecutionMode::Testnet | ExecutionMode::Paper => {
            creds_cfg.testnet_base_url.trim_end_matches('/').to_string()
        }
    }
}

/// Builds a signed query string with `recvWindow`, `timestamp`, and HMAC-SHA256 `signature`.
pub fn build_signed_query(
    params: &[(&str, String)],
    recv_window_ms: u64,
    timestamp_ms: i64,
    secret: &str,
) -> String {
    let mut parts: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();
    parts.push(format!("recvWindow={}", recv_window_ms));
    parts.push(format!("timestamp={}", timestamp_ms));
    let unsigned = parts.join("&");
    let sig = sign_binance_query(&unsigned, secret);
    format!("{}&signature={}", unsigned, sig)
}

/// Parses Binance `/fapi/v1/klines` JSON array payload into a vector of [`Candle`] items.
pub fn parse_binance_klines_json(raw: &Value) -> Result<Vec<Candle>, DomainError> {
    let rows = raw.as_array().ok_or_else(|| DomainError::ExchangeError {
        message: "expected JSON array from /fapi/v1/klines".to_string(),
    })?;

    let mut candles = Vec::with_capacity(rows.len());
    for row in rows {
        let items = row.as_array().ok_or_else(|| DomainError::ExchangeError {
            message: "expected kline entry to be an array".to_string(),
        })?;
        if items.len() < 7 {
            return Err(DomainError::ExchangeError {
                message: format!("kline array has fewer than 7 elements: {}", items.len()),
            });
        }
        let open_time = items[0].as_i64().unwrap_or(0);
        let open = parse_json_f64(&items[1], "open")?;
        let high = parse_json_f64(&items[2], "high")?;
        let low = parse_json_f64(&items[3], "low")?;
        let close = parse_json_f64(&items[4], "close")?;
        let volume = parse_json_f64(&items[5], "volume")?;
        let close_time = items[6].as_i64().unwrap_or(0);

        candles.push(Candle {
            open_time,
            open,
            high,
            low,
            close,
            volume,
            close_time,
        });
    }

    Ok(candles)
}

/// Parses Binance `/fapi/v2/account` JSON response into an [`AccountSnapshot`].
pub fn parse_binance_account_json(
    raw: &Value,
    symbol_filter: &str,
) -> Result<AccountSnapshot, DomainError> {
    let total_wallet_balance = parse_json_f64(&raw["totalWalletBalance"], "totalWalletBalance")?;
    let available_balance = parse_json_f64(&raw["availableBalance"], "availableBalance")?;
    let unrealized_pnl = parse_json_f64(&raw["totalUnrealizedProfit"], "totalUnrealizedProfit")
        .unwrap_or(0.0);

    let mut positions = Vec::new();
    if let Some(pos_array) = raw["positions"].as_array() {
        for p in pos_array {
            let sym = p["symbol"].as_str().unwrap_or_default();
            if !symbol_filter.is_empty() && !sym.eq_ignore_ascii_case(symbol_filter) {
                continue;
            }
            let amt = parse_json_f64(&p["positionAmt"], "positionAmt").unwrap_or(0.0);
            if amt.abs() < 1e-12 {
                continue;
            }
            let side = if amt > 0.0 {
                PositionSide::Long
            } else {
                PositionSide::Short
            };
            let entry_price = parse_json_f64(&p["entryPrice"], "entryPrice").unwrap_or(0.0);
            let unrealized = parse_json_f64(&p["unrealizedProfit"], "unrealizedProfit").unwrap_or(0.0);
            let leverage = p["leverage"]
                .as_str()
                .and_then(|s| s.parse::<u32>().ok())
                .or_else(|| p["leverage"].as_u64().map(|v| v as u32))
                .unwrap_or(1);

            positions.push(Position {
                symbol: sym.to_string(),
                side,
                quantity: amt.abs(),
                entry_price,
                mark_price: entry_price,
                unrealized_pnl: unrealized,
                leverage,
                stop_loss: None,
                take_profit: None,
            });
        }
    }

    Ok(AccountSnapshot {
        total_wallet_balance,
        available_balance,
        unrealized_pnl,
        positions,
    })
}

fn parse_json_f64(val: &Value, field_name: &str) -> Result<f64, DomainError> {
    if let Some(f) = val.as_f64() {
        return Ok(f);
    }
    if let Some(s) = val.as_str() {
        return s.parse::<f64>().map_err(|e| DomainError::ExchangeError {
            message: format!("invalid float in '{}': {}", field_name, e),
        });
    }
    Err(DomainError::ExchangeError {
        message: format!("missing or non-numeric field '{}'", field_name),
    })
}

/// Concrete Binance USDⓈ-M Futures HTTP REST client adapter.
pub struct BinanceFuturesClient<C> {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    api_secret: String,
    recv_window_ms: u64,
    clock: C,
}

impl<C: ClockPort> BinanceFuturesClient<C> {
    /// Creates a new Binance USDⓈ-M Futures client.
    pub fn new(
        base_url: String,
        api_key: String,
        api_secret: String,
        recv_window_ms: u64,
        clock: C,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            api_secret,
            recv_window_ms,
            clock,
        }
    }

    fn ensure_credentials(&self) -> Result<(), DomainError> {
        if self.api_key.trim().is_empty() || self.api_secret.trim().is_empty() {
            return Err(DomainError::ExchangeError {
                message: "Binance API key/secret not configured in environment".to_string(),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl<C: ClockPort> ExchangePort for BinanceFuturesClient<C> {
    async fn fetch_klines(
        &self,
        symbol: &str,
        interval: &str,
        limit: usize,
    ) -> Result<Vec<Candle>, DomainError> {
        let url = format!(
            "{}/fapi/v1/klines?symbol={}&interval={}&limit={}",
            self.base_url,
            symbol.to_uppercase(),
            interval,
            limit
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DomainError::ExchangeError {
                message: format!("GET klines network error: {}", e),
            })?;

        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| DomainError::ExchangeError {
            message: format!("GET klines invalid JSON: {}", e),
        })?;

        if !status.is_success() {
            return Err(DomainError::ExchangeError {
                message: format!("Binance klines HTTP {}: {}", status, body),
            });
        }

        parse_binance_klines_json(&body)
    }

    async fn fetch_account(&self, symbol: &str) -> Result<AccountSnapshot, DomainError> {
        self.ensure_credentials()?;
        let query = build_signed_query(
            &[],
            self.recv_window_ms,
            self.clock.now_ms(),
            &self.api_secret,
        );
        let url = format!("{}/fapi/v2/account?{}", self.base_url, query);
        let resp = self
            .http
            .get(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|e| DomainError::ExchangeError {
                message: format!("GET account network error: {}", e),
            })?;

        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| DomainError::ExchangeError {
            message: format!("GET account invalid JSON: {}", e),
        })?;

        if !status.is_success() {
            return Err(DomainError::ExchangeError {
                message: format!("Binance account HTTP {}: {}", status, body),
            });
        }

        parse_binance_account_json(&body, symbol)
    }

    async fn set_leverage(&self, symbol: &str, leverage: u32) -> Result<(), DomainError> {
        self.ensure_credentials()?;
        let params = vec![
            ("symbol", symbol.to_uppercase()),
            ("leverage", leverage.to_string()),
        ];
        let query = build_signed_query(
            &params,
            self.recv_window_ms,
            self.clock.now_ms(),
            &self.api_secret,
        );
        let url = format!("{}/fapi/v1/leverage?{}", self.base_url, query);
        let resp = self
            .http
            .post(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|e| DomainError::ExchangeError {
                message: format!("POST leverage network error: {}", e),
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(DomainError::ExchangeError {
                message: format!("Binance set_leverage error: {}", text),
            });
        }
        Ok(())
    }

    async fn place_order(&self, req: &TradeOrderRequest) -> Result<OrderFill, DomainError> {
        self.ensure_credentials()?;
        let side_str = match req.side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        };
        let mut params = vec![
            ("symbol", req.symbol.to_uppercase()),
            ("side", side_str.to_string()),
            ("type", "MARKET".to_string()),
            ("quantity", format!("{:.3}", req.quantity)),
        ];
        if req.reduce_only {
            params.push(("reduceOnly", "true".to_string()));
        }

        let now_ms = self.clock.now_ms();
        let query = build_signed_query(&params, self.recv_window_ms, now_ms, &self.api_secret);
        let url = format!("{}/fapi/v1/order?{}", self.base_url, query);
        let resp = self
            .http
            .post(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|e| DomainError::ExchangeError {
                message: format!("POST order network error: {}", e),
            })?;

        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| DomainError::ExchangeError {
            message: format!("POST order invalid JSON: {}", e),
        })?;

        if !status.is_success() {
            return Err(DomainError::ExchangeError {
                message: format!("Binance order HTTP {}: {}", status, body),
            });
        }

        let order_id = body["orderId"]
            .as_i64()
            .map(|id| id.to_string())
            .or_else(|| body["orderId"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| format!("bn-{}", now_ms));

        let avg_price = parse_json_f64(&body["avgPrice"], "avgPrice")
            .ok()
            .filter(|&p| p > 0.0)
            .unwrap_or(req.entry_price);

        // Submit protective STOP_MARKET and TAKE_PROFIT_MARKET orders when opening a position
        if !req.reduce_only {
            let exit_side = match req.position_side.exit_order_side() {
                OrderSide::Buy => "BUY",
                OrderSide::Sell => "SELL",
            };
            if let Some(sl) = req.stop_loss {
                let sl_params = vec![
                    ("symbol", req.symbol.to_uppercase()),
                    ("side", exit_side.to_string()),
                    ("type", "STOP_MARKET".to_string()),
                    ("stopPrice", format!("{:.2}", sl)),
                    ("closePosition", "true".to_string()),
                ];
                let sl_q = build_signed_query(
                    &sl_params,
                    self.recv_window_ms,
                    self.clock.now_ms(),
                    &self.api_secret,
                );
                let _ = self
                    .http
                    .post(format!("{}/fapi/v1/order?{}", self.base_url, sl_q))
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
                    .await;
            }
            if let Some(tp) = req.take_profit {
                let tp_params = vec![
                    ("symbol", req.symbol.to_uppercase()),
                    ("side", exit_side.to_string()),
                    ("type", "TAKE_PROFIT_MARKET".to_string()),
                    ("stopPrice", format!("{:.2}", tp)),
                    ("closePosition", "true".to_string()),
                ];
                let tp_q = build_signed_query(
                    &tp_params,
                    self.recv_window_ms,
                    self.clock.now_ms(),
                    &self.api_secret,
                );
                let _ = self
                    .http
                    .post(format!("{}/fapi/v1/order?{}", self.base_url, tp_q))
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
                    .await;
            }
        }

        Ok(OrderFill {
            order_id,
            symbol: req.symbol.to_uppercase(),
            side: req.side,
            position_side: req.position_side,
            executed_qty: req.quantity,
            avg_price,
            stop_loss: req.stop_loss,
            take_profit: req.take_profit,
            realized_pnl: 0.0,
            timestamp_ms: now_ms,
        })
    }

    async fn close_all_positions(&self, symbol: &str) -> Result<Vec<OrderFill>, DomainError> {
        self.ensure_credentials()?;
        let snapshot = self.fetch_account(symbol).await?;
        let mut fills = Vec::new();

        for pos in snapshot.positions {
            let close_req = TradeOrderRequest {
                symbol: pos.symbol.clone(),
                side: pos.side.exit_order_side(),
                position_side: pos.side,
                quantity: pos.quantity,
                entry_price: pos.mark_price,
                stop_loss: None,
                take_profit: None,
                leverage: pos.leverage,
                reduce_only: true,
            };
            let fill = self.place_order(&close_req).await?;
            fills.push(fill);
        }

        Ok(fills)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn should_sign_binance_query_with_deterministic_hmac_sha256() {
        let query = "symbol=BTCUSDT&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=9000&recvWindow=5000&timestamp=1591702613943";
        let secret = "2b5eb11e18796d12d88f13dc27dbbd02c2cc51ff7059765ed9821957d82bb4d9";
        let sig = sign_binance_query(query, secret);
        assert_eq!(
            sig,
            "ec11dcc17e67e47f0d3c3f513dfe9062307e37619c5c82ebaa8fe0bdf3d59519"
        );
    }

    #[test]
    fn should_resolve_binance_base_url_by_mode() {
        let cfg = ExchangeCredentialsConfig::default();
        assert_eq!(
            resolve_binance_base_url(ExecutionMode::Live, &cfg),
            "https://fapi.binance.com"
        );
        assert_eq!(
            resolve_binance_base_url(ExecutionMode::Testnet, &cfg),
            "https://testnet.binancefuture.com"
        );
        assert_eq!(
            resolve_binance_base_url(ExecutionMode::Paper, &cfg),
            "https://testnet.binancefuture.com"
        );
    }

    #[test]
    fn should_build_signed_query_with_timestamp_and_signature() {
        let q = build_signed_query(
            &[("symbol", "BTCUSDT".to_string())],
            5_000,
            1_700_000_000_000,
            "my-secret",
        );
        assert!(q.starts_with("symbol=BTCUSDT&recvWindow=5000&timestamp=1700000000000&signature="));
    }

    #[test]
    fn should_parse_binance_klines_json_and_handle_invalid_payloads() {
        let payload = json!([
            [
                1499040000000i64,
                "50000.50",
                "50100.00",
                "49900.00",
                "50050.25",
                "123.45",
                1499644799999i64,
                "6178703.36",
                308,
                "60.0",
                "3000000.0",
                "0"
            ]
        ]);
        let candles = parse_binance_klines_json(&payload).unwrap();
        assert_eq!(candles.len(), 1);
        assert!((candles[0].open - 50000.50).abs() < 1e-6);
        assert!((candles[0].close - 50050.25).abs() < 1e-6);

        assert!(parse_binance_klines_json(&json!({"error": true})).is_err());
        assert!(parse_binance_klines_json(&json!([[1, 2]])).is_err());
    }

    #[test]
    fn should_parse_binance_account_json_and_filter_non_zero_positions() {
        let payload = json!({
            "totalWalletBalance": "1250.50",
            "availableBalance": "1100.25",
            "totalUnrealizedProfit": "15.75",
            "positions": [
                {
                    "symbol": "BTCUSDT",
                    "positionAmt": "0.025",
                    "entryPrice": "60000.0",
                    "unrealizedProfit": "15.75",
                    "leverage": "5"
                },
                {
                    "symbol": "ETHUSDT",
                    "positionAmt": "0.000",
                    "entryPrice": "0.0",
                    "unrealizedProfit": "0.0",
                    "leverage": "3"
                }
            ]
        });

        let snapshot = parse_binance_account_json(&payload, "BTCUSDT").unwrap();
        assert!((snapshot.total_wallet_balance - 1250.50).abs() < 1e-6);
        assert_eq!(snapshot.positions.len(), 1);
        assert_eq!(snapshot.positions[0].side, PositionSide::Long);
        assert!((snapshot.positions[0].quantity - 0.025).abs() < 1e-6);
        assert_eq!(snapshot.positions[0].leverage, 5);
    }

    struct TestClock;
    impl ClockPort for TestClock {
        fn now_ms(&self) -> i64 {
            1_700_000_000_000
        }
    }

    #[tokio::test]
    async fn should_reject_authenticated_calls_when_credentials_missing() {
        let client = BinanceFuturesClient::new(
            "http://127.0.0.1:9".to_string(),
            "".to_string(),
            "".to_string(),
            5_000,
            TestClock,
        );
        assert!(client.fetch_account("BTCUSDT").await.is_err());
        assert!(client.set_leverage("BTCUSDT", 3).await.is_err());
        assert!(client.close_all_positions("BTCUSDT").await.is_err());
        let req = TradeOrderRequest {
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            position_side: PositionSide::Long,
            quantity: 0.01,
            entry_price: 50_000.0,
            stop_loss: None,
            take_profit: None,
            leverage: 3,
            reduce_only: false,
        };
        assert!(client.place_order(&req).await.is_err());
        assert!(client.fetch_klines("BTCUSDT", "5m", 10).await.is_err());
    }
}
