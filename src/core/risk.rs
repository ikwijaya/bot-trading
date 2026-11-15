use crate::core::domain::{
    AccountSnapshot, BotConfig, BotState, ConfigCheckItem, DomainError, ExecutionMode,
    PositionSide, PositionSizing, ProbabilityAssessment, RiskConfig, SizingMode,
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

/// Evaluates the mathematical Expected Value (EV), Break-Even Win Rate, and Half-Kelly fraction
/// of a trade after accounting for round-trip exchange taker fees (`0.08%`).
///
/// # Parameters
/// - `win_prob`: Estimated win probability (`0.0..=1.0`, typically derived from signal/AI confidence).
/// - `entry_price`: Planned entry price (`> 0.0`).
/// - `stop_loss`: Planned stop-loss price (`> 0.0`).
/// - `take_profit`: Planned take-profit price (`> 0.0`).
/// - `require_positive_ev`: When `true`, rejects trades whose net Expected Value is `<= 0.0` or where `win_prob <= break_even_win_rate`.
///
/// # Returns
/// [`ProbabilityAssessment`] containing win probability, break-even win rate, net R:R, EV (in R), and Half-Kelly fraction.
pub fn evaluate_trade_probability(
    win_prob: f64,
    entry_price: f64,
    stop_loss: f64,
    take_profit: f64,
    require_positive_ev: bool,
) -> Result<ProbabilityAssessment, DomainError> {
    if entry_price <= 0.0 || stop_loss <= 0.0 || take_profit <= 0.0 {
        return Err(DomainError::InvalidConfig {
            field: "entry_price/stop_loss/take_profit",
            reason: "prices must be positive".to_string(),
        });
    }

    let p = win_prob.clamp(0.01, 0.99);
    // Standard round-trip taker fee buffer (0.04% entry + 0.04% exit = 0.08%)
    let fee_pct = 0.0008;
    let gross_reward_pct = (take_profit - entry_price).abs() / entry_price;
    let gross_risk_pct = (entry_price - stop_loss).abs() / entry_price;

    let net_reward_pct = (gross_reward_pct - fee_pct).max(0.0);
    let net_risk_pct = (gross_risk_pct + fee_pct).max(1e-6);
    let net_reward_to_risk = net_reward_pct / net_risk_pct;

    let break_even_win_rate = if net_reward_to_risk > 0.0 {
        1.0 / (1.0 + net_reward_to_risk)
    } else {
        1.0
    };

    let expected_value_r = (p * net_reward_to_risk) - (1.0 - p);
    let full_kelly = if net_reward_to_risk > 0.0 {
        p - ((1.0 - p) / net_reward_to_risk)
    } else {
        0.0
    };
    let half_kelly_fraction = (full_kelly.max(0.0) * 0.5).clamp(0.0, 0.5);
    let is_positive_ev = expected_value_r > 0.0 && p > break_even_win_rate;

    if require_positive_ev && !is_positive_ev {
        return Err(DomainError::RiskLimitExceeded {
            reason: format!(
                "negative/insufficient Expected Value (EV={:+.2}R, win_prob={:.1}% <= break_even={:.1}%, net_RR={:.2})",
                expected_value_r,
                p * 100.0,
                break_even_win_rate * 100.0,
                net_reward_to_risk
            ),
        });
    }

    Ok(ProbabilityAssessment {
        win_probability: p,
        break_even_win_rate,
        net_reward_to_risk,
        expected_value_r,
        half_kelly_fraction,
        is_positive_ev,
    })
}

/// Calculates contract quantity and notional size honoring `SizingMode` (`ProbabilityEv`, `RiskPct`,
/// `MaxBalance`, `FixedMin`) as well as `min_order_usdt` and `max_order_usdt` purchase bounds.
pub fn calculate_position_size_with_policy(
    balance_usdt: f64,
    entry_price: f64,
    stop_loss_price: f64,
    prob: &ProbabilityAssessment,
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

    let stop_distance = (entry_price - stop_loss_price).abs();
    if stop_distance < 1e-8 {
        return Err(DomainError::InvalidConfig {
            field: "stop_loss_price",
            reason: "stop_loss_price cannot be equal to entry_price".to_string(),
        });
    }

    let max_wallet_notional = balance_usdt * (risk_cfg.leverage as f64) * 0.95;
    let ceiling_notional = if risk_cfg.max_order_usdt > 0.0 {
        max_wallet_notional.min(risk_cfg.max_order_usdt)
    } else {
        max_wallet_notional
    };
    let floor_notional = risk_cfg.min_order_usdt.max(0.0);

    if floor_notional > ceiling_notional {
        return Err(DomainError::RiskLimitExceeded {
            reason: format!(
                "min_order_usdt (${:.2}) exceeds maximum affordable/allowed notional (${:.2})",
                floor_notional, ceiling_notional
            ),
        });
    }

    let stop_loss_pct = stop_distance / entry_price;
    let raw_notional = match risk_cfg.sizing_mode {
        SizingMode::ProbabilityEv => {
            // Scale base risk_per_trade_pct dynamically by Half-Kelly edge confidence
            let kelly_scale = (0.5 + prob.half_kelly_fraction * 2.0).clamp(0.5, 1.5);
            let effective_risk_pct =
                (risk_cfg.risk_per_trade_pct * kelly_scale).clamp(0.002, 0.20);
            let risk_amount = balance_usdt * effective_risk_pct;
            risk_amount / stop_loss_pct
        }
        SizingMode::RiskPct => {
            let risk_amount = balance_usdt * risk_cfg.risk_per_trade_pct;
            risk_amount / stop_loss_pct
        }
        SizingMode::MaxBalance => ceiling_notional,
        SizingMode::FixedMin => floor_notional.max(10.0),
    };

    let clamped_notional = raw_notional.clamp(floor_notional, ceiling_notional);
    let raw_qty = clamped_notional / entry_price;
    let quantity = ((raw_qty * 1_000.0).floor() / 1_000.0).max(0.001);
    let notional_usdt = quantity * entry_price;
    let risk_amount_usdt = quantity * stop_distance;

    Ok(PositionSizing {
        quantity,
        notional_usdt,
        risk_amount_usdt,
        leverage: risk_cfg.leverage,
    })
}

/// Validates all configuration parameters and environment key requirements, returning a structured
/// checklist (`Vec<ConfigCheckItem>`) suitable for `trade-bot check-config` and the setup wizard.
pub fn validate_config_requirements(
    cfg: &BotConfig,
    has_exchange_keys: bool,
    has_ai_key: bool,
    has_telegram_creds: bool,
) -> Vec<ConfigCheckItem> {
    let mut checks = Vec::new();

    // 1. Symbol
    let symbol_ok = !cfg.symbol.trim().is_empty();
    checks.push(ConfigCheckItem {
        field: "symbol".to_string(),
        passed: symbol_ok,
        message: if symbol_ok {
            format!("Simbol pasangan kontrak valid: {} (Exchange: {})", cfg.symbol, cfg.exchange.provider)
        } else {
            "Simbol tidak boleh kosong (contoh: BTCUSDT)".to_string()
        },
    });

    // 2. EMA Periods
    let ema_ok = cfg.strategy.fast_ema_period > 0
        && cfg.strategy.fast_ema_period < cfg.strategy.slow_ema_period;
    checks.push(ConfigCheckItem {
        field: "strategy.ema_periods".to_string(),
        passed: ema_ok,
        message: if ema_ok {
            format!(
                "Periode EMA valid: fast={} < slow={}",
                cfg.strategy.fast_ema_period, cfg.strategy.slow_ema_period
            )
        } else {
            format!(
                "fast_ema_period ({}) harus > 0 dan lebih kecil dari slow_ema_period ({})",
                cfg.strategy.fast_ema_period, cfg.strategy.slow_ema_period
            )
        },
    });

    // 3. Kline Limit vs Slow EMA
    let kline_ok = cfg.kline_limit > cfg.strategy.slow_ema_period + 5;
    checks.push(ConfigCheckItem {
        field: "kline_limit".to_string(),
        passed: kline_ok,
        message: if kline_ok {
            format!(
                "Jumlah candlestick ({}) mencukupi untuk kalkulasi indikator (min {})",
                cfg.kline_limit,
                cfg.strategy.slow_ema_period + 6
            )
        } else {
            format!(
                "kline_limit ({}) terlalu sedikit; harus > slow_ema_period + 5 ({})",
                cfg.kline_limit,
                cfg.strategy.slow_ema_period + 5
            )
        },
    });

    // 4. RSI Thresholds
    let rsi_ok = cfg.strategy.rsi_oversold > 0.0
        && cfg.strategy.rsi_oversold < cfg.strategy.rsi_overbought
        && cfg.strategy.rsi_overbought < 100.0;
    checks.push(ConfigCheckItem {
        field: "strategy.rsi_bounds".to_string(),
        passed: rsi_ok,
        message: if rsi_ok {
            format!(
                "Batas RSI valid: oversold={:.1}, overbought={:.1}",
                cfg.strategy.rsi_oversold, cfg.strategy.rsi_overbought
            )
        } else {
            "Batas RSI tidak valid (syarat: 0 < rsi_oversold < rsi_overbought < 100)".to_string()
        },
    });

    // 5. Leverage & Max Leverage
    let lev_ok = cfg.risk.leverage >= 1 && cfg.risk.leverage <= cfg.risk.max_leverage;
    checks.push(ConfigCheckItem {
        field: "risk.leverage".to_string(),
        passed: lev_ok,
        message: if lev_ok {
            format!(
                "Leverage aman: {}x (batas maksimum: {}x)",
                cfg.risk.leverage, cfg.risk.max_leverage
            )
        } else {
            format!(
                "Leverage ({}x) melanggar batas 1x..={}x",
                cfg.risk.leverage, cfg.risk.max_leverage
            )
        },
    });

    // 6. Min & Max Order USDT Bounds
    let bounds_ok = cfg.risk.min_order_usdt > 0.0
        && (cfg.risk.max_order_usdt == 0.0 || cfg.risk.max_order_usdt >= cfg.risk.min_order_usdt);
    checks.push(ConfigCheckItem {
        field: "risk.order_bounds_usdt".to_string(),
        passed: bounds_ok,
        message: if bounds_ok {
            let max_label = if cfg.risk.max_order_usdt <= 0.0 {
                "Sesuai Saldo (Auto)".to_string()
            } else {
                format!("${:.2} USDT", cfg.risk.max_order_usdt)
            };
            format!(
                "Batas pembelian valid: Min=${:.2} USDT, Max={} (Mode: {})",
                cfg.risk.min_order_usdt, max_label, cfg.risk.sizing_mode
            )
        } else {
            format!(
                "min_order_usdt (${:.2}) harus > 0 dan <= max_order_usdt (${:.2})",
                cfg.risk.min_order_usdt, cfg.risk.max_order_usdt
            )
        },
    });

    // 7. Exchange Credentials (Required for Testnet/Live)
    let ex_ok = cfg.mode == ExecutionMode::Paper || has_exchange_keys;
    checks.push(ConfigCheckItem {
        field: "exchange.credentials".to_string(),
        passed: ex_ok,
        message: if cfg.mode == ExecutionMode::Paper {
            "Mode PAPER aktif — tidak memerlukan API Key Exchange untuk simulasi".to_string()
        } else if has_exchange_keys {
            format!(
                "API Key & Secret Exchange ({}) terdeteksi di environment ({}, {})",
                cfg.exchange.provider, cfg.exchange.api_key_env, cfg.exchange.api_secret_env
            )
        } else {
            format!(
                "Mode {} membutuhkan environment variable {} dan {}",
                cfg.mode, cfg.exchange.api_key_env, cfg.exchange.api_secret_env
            )
        },
    });

    // 8. AI Advisor Credentials
    let ai_ok = !cfg.ai.enabled || has_ai_key;
    checks.push(ConfigCheckItem {
        field: "ai.credentials".to_string(),
        passed: ai_ok,
        message: if !cfg.ai.enabled {
            "AI Advisor dinonaktifkan (ai.enabled = false) — bot menggunakan pure teknikal + learned skills".to_string()
        } else if has_ai_key {
            format!(
                "AI Advisor aktif ({}) & API Key ({}) tersedia",
                cfg.ai.model, cfg.ai.api_key_env
            )
        } else {
            format!(
                "ai.enabled = true tetapi environment variable {} belum diset",
                cfg.ai.api_key_env
            )
        },
    });

    // 9. Telegram Credentials
    let tg_ok = !cfg.telegram.enabled || has_telegram_creds;
    checks.push(ConfigCheckItem {
        field: "telegram.credentials".to_string(),
        passed: tg_ok,
        message: if !cfg.telegram.enabled {
            "Notifikasi Telegram dinonaktifkan (telegram.enabled = false)".to_string()
        } else if has_telegram_creds {
            format!(
                "Integrasi Telegram aktif & kredensial ({}, {}) tersedia",
                cfg.telegram.bot_token_env, cfg.telegram.chat_id_env
            )
        } else {
            format!(
                "telegram.enabled = true tetapi {} / {} belum diset di environment",
                cfg.telegram.bot_token_env, cfg.telegram.chat_id_env
            )
        },
    });

    checks
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

    #[test]
    fn should_evaluate_trade_probability_and_reject_negative_ev() {
        // Positive EV: entry 50,000, SL 49,500 (-1%), TP 51,000 (+2%), win_prob = 0.65
        let pos_ev =
            evaluate_trade_probability(0.65, 50_000.0, 49_500.0, 51_000.0, true).unwrap();
        assert!(pos_ev.is_positive_ev);
        assert!(pos_ev.expected_value_r > 0.0);
        assert!(pos_ev.win_probability > pos_ev.break_even_win_rate);
        assert!(pos_ev.half_kelly_fraction > 0.0);

        // Negative EV: poor win_prob (0.25) with 1:1 R:R -> should fail when require_positive_ev = true
        let neg_ev_err = evaluate_trade_probability(0.25, 50_000.0, 49_500.0, 50_500.0, true);
        assert!(matches!(
            neg_ev_err,
            Err(DomainError::RiskLimitExceeded { .. })
        ));

        // When require_positive_ev = false, returns ProbabilityAssessment with is_positive_ev = false
        let neg_ev_allowed =
            evaluate_trade_probability(0.25, 50_000.0, 49_500.0, 50_500.0, false).unwrap();
        assert!(!neg_ev_allowed.is_positive_ev);
    }

    #[test]
    fn should_calculate_position_size_across_sizing_modes_and_min_max_limits() {
        let prob =
            evaluate_trade_probability(0.70, 50_000.0, 49_500.0, 51_000.0, true).unwrap();

        // 1. FixedMin mode -> clamps to min_order_usdt ($100 -> 0.002 BTC)
        let cfg_min = RiskConfig {
            sizing_mode: SizingMode::FixedMin,
            min_order_usdt: 100.0,
            max_order_usdt: 500.0,
            leverage: 3,
            ..RiskConfig::default()
        };
        let sz_min =
            calculate_position_size_with_policy(1_000.0, 50_000.0, 49_500.0, &prob, &cfg_min)
                .unwrap();
        assert!((sz_min.notional_usdt - 100.0).abs() < 1e-6);

        // 2. MaxBalance mode with max_order_usdt cap ($400 -> 0.008 BTC)
        let cfg_max = RiskConfig {
            sizing_mode: SizingMode::MaxBalance,
            min_order_usdt: 50.0,
            max_order_usdt: 400.0,
            leverage: 3,
            ..RiskConfig::default()
        };
        let sz_max =
            calculate_position_size_with_policy(1_000.0, 50_000.0, 49_500.0, &prob, &cfg_max)
                .unwrap();
        assert!((sz_max.notional_usdt - 400.0).abs() < 1e-6);

        // 3. ProbabilityEv mode
        let cfg_prob = RiskConfig {
            sizing_mode: SizingMode::ProbabilityEv,
            min_order_usdt: 50.0,
            max_order_usdt: 2_500.0,
            leverage: 3,
            ..RiskConfig::default()
        };
        let sz_prob =
            calculate_position_size_with_policy(1_000.0, 50_000.0, 49_500.0, &prob, &cfg_prob)
                .unwrap();
        assert!(sz_prob.notional_usdt >= 50.0 && sz_prob.notional_usdt <= 2_500.0);

        // 4. Error when min_order_usdt > max_order_usdt / affordable ceiling
        let cfg_impossible = RiskConfig {
            sizing_mode: SizingMode::ProbabilityEv,
            min_order_usdt: 5_000.0,
            max_order_usdt: 500.0,
            ..RiskConfig::default()
        };
        assert!(matches!(
            calculate_position_size_with_policy(
                1_000.0,
                50_000.0,
                49_500.0,
                &prob,
                &cfg_impossible
            ),
            Err(DomainError::RiskLimitExceeded { .. })
        ));
    }

    #[test]
    fn should_validate_config_requirements_and_detect_invalid_fields() {
        let default_cfg = BotConfig::default();
        let checks = validate_config_requirements(&default_cfg, false, false, false);
        assert_eq!(checks.len(), 9);
        assert!(checks.iter().all(|c| c.passed));

        // Broken config in Live mode without keys and invalid EMA/leverage
        let mut bad_cfg = BotConfig::default();
        bad_cfg.mode = ExecutionMode::Live;
        bad_cfg.symbol = "".to_string();
        bad_cfg.strategy.fast_ema_period = 30;
        bad_cfg.strategy.slow_ema_period = 10;
        bad_cfg.risk.leverage = 50;
        bad_cfg.risk.max_leverage = 10;
        bad_cfg.ai.enabled = true;
        bad_cfg.telegram.enabled = true;

        let bad_checks = validate_config_requirements(&bad_cfg, false, false, false);
        let failed_count = bad_checks.iter().filter(|c| !c.passed).count();
        assert!(failed_count >= 5);
    }
}
