use crate::core::domain::{Candle, DomainError, SignalAction, StrategyConfig, TechnicalSignal};

/// Computes the Exponential Moving Average (EMA) series for a slice of prices.
///
/// # Parameters
/// - `prices`: Chronological slice of closing prices.
/// - `period`: EMA lookback window (`>= 1`).
///
/// # Returns
/// Vector of EMA values starting from index `period - 1` of the input slice.
///
/// # Errors
/// - [`DomainError::InvalidConfig`] if `period == 0`.
/// - [`DomainError::InsufficientData`] if `prices.len() < period`.
///
/// # Example
/// ```
/// use trade_bot::core::indicators::calculate_ema;
/// let ema = calculate_ema(&[10.0, 11.0, 12.0, 13.0], 2).unwrap();
/// assert_eq!(ema.len(), 3);
/// ```
pub fn calculate_ema(prices: &[f64], period: usize) -> Result<Vec<f64>, DomainError> {
    if period == 0 {
        return Err(DomainError::InvalidConfig {
            field: "period",
            reason: "EMA period must be greater than 0".to_string(),
        });
    }
    if prices.len() < period {
        return Err(DomainError::InsufficientData {
            indicator: "EMA",
            required: period,
            actual: prices.len(),
        });
    }

    let initial_sma: f64 = prices[..period].iter().sum::<f64>() / (period as f64);
    let multiplier = 2.0 / ((period as f64) + 1.0);

    let mut result = Vec::with_capacity(prices.len() - period + 1);
    let mut prev_ema = initial_sma;
    result.push(prev_ema);

    for &price in &prices[period..] {
        let next_ema = (price - prev_ema) * multiplier + prev_ema;
        result.push(next_ema);
        prev_ema = next_ema;
    }

    Ok(result)
}

/// Computes the latest Relative Strength Index (RSI) using Wilder's smoothing method.
///
/// # Parameters
/// - `prices`: Chronological slice of closing prices.
/// - `period`: RSI lookback period (`>= 1`).
///
/// # Returns
/// Latest RSI value in the range `0.0..=100.0`.
///
/// # Errors
/// - [`DomainError::InvalidConfig`] if `period == 0`.
/// - [`DomainError::InsufficientData`] if `prices.len() < period + 1`.
pub fn calculate_rsi(prices: &[f64], period: usize) -> Result<f64, DomainError> {
    if period == 0 {
        return Err(DomainError::InvalidConfig {
            field: "rsi_period",
            reason: "RSI period must be greater than 0".to_string(),
        });
    }
    if prices.len() < period + 1 {
        return Err(DomainError::InsufficientData {
            indicator: "RSI",
            required: period + 1,
            actual: prices.len(),
        });
    }

    let mut gains = 0.0;
    let mut losses = 0.0;

    for i in 1..=period {
        let delta = prices[i] - prices[i - 1];
        if delta >= 0.0 {
            gains += delta;
        } else {
            losses -= delta;
        }
    }

    let period_f = period as f64;
    let mut avg_gain = gains / period_f;
    let mut avg_loss = losses / period_f;

    for i in (period + 1)..prices.len() {
        let delta = prices[i] - prices[i - 1];
        let (gain, loss) = if delta >= 0.0 {
            (delta, 0.0)
        } else {
            (0.0, -delta)
        };
        avg_gain = ((avg_gain * (period_f - 1.0)) + gain) / period_f;
        avg_loss = ((avg_loss * (period_f - 1.0)) + loss) / period_f;
    }

    if avg_loss == 0.0 {
        if avg_gain == 0.0 {
            return Ok(50.0);
        }
        return Ok(100.0);
    }

    let rs = avg_gain / avg_loss;
    Ok(100.0 - (100.0 / (1.0 + rs)))
}

/// Computes the latest Average True Range (ATR) using Wilder's smoothing method.
///
/// # Parameters
/// - `candles`: Chronological slice of OHLCV candles.
/// - `period`: ATR lookback window (`>= 1`).
///
/// # Returns
/// Latest ATR value (`>= 0.0`).
///
/// # Errors
/// - [`DomainError::InvalidConfig`] if `period == 0`.
/// - [`DomainError::InsufficientData`] if `candles.len() < period + 1`.
pub fn calculate_atr(candles: &[Candle], period: usize) -> Result<f64, DomainError> {
    if period == 0 {
        return Err(DomainError::InvalidConfig {
            field: "atr_period",
            reason: "ATR period must be greater than 0".to_string(),
        });
    }
    if candles.len() < period + 1 {
        return Err(DomainError::InsufficientData {
            indicator: "ATR",
            required: period + 1,
            actual: candles.len(),
        });
    }

    let mut tr_values = Vec::with_capacity(candles.len() - 1);
    for i in 1..candles.len() {
        let current = &candles[i];
        let prev_close = candles[i - 1].close;
        let hl = current.high - current.low;
        let hc = (current.high - prev_close).abs();
        let lc = (current.low - prev_close).abs();
        let tr = hl.max(hc).max(lc);
        tr_values.push(tr);
    }

    let period_f = period as f64;
    let mut atr = tr_values[..period].iter().sum::<f64>() / period_f;
    for &tr in &tr_values[period..] {
        atr = ((atr * (period_f - 1.0)) + tr) / period_f;
    }

    Ok(atr)
}

