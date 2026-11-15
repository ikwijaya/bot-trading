use crate::core::domain::{
    AccountSnapshot, BotState, DomainError, PositionSide, PositionSizing, RiskConfig,
};

/// Computes ATR-based Stop-Loss and Take-Profit prices for a given entry price and position side.
///
/// # Parameters
/// - `entry_price`: Current market/fill price (`> 0.0`).
/// - `atr`: Latest Average True Range (`> 0.0`).
/// - `side`: [`PositionSide::Long`] or [`PositionSide::Short`].
/// - `risk_cfg`: [`RiskConfig`] containing `atr_sl_multiplier` and `atr_tp_multiplier`.
///
/// # Returns
/// Tuple `(stop_loss_price, take_profit_price)`.
///
/// # Errors
/// Returns [`DomainError::InvalidConfig`] if `entry_price <= 0.0`, `atr <= 0.0`, or multipliers `<= 0.0`.
pub fn compute_sl_tp_prices(
    entry_price: f64,
    atr: f64,
    side: PositionSide,
    risk_cfg: &RiskConfig,
) -> Result<(f64, f64), DomainError> {
    if entry_price <= 0.0 {
        return Err(DomainError::InvalidConfig {
            field: "entry_price",
            reason: "entry_price must be positive".to_string(),
        });
    }
    if atr <= 0.0 {
        return Err(DomainError::InvalidConfig {
            field: "atr",
            reason: "atr must be positive".to_string(),
        });
    }
    if risk_cfg.atr_sl_multiplier <= 0.0 || risk_cfg.atr_tp_multiplier <= 0.0 {
        return Err(DomainError::InvalidConfig {
            field: "atr_sl_multiplier/atr_tp_multiplier",
            reason: "ATR multipliers must be positive".to_string(),
        });
    }

    let sl_offset = atr * risk_cfg.atr_sl_multiplier;
    let tp_offset = atr * risk_cfg.atr_tp_multiplier;

    let (sl, tp) = match side {
        PositionSide::Long => {
            let sl = (entry_price - sl_offset).max(entry_price * 0.01);
            let tp = entry_price + tp_offset;
            (sl, tp)
        }
        PositionSide::Short => {
            let sl = entry_price + sl_offset;
            let tp = (entry_price - tp_offset).max(entry_price * 0.01);
            (sl, tp)
        }
    };

    Ok(((sl * 100.0).round() / 100.0, (tp * 100.0).round() / 100.0))
}

/// Calculates contract quantity and notional size based on account balance, stop-loss distance, and leverage cap.
///
/// # Parameters
/// - `balance_usdt`: Available wallet balance in USDT (`> 0.0`).
/// - `entry_price`: Planned entry price (`> 0.0`).
/// - `stop_loss_price`: Planned stop-loss price (`> 0.0`, `!= entry_price`).
/// - `risk_cfg`: [`RiskConfig`] parameters.
///
/// # Returns
/// [`PositionSizing`] with rounded contract quantity, notional value, risk amount, and leverage.
pub fn calculate_position_size(
    balance_usdt: f64,
    entry_price: f64,
    stop_loss_price: f64,
    risk_cfg: &RiskConfig,
) -> Result<PositionSizing, DomainError> {
    if balance_usdt <= 0.0 {
        return Err(DomainError::RiskLimitExceeded {
            reason: "available USDT balance must be positive".to_string(),
        });
    }
    if entry_price <= 0.0 || stop_loss_price <= 0.0 {
        return Err(DomainError::InvalidConfig {
            field: "entry_price/stop_loss_price",
            reason: "entry and stop-loss prices must be positive".to_string(),
        });
    }
    if risk_cfg.leverage == 0 || risk_cfg.leverage > risk_cfg.max_leverage {
        return Err(DomainError::RiskLimitExceeded {
            reason: format!(
                "configured leverage {}x exceeds allowed range 1x..={}x",
                risk_cfg.leverage, risk_cfg.max_leverage
            ),
        });
    }
    if risk_cfg.risk_per_trade_pct <= 0.0 || risk_cfg.risk_per_trade_pct > 1.0 {
        return Err(DomainError::InvalidConfig {
            field: "risk_per_trade_pct",
            reason: "risk_per_trade_pct must be within (0.0, 1.0]".to_string(),
        });
    }

    let stop_distance = (entry_price - stop_loss_price).abs();
    if stop_distance < 1e-8 {
        return Err(DomainError::InvalidConfig {
            field: "stop_loss_price",
            reason: "stop_loss_price cannot be equal to entry_price".to_string(),
        });
    }

    let risk_amount_usdt = balance_usdt * risk_cfg.risk_per_trade_pct;
    let raw_qty = risk_amount_usdt / stop_distance;
    let max_notional = balance_usdt * (risk_cfg.leverage as f64) * 0.95;
    let capped_notional = (raw_qty * entry_price).min(max_notional);
    let capped_qty = capped_notional / entry_price;

    // Round down to 3 decimal places (standard BTC/ETH USDⓈ-M precision)
    let quantity = ((capped_qty * 1_000.0).floor() / 1_000.0).max(0.001);
    let notional_usdt = quantity * entry_price;

    Ok(PositionSizing {
        quantity,
        notional_usdt,
        risk_amount_usdt,
        leverage: risk_cfg.leverage,
    })
}

