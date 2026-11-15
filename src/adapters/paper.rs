use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::adapters::binance::parse_binance_klines_json;
use crate::core::domain::{
    AccountSnapshot, BotConfig, BotState, Candle, DomainError, OrderFill, Position, PositionSide,
    TradeOrderRequest,
};
use crate::core::ports::{ClockPort, ExchangePort, StateStorePort};

/// Generates deterministic synthetic OHLCV candles when running in offline Paper Trading mode.
pub fn generate_synthetic_candles(limit: usize, now_ms: i64) -> Vec<Candle> {
    let count = limit.max(30);
    let step_ms = 300_000i64; // 5m
    let start_ms = now_ms - (count as i64) * step_ms;

    // Gentle upward trend with pullbacks so EMA/RSI/ATR produce realistic signals (~60-65 RSI)
    (0..count)
        .map(|i| {
            let pullbacks = (i / 3) as f64;
            let base = 65_000.0 + (i as f64) * 30.0 - pullbacks * 45.0;
            let wave = if i % 3 == 1 { -25.0 } else { 20.0 };
            let close = base + wave;
            let open = close - 12.0;
            let high = close + 45.0;
            let low = close - 45.0;
            let open_time = start_ms + (i as i64) * step_ms;
            Candle {
                open_time,
                open,
                high,
                low,
                close,
                volume: 120.0 + (i as f64),
                close_time: open_time + step_ms - 1,
            }
        })
        .collect()
}

/// Evaluates open paper positions against the latest candle to trigger Stop-Loss or Take-Profit fills.
pub fn evaluate_paper_sl_tp(
    positions: &mut Vec<Position>,
    wallet_balance: &mut f64,
    latest_candle: &Candle,
    now_ms: i64,
) -> Vec<OrderFill> {
    let mut fills = Vec::new();
    let mut remaining = Vec::with_capacity(positions.len());

    for mut pos in positions.drain(..) {
        pos.mark_price = latest_candle.close;
        pos.unrealized_pnl = pos.compute_unrealized_pnl(latest_candle.close);

        let mut trigger_price: Option<f64> = None;
        match pos.side {
            PositionSide::Long => {
                if let Some(sl) = pos.stop_loss {
                    if latest_candle.low <= sl {
                        trigger_price = Some(sl);
                    }
                }
                if trigger_price.is_none() {
                    if let Some(tp) = pos.take_profit {
                        if latest_candle.high >= tp {
                            trigger_price = Some(tp);
                        }
                    }
                }
            }
            PositionSide::Short => {
                if let Some(sl) = pos.stop_loss {
                    if latest_candle.high >= sl {
                        trigger_price = Some(sl);
                    }
                }
                if trigger_price.is_none() {
                    if let Some(tp) = pos.take_profit {
                        if latest_candle.low <= tp {
                            trigger_price = Some(tp);
                        }
                    }
                }
            }
        }

        if let Some(exit_price) = trigger_price {
            let pnl = pos.compute_unrealized_pnl(exit_price);
            *wallet_balance += pnl;
            fills.push(OrderFill {
                order_id: format!("paper-sltp-{}", now_ms),
                symbol: pos.symbol.clone(),
                side: pos.side.exit_order_side(),
                position_side: pos.side,
                executed_qty: pos.quantity,
                avg_price: exit_price,
                stop_loss: None,
                take_profit: None,
                realized_pnl: pnl,
                timestamp_ms: now_ms,
            });
        } else {
            remaining.push(pos);
        }
    }

    *positions = remaining;
    fills
}

struct PaperMemory {
    balance_usdt: f64,
    leverage: u32,
    positions: Vec<Position>,
    order_seq: u64,
}

/// Paper Trading simulator adapter implementing [`ExchangePort`].
pub struct PaperExchangeAdapter<S, C> {
    http: reqwest::Client,
    public_base_url: String,
    state_store: Arc<S>,
    clock: C,
    memory: Mutex<PaperMemory>,
}

impl<S: StateStorePort, C: ClockPort> PaperExchangeAdapter<S, C> {
    /// Creates a new [`PaperExchangeAdapter`] seeded from existing state or initial config.
    pub fn new(cfg: &BotConfig, state_store: Arc<S>, clock: C) -> Self {
        let existing = state_store
            .load_state()
            .ok()
            .flatten()
            .unwrap_or_else(|| BotState::new(cfg));

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        Self {
            http,
            public_base_url: cfg
                .exchange
                .live_base_url
                .trim_end_matches('/')
                .to_string(),
            state_store,
            clock,
            memory: Mutex::new(PaperMemory {
                balance_usdt: existing.current_balance_usdt,
                leverage: cfg.risk.leverage,
                positions: existing.open_positions,
                order_seq: existing.total_trades,
            }),
        }
    }