/// Evaluates a slice of candles into a [`TechnicalSignal`] using EMA trend, RSI filter, and ATR volatility.
///
/// # Parameters
/// - `candles`: Chronological slice of OHLCV candles.
/// - `cfg`: [`StrategyConfig`] thresholds and lookback periods.
///
/// # Returns
/// [`TechnicalSignal`] containing the recommended directional action, indicator readings, and confidence score.
pub fn evaluate_technical_snapshot(
    candles: &[Candle],
    cfg: &StrategyConfig,
) -> Result<TechnicalSignal, DomainError> {
    if cfg.fast_ema_period == 0 || cfg.slow_ema_period <= cfg.fast_ema_period {
        return Err(DomainError::InvalidConfig {
            field: "fast_ema_period/slow_ema_period",
            reason: "fast_ema_period must be > 0 and strictly less than slow_ema_period"
                .to_string(),
        });
    }

    let prices: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let fast_series = calculate_ema(&prices, cfg.fast_ema_period)?;
    let slow_series = calculate_ema(&prices, cfg.slow_ema_period)?;
    let rsi = calculate_rsi(&prices, cfg.rsi_period)?;
    let atr = calculate_atr(candles, cfg.atr_period)?;

    let fast_ema = *fast_series.last().unwrap_or(&0.0);
    let slow_ema = *slow_series.last().unwrap_or(&0.0);
    let last_price = *prices.last().unwrap_or(&0.0);

    let spread_pct = if slow_ema.abs() > f64::EPSILON {
        (fast_ema - slow_ema).abs() / slow_ema
    } else {
        0.0
    };

    // Scale confidence smoothly from trend strength + RSI alignment
    let trend_strength = (spread_pct * 200.0).clamp(0.0, 0.35);

    if fast_ema > slow_ema && rsi < cfg.rsi_overbought && rsi >= 45.0 && last_price >= slow_ema {
        let rsi_headroom = ((cfg.rsi_overbought - rsi) / (cfg.rsi_overbought - 45.0)).clamp(0.0, 1.0);
        let confidence = (0.55 + trend_strength + 0.15 * rsi_headroom).clamp(0.0, 0.98);
        Ok(TechnicalSignal {
            action: SignalAction::Long,
            fast_ema,
            slow_ema,
            rsi,
            atr,
            last_price,
            confidence,
            reason: format!(
                "Bullish trend (EMA{}={:.2} > EMA{}={:.2}, RSI={:.1}, ATR={:.2})",
                cfg.fast_ema_period, fast_ema, cfg.slow_ema_period, slow_ema, rsi, atr
            ),
        })
    } else if fast_ema < slow_ema && rsi > cfg.rsi_oversold && rsi <= 55.0 && last_price <= slow_ema
    {
        let rsi_headroom = ((55.0 - rsi) / (55.0 - cfg.rsi_oversold)).clamp(0.0, 1.0);
        let confidence = (0.55 + trend_strength + 0.15 * rsi_headroom).clamp(0.0, 0.98);
        Ok(TechnicalSignal {
            action: SignalAction::Short,
            fast_ema,
            slow_ema,
            rsi,
            atr,
            last_price,
            confidence,
            reason: format!(
                "Bearish trend (EMA{}={:.2} < EMA{}={:.2}, RSI={:.1}, ATR={:.2})",
                cfg.fast_ema_period, fast_ema, cfg.slow_ema_period, slow_ema, rsi, atr
            ),
        })
    } else {
        Ok(TechnicalSignal {
            action: SignalAction::Hold,
            fast_ema,
            slow_ema,
            rsi,
            atr,
            last_price,
            confidence: 0.40,
            reason: format!(
                "Neutral / filtered (EMA{}={:.2}, EMA{}={:.2}, RSI={:.1}, ATR={:.2})",
                cfg.fast_ema_period, fast_ema, cfg.slow_ema_period, slow_ema, rsi, atr
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_candles_from_closes(closes: &[f64]) -> Vec<Candle> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &close)| Candle {
                open_time: (i as i64) * 60_000,
                open: close - 2.0,
                high: close + 5.0,
                low: close - 5.0,
                close,
                volume: 100.0,
                close_time: ((i as i64) + 1) * 60_000 - 1,
            })
            .collect()
    }

    #[test]
    fn should_calculate_ema_correctly_for_valid_prices() {
        let prices = vec![10.0, 12.0, 14.0, 16.0, 18.0];
        let ema = calculate_ema(&prices, 3).expect("EMA calculation should succeed");
        assert_eq!(ema.len(), 3);
        // Initial SMA of [10, 12, 14] = 12.0
        assert!((ema[0] - 12.0).abs() < 1e-6);
        // Multiplier = 2 / (3 + 1) = 0.5 -> next = (16 - 12)*0.5 + 12 = 14.0
        assert!((ema[1] - 14.0).abs() < 1e-6);
        assert!((ema[2] - 16.0).abs() < 1e-6);
    }

    #[test]
    fn should_reject_ema_when_period_zero_or_insufficient_data() {
        assert!(matches!(
            calculate_ema(&[10.0, 11.0], 0),
            Err(DomainError::InvalidConfig { .. })
        ));
        assert!(matches!(
            calculate_ema(&[10.0, 11.0], 5),
            Err(DomainError::InsufficientData {
                indicator: "EMA",
                required: 5,
                actual: 2
            })
        ));
    }

    #[test]
    fn should_calculate_rsi_for_uptrend_downtrend_and_flat() {
        let rising: Vec<f64> = (0..20).map(|i| 100.0 + (i as f64) * 2.0).collect();
        let rsi_up = calculate_rsi(&rising, 14).unwrap();
        assert!((rsi_up - 100.0).abs() < 1e-6);

        let falling: Vec<f64> = (0..20).map(|i| 200.0 - (i as f64) * 2.0).collect();
        let rsi_down = calculate_rsi(&falling, 14).unwrap();
        assert!(rsi_down < 1.0);

        let flat = vec![100.0; 20];
        let rsi_flat = calculate_rsi(&flat, 14).unwrap();
        assert!((rsi_flat - 50.0).abs() < 1e-6);
    }

    #[test]
    fn should_reject_rsi_on_invalid_period_or_short_series() {
        assert!(matches!(
            calculate_rsi(&[1.0, 2.0], 0),
            Err(DomainError::InvalidConfig { .. })
        ));
        assert!(matches!(
            calculate_rsi(&[1.0, 2.0, 3.0], 5),
            Err(DomainError::InsufficientData {
                indicator: "RSI",
                ..
            })
        ));
    }

    #[test]
    fn should_calculate_atr_and_handle_errors() {
        let closes: Vec<f64> = (0..20).map(|i| 1000.0 + (i as f64)).collect();
        let candles = sample_candles_from_closes(&closes);
        let atr = calculate_atr(&candles, 14).unwrap();
        // Each candle has high = close + 5, low = close - 5 -> range = 10.0
        assert!((atr - 10.0).abs() < 1e-6);

        assert!(matches!(
            calculate_atr(&candles, 0),
            Err(DomainError::InvalidConfig { .. })
        ));
        assert!(matches!(
            calculate_atr(&candles[..5], 10),
            Err(DomainError::InsufficientData {
                indicator: "ATR",
                ..
            })
        ));
    }

    #[test]
    fn should_evaluate_technical_snapshot_for_bullish_bearish_and_hold() {
        let cfg = StrategyConfig {
            fast_ema_period: 3,
            slow_ema_period: 7,
            rsi_period: 5,
            rsi_overbought: 85.0,
            rsi_oversold: 15.0,
            atr_period: 5,
            min_confidence: 0.60,
        };

        // Moderate uptrend with small pullbacks so RSI stays between 45 and 85
        let bullish_closes = vec![
            100.0, 102.0, 101.0, 104.0, 103.0, 106.0, 105.0, 108.0, 107.0, 110.0, 112.0,
        ];
        let bullish_candles = sample_candles_from_closes(&bullish_closes);
        let bull_sig = evaluate_technical_snapshot(&bullish_candles, &cfg).unwrap();
        assert_eq!(bull_sig.action, SignalAction::Long);
        assert!(bull_sig.confidence >= 0.55);

        // Moderate downtrend with small bounces so RSI stays between 15 and 55
        let bearish_closes = vec![
            112.0, 110.0, 111.0, 108.0, 109.0, 106.0, 107.0, 104.0, 105.0, 102.0, 100.0,
        ];
        let bearish_candles = sample_candles_from_closes(&bearish_closes);
        let bear_sig = evaluate_technical_snapshot(&bearish_candles, &cfg).unwrap();
        assert_eq!(bear_sig.action, SignalAction::Short);
        assert!(bear_sig.confidence >= 0.55);

        // Flat market -> Hold
        let flat_candles = sample_candles_from_closes(&vec![100.0; 12]);
        let hold_sig = evaluate_technical_snapshot(&flat_candles, &cfg).unwrap();
        assert_eq!(hold_sig.action, SignalAction::Hold);
    }

    #[test]
    fn should_reject_invalid_strategy_ema_periods() {
        let mut cfg = StrategyConfig::default();
        cfg.fast_ema_period = 21;
        cfg.slow_ema_period = 9;
        let candles = sample_candles_from_closes(&vec![100.0; 30]);
        assert!(matches!(
            evaluate_technical_snapshot(&candles, &cfg),
            Err(DomainError::InvalidConfig { .. })
        ));
    }
}