/// Checks whether the current account equity has breached the maximum daily drawdown percentage.
///
/// # Parameters
/// - `day_start_balance`: Starting wallet balance at the beginning of the trading day (`> 0.0`).
/// - `current_equity`: Current total wallet balance plus unrealized PnL.
/// - `max_drawdown_pct`: Maximum allowed drawdown ratio (e.g. `0.05` for 5%).
///
/// # Returns
/// Current drawdown ratio (`0.0..max_drawdown_pct`).
///
/// # Errors
/// Returns [`DomainError::RiskLimitExceeded`] if drawdown meets or exceeds `max_drawdown_pct`.
pub fn check_daily_drawdown(
    day_start_balance: f64,
    current_equity: f64,
    max_drawdown_pct: f64,
) -> Result<f64, DomainError> {
    if day_start_balance <= 0.0 {
        return Err(DomainError::InvalidConfig {
            field: "day_start_balance",
            reason: "day_start_balance must be positive".to_string(),
        });
    }
    if max_drawdown_pct <= 0.0 || max_drawdown_pct >= 1.0 {
        return Err(DomainError::InvalidConfig {
            field: "max_daily_drawdown_pct",
            reason: "max_daily_drawdown_pct must be between 0.0 and 1.0".to_string(),
        });
    }

    let drawdown = if current_equity < day_start_balance {
        (day_start_balance - current_equity) / day_start_balance
    } else {
        0.0
    };

    if drawdown >= max_drawdown_pct {
        return Err(DomainError::RiskLimitExceeded {
            reason: format!(
                "daily drawdown {:.2}% reached circuit breaker limit {:.2}%",
                drawdown * 100.0,
                max_drawdown_pct * 100.0
            ),
        });
    }

    Ok(drawdown)
}