    async fn try_fetch_public_klines(
        &self,
        symbol: &str,
        interval: &str,
        limit: usize,
    ) -> Option<Vec<Candle>> {
        let url = format!(
            "{}/fapi/v1/klines?symbol={}&interval={}&limit={}",
            self.public_base_url,
            symbol.to_uppercase(),
            interval,
            limit
        );
        let resp = self.http.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let val: serde_json::Value = resp.json().await.ok()?;
        parse_binance_klines_json(&val).ok()
    }
}

#[async_trait]
impl<S: StateStorePort, C: ClockPort> ExchangePort for PaperExchangeAdapter<S, C> {
    async fn fetch_klines(
        &self,
        symbol: &str,
        interval: &str,
        limit: usize,
    ) -> Result<Vec<Candle>, DomainError> {
        let now_ms = self.clock.now_ms();
        let candles = self
            .try_fetch_public_klines(symbol, interval, limit)
            .await
            .unwrap_or_else(|| generate_synthetic_candles(limit, now_ms));

        if let Some(latest) = candles.last() {
            let mut mem = self.memory.lock().unwrap();
            let mut positions = std::mem::take(&mut mem.positions);
            let mut balance = mem.balance_usdt;
            let _ = evaluate_paper_sl_tp(&mut positions, &mut balance, latest, now_ms);
            mem.positions = positions;
            mem.balance_usdt = balance;
        }

        Ok(candles)
    }

    async fn fetch_account(&self, _symbol: &str) -> Result<AccountSnapshot, DomainError> {
        let mem = self.memory.lock().unwrap();
        let unrealized_pnl: f64 = mem.positions.iter().map(|p| p.unrealized_pnl).sum();
        let used_margin: f64 = mem
            .positions
            .iter()
            .map(|p| (p.quantity * p.entry_price) / (p.leverage.max(1) as f64))
            .sum();
        let available_balance = (mem.balance_usdt - used_margin).max(0.0);

        Ok(AccountSnapshot {
            total_wallet_balance: mem.balance_usdt,
            available_balance,
            unrealized_pnl,
            positions: mem.positions.clone(),
        })
    }

    async fn set_leverage(&self, _symbol: &str, leverage: u32) -> Result<(), DomainError> {
        let mut mem = self.memory.lock().unwrap();
        mem.leverage = leverage;
        Ok(())
    }

    async fn place_order(&self, req: &TradeOrderRequest) -> Result<OrderFill, DomainError> {
        let now_ms = self.clock.now_ms();
        let mut mem = self.memory.lock().unwrap();
        mem.order_seq += 1;
        let order_id = format!("paper-{}", mem.order_seq);

        mem.positions.push(Position {
            symbol: req.symbol.to_uppercase(),
            side: req.position_side,
            quantity: req.quantity,
            entry_price: req.entry_price,
            mark_price: req.entry_price,
            unrealized_pnl: 0.0,
            leverage: req.leverage,
            stop_loss: req.stop_loss,
            take_profit: req.take_profit,
        });

        Ok(OrderFill {
            order_id,
            symbol: req.symbol.to_uppercase(),
            side: req.side,
            position_side: req.position_side,
            executed_qty: req.quantity,
            avg_price: req.entry_price,
            stop_loss: req.stop_loss,
            take_profit: req.take_profit,
            realized_pnl: 0.0,
            timestamp_ms: now_ms,
        })
    }

    async fn close_all_positions(&self, symbol: &str) -> Result<Vec<OrderFill>, DomainError> {
        let now_ms = self.clock.now_ms();
        let mut mem = self.memory.lock().unwrap();
        let mut fills = Vec::new();
        let mut remaining = Vec::new();
        let old_positions = std::mem::take(&mut mem.positions);

        for pos in old_positions {
            if pos.symbol.eq_ignore_ascii_case(symbol) || symbol.is_empty() {
                mem.order_seq += 1;
                let order_id = format!("paper-close-{}", mem.order_seq);
                let pnl = pos.compute_unrealized_pnl(pos.mark_price);
                mem.balance_usdt += pnl;
                fills.push(OrderFill {
                    order_id,
                    symbol: pos.symbol.clone(),
                    side: pos.side.exit_order_side(),
                    position_side: pos.side,
                    executed_qty: pos.quantity,
                    avg_price: pos.mark_price,
                    stop_loss: None,
                    take_profit: None,
                    realized_pnl: pnl,
                    timestamp_ms: now_ms,
                });
            } else {
                remaining.push(pos);
            }
        }
        mem.positions = remaining;

        if let Ok(Some(mut st)) = self.state_store.load_state() {
            st.current_balance_usdt = mem.balance_usdt;
            st.open_positions = mem.positions.clone();
            for fill in &fills {
                st.realized_pnl_usdt += fill.realized_pnl;
                st.total_trades += 1;
                st.recent_fills.push(fill.clone());
            }
            let _ = self.state_store.save_state(&st);
        }

        Ok(fills)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::OrderSide;

    struct DummyStore;
    impl StateStorePort for DummyStore {
        fn load_state(&self) -> Result<Option<BotState>, DomainError> {
            Ok(None)
        }
        fn save_state(&self, _state: &BotState) -> Result<(), DomainError> {
            Ok(())
        }
    }

    struct DummyClock;
    impl ClockPort for DummyClock {
        fn now_ms(&self) -> i64 {
            1_700_000_000_000
        }
    }

    #[test]
    fn should_generate_synthetic_candles_with_expected_length() {
        let candles = generate_synthetic_candles(50, 1_700_000_000_000);
        assert_eq!(candles.len(), 50);
        assert!(candles.last().unwrap().close > candles.first().unwrap().close);
    }

    #[test]
    fn should_trigger_paper_stop_loss_and_take_profit_correctly() {
        let mut wallet = 1_000.0;
        let mut positions = vec![
            Position {
                symbol: "BTCUSDT".to_string(),
                side: PositionSide::Long,
                quantity: 0.1,
                entry_price: 50_000.0,
                mark_price: 50_000.0,
                unrealized_pnl: 0.0,
                leverage: 3,
                stop_loss: Some(49_500.0),
                take_profit: Some(51_000.0),
            },
            Position {
                symbol: "BTCUSDT".to_string(),
                side: PositionSide::Short,
                quantity: 0.1,
                entry_price: 50_000.0,
                mark_price: 50_000.0,
                unrealized_pnl: 0.0,
                leverage: 3,
                stop_loss: Some(50_500.0),
                take_profit: Some(49_000.0),
            },
        ];

        // Candle hits 51,200 high -> triggers Long TP (+100 USDT) and Short SL (-50 USDT)
        let candle = Candle {
            open_time: 1,
            open: 50_000.0,
            high: 51_200.0,
            low: 49_900.0,
            close: 51_100.0,
            volume: 10.0,
            close_time: 2,
        };

        let fills = evaluate_paper_sl_tp(&mut positions, &mut wallet, &candle, 100);
        assert_eq!(fills.len(), 2);
        assert!(positions.is_empty());
        assert!((wallet - 1_050.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn should_place_and_close_paper_orders() {
        let mut cfg = BotConfig::default();
        // Force unreachable localhost port so it uses synthetic candles immediately without network wait
        cfg.exchange.live_base_url = "http://127.0.0.1:9".to_string();

        let adapter = PaperExchangeAdapter::new(&cfg, Arc::new(DummyStore), DummyClock);
        let candles = adapter.fetch_klines("BTCUSDT", "5m", 35).await.unwrap();
        assert_eq!(candles.len(), 35);

        adapter.set_leverage("BTCUSDT", 5).await.unwrap();
        let fill = adapter
            .place_order(&TradeOrderRequest {
                symbol: "BTCUSDT".to_string(),
                side: OrderSide::Buy,
                position_side: PositionSide::Long,
                quantity: 0.01,
                entry_price: 65_000.0,
                stop_loss: Some(64_000.0),
                take_profit: Some(67_000.0),
                leverage: 5,
                reduce_only: false,
            })
            .await
            .unwrap();
        assert_eq!(fill.executed_qty, 0.01);

        let acct = adapter.fetch_account("BTCUSDT").await.unwrap();
        assert_eq!(acct.positions.len(), 1);

        let closed = adapter.close_all_positions("BTCUSDT").await.unwrap();
        assert_eq!(closed.len(), 1);
        let acct_after = adapter.fetch_account("BTCUSDT").await.unwrap();
        assert!(acct_after.positions.is_empty());
    }
}