/// Validates all pre-trade risk guardrails (leverage cap, daily drawdown circuit breaker, and max open positions).
pub fn validate_risk_limits(
    state: &BotState,
    account: &AccountSnapshot,
    risk_cfg: &RiskConfig,
) -> Result<(), DomainError> {
    if risk_cfg.leverage == 0 || risk_cfg.leverage > risk_cfg.max_leverage {
        return Err(DomainError::RiskLimitExceeded {
            reason: format!(
                "leverage {}x exceeds max_leverage {}x",
                risk_cfg.leverage, risk_cfg.max_leverage
            ),
        });
    }

    let current_equity = account.total_wallet_balance + account.unrealized_pnl;
    check_daily_drawdown(
        state.day_start_balance_usdt,
        current_equity,
        risk_cfg.max_daily_drawdown_pct,
    )?;

    if account.positions.len() >= risk_cfg.max_open_positions {
        return Err(DomainError::RiskLimitExceeded {
            reason: format!(
                "open positions ({}) reached max_open_positions ({})",
                account.positions.len(),
                risk_cfg.max_open_positions
            ),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::{BotConfig, Position};

    #[test]
    fn should_compute_sl_tp_prices_for_long_and_short() {
        let cfg = RiskConfig {
            atr_sl_multiplier: 1.5,
            atr_tp_multiplier: 3.0,
            ..RiskConfig::default()
        };

        let (sl_long, tp_long) =
            compute_sl_tp_prices(50_000.0, 200.0, PositionSide::Long, &cfg).unwrap();
        assert!((sl_long - 49_700.0).abs() < 1e-6);
        assert!((tp_long - 50_600.0).abs() < 1e-6);

        let (sl_short, tp_short) =
            compute_sl_tp_prices(50_000.0, 200.0, PositionSide::Short, &cfg).unwrap();
        assert!((sl_short - 50_300.0).abs() < 1e-6);
        assert!((tp_short - 49_400.0).abs() < 1e-6);
    }

    #[test]
    fn should_reject_invalid_sl_tp_inputs() {
        let cfg = RiskConfig::default();
        assert!(matches!(
            compute_sl_tp_prices(0.0, 100.0, PositionSide::Long, &cfg),
            Err(DomainError::InvalidConfig { .. })
        ));
        assert!(matches!(
            compute_sl_tp_prices(50_000.0, -5.0, PositionSide::Long, &cfg),
            Err(DomainError::InvalidConfig { .. })
        ));
        let bad_cfg = RiskConfig {
            atr_sl_multiplier: 0.0,
            ..cfg
        };
        assert!(matches!(
            compute_sl_tp_prices(50_000.0, 100.0, PositionSide::Long, &bad_cfg),
            Err(DomainError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn should_calculate_position_size_and_respect_leverage_cap() {
        let cfg = RiskConfig {
            leverage: 3,
            max_leverage: 10,
            risk_per_trade_pct: 0.02, // $20 risk on $1000
            ..RiskConfig::default()
        };

        // Stop distance = $200 -> raw qty = 20 / 200 = 0.1 BTC ($5000 notional),
        // but 3x leverage on $1000 caps notional at $1000 * 3 * 0.95 = $2850 -> 0.057 BTC
        let sizing = calculate_position_size(1_000.0, 50_000.0, 49_800.0, &cfg).unwrap();
        assert!((sizing.quantity - 0.057).abs() < 1e-6);
        assert!(sizing.notional_usdt <= 3_000.0);
        assert_eq!(sizing.leverage, 3);
    }

    #[test]
    fn should_reject_position_size_on_invalid_balance_or_leverage() {
        let cfg = RiskConfig::default();
        assert!(matches!(
            calculate_position_size(0.0, 50_000.0, 49_500.0, &cfg),
            Err(DomainError::RiskLimitExceeded { .. })
        ));
        assert!(matches!(
            calculate_position_size(1_000.0, 50_000.0, 50_000.0, &cfg),
            Err(DomainError::InvalidConfig { .. })
        ));
        let bad_lev = RiskConfig {
            leverage: 25,
            max_leverage: 10,
            ..cfg
        };
        assert!(matches!(
            calculate_position_size(1_000.0, 50_000.0, 49_500.0, &bad_lev),
            Err(DomainError::RiskLimitExceeded { .. })
        ));
    }

    #[test]
    fn should_enforce_daily_drawdown_and_validate_risk_limits() {
        let bot_cfg = BotConfig::default();
        let state = BotState::new(&bot_cfg); // day_start_balance = 1000.0
        let healthy_account = AccountSnapshot {
            total_wallet_balance: 980.0,
            available_balance: 980.0,
            unrealized_pnl: 0.0,
            positions: vec![],
        };
        assert!(validate_risk_limits(&state, &healthy_account, &bot_cfg.risk).is_ok());

        // Breached 5% daily drawdown ($940 equity vs $1000 start = 6% drawdown)
        let drawdown_account = AccountSnapshot {
            total_wallet_balance: 940.0,
            available_balance: 940.0,
            unrealized_pnl: 0.0,
            positions: vec![],
        };
        assert!(matches!(
            validate_risk_limits(&state, &drawdown_account, &bot_cfg.risk),
            Err(DomainError::RiskLimitExceeded { .. })
        ));

        // Max open positions reached
        let full_account = AccountSnapshot {
            total_wallet_balance: 1_000.0,
            available_balance: 900.0,
            unrealized_pnl: 5.0,
            positions: vec![Position {
                symbol: "BTCUSDT".to_string(),
                side: PositionSide::Long,
                quantity: 0.01,
                entry_price: 50_000.0,
                mark_price: 50_500.0,
                unrealized_pnl: 5.0,
                leverage: 3,
                stop_loss: Some(49_500.0),
                take_profit: Some(51_000.0),
            }],
        };
        assert!(matches!(
            validate_risk_limits(&state, &full_account, &bot_cfg.risk),
            Err(DomainError::RiskLimitExceeded { .. })
        ));
    }
}
