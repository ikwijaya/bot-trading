use crate::core::domain::{
    AccountSnapshot, AgentDecision, AiAssessment, BotConfig, BotState, CycleReport, DecisionPolicy,
    DomainError, OrderFill, PendingOrderProposal, PositionSide, ProbabilityAssessment,
    SignalAction, SkillKnowledgeBase, TechnicalSignal, TradeOrderRequest,
};
use crate::core::indicators::evaluate_technical_snapshot;
use crate::core::ports::{AiAdvisorPort, ClockPort, ExchangePort, StateStorePort};
use crate::core::risk::{
    calculate_position_size_with_policy, compute_sl_tp_prices, evaluate_trade_probability,
    validate_risk_limits,
};

/// Pure function that combines a rule-based [`TechnicalSignal`] with an optional [`AiAssessment`].
///
/// # Parameters
/// - `tech`: Evaluated technical indicator signal.
/// - `ai`: Optional AI market assessment.
/// - `ai_weight`: Weight (`0.0..=1.0`) given to the AI assessment when combining confidences.
/// - `min_confidence`: Minimum confidence threshold (`0.0..=1.0`) required to trigger an entry/close action.
///
/// # Returns
/// Deterministic [`AgentDecision`] containing final [`SignalAction`], blended confidence, and explanations.
pub fn combine_signals(
    tech: &TechnicalSignal,
    ai: Option<&AiAssessment>,
    ai_weight: f64,
    min_confidence: f64,
) -> AgentDecision {
    let w = ai_weight.clamp(0.0, 1.0);

    match ai {
        None => {
            if tech.action != SignalAction::Hold && tech.confidence >= min_confidence {
                AgentDecision {
                    action: tech.action,
                    confidence: tech.confidence,
                    technical_reason: tech.reason.clone(),
                    ai_reason: None,
                }
            } else {
                AgentDecision {
                    action: SignalAction::Hold,
                    confidence: tech.confidence,
                    technical_reason: tech.reason.clone(),
                    ai_reason: None,
                }
            }
        }
        Some(assessment) => {
            if assessment.action == SignalAction::Close && assessment.confidence >= min_confidence {
                return AgentDecision {
                    action: SignalAction::Close,
                    confidence: assessment.confidence,
                    technical_reason: tech.reason.clone(),
                    ai_reason: Some(assessment.reasoning.clone()),
                };
            }

            if assessment.action == tech.action && tech.action != SignalAction::Hold {
                let blended = (1.0 - w) * tech.confidence + w * assessment.confidence;
                let final_action = if blended >= min_confidence {
                    tech.action
                } else {
                    SignalAction::Hold
                };
                AgentDecision {
                    action: final_action,
                    confidence: blended,
                    technical_reason: tech.reason.clone(),
                    ai_reason: Some(assessment.reasoning.clone()),
                }
            } else {
                let blended = (1.0 - w) * tech.confidence + w * (1.0 - assessment.confidence);
                AgentDecision {
                    action: SignalAction::Hold,
                    confidence: blended,
                    technical_reason: tech.reason.clone(),
                    ai_reason: Some(format!(
                        "AI veto/divergence (AI={:?}: {})",
                        assessment.action, assessment.reasoning
                    )),
                }
            }
        }
    }
}

/// Pure function that updates the continuous-learning [`SkillKnowledgeBase`] after each cycle.
pub fn update_skill_knowledge(
    kb: &mut SkillKnowledgeBase,
    technical: &TechnicalSignal,
    ai: Option<&AiAssessment>,
    prob: Option<&ProbabilityAssessment>,
    closed_fills: &[OrderFill],
    now_ms: i64,
) {
    kb.total_cycles += 1;
    kb.last_updated_ms = now_ms;

    if technical.fast_ema > technical.slow_ema {
        kb.bullish_cycles += 1;
    } else if technical.fast_ema < technical.slow_ema {
        kb.bearish_cycles += 1;
    }

    for fill in closed_fills {
        if fill.realized_pnl >= 0.0 {
            kb.winning_closes += 1;
        } else {
            kb.losing_closes += 1;
        }
    }

    if let Some(p) = prob {
        if p.is_positive_ev {
            kb.positive_ev_signals += 1;
            kb.cumulative_ev_r += p.expected_value_r;
        }
    }

    if let Some(a) = ai {
        kb.recent_ai_insights.push(format!(
            "[{:?} conf={:.0}%] {}",
            a.action,
            a.confidence * 100.0,
            a.reasoning
        ));
        if kb.recent_ai_insights.len() > 5 {
            let excess = kb.recent_ai_insights.len() - 5;
            kb.recent_ai_insights.drain(0..excess);
        }
    }

    // Re-synthesize adaptive trading rules from accumulated history
    let mut rules = Vec::new();
    let regime = if kb.bullish_cycles >= kb.bearish_cycles {
        "Bullish / Uptrend Dominan"
    } else {
        "Bearish / Downtrend Dominan"
    };
    rules.push(format!(
        "Rezim Pasar Terkini: {} (Bullish: {} siklus, Bearish: {} siklus).",
        regime, kb.bullish_cycles, kb.bearish_cycles
    ));

    let total_closed = kb.winning_closes + kb.losing_closes;
    if total_closed > 0 {
        let win_rate = (kb.winning_closes as f64 / total_closed as f64) * 100.0;
        if win_rate < 45.0 {
            rules.push(format!(
                "Adaptasi Defensif: Realized Win-Rate {:.1}% (<45%) — prioritaskan sinyal dengan Expected Value > +0.30R dan hindari entry dekat batas RSI ekstrem.",
                win_rate
            ));
        } else {
            rules.push(format!(
                "Performa Stabil: Realized Win-Rate {:.1}% ({} menang / {} kalah) — pertahankan disiplin Half-Kelly & ATR SL/TP.",
                win_rate, kb.winning_closes, kb.losing_closes
            ));
        }
    } else {
        rules.push(
            "Fase Observasi Awal: Belum ada posisi tertutup; gunakan filter ketat Expected Value > 0 setelah fee 0.08%."
                .to_string(),
        );
    }

    if technical.rsi >= 65.0 {
        rules.push(format!(
            "Peringatan Momentum Terakhir: RSI tinggi ({:.1}) — waspadai potensi pullback sebelum membuka LONG baru.",
            technical.rsi
        ));
    } else if technical.rsi <= 35.0 {
        rules.push(format!(
            "Peringatan Momentum Terakhir: RSI rendah ({:.1}) — waspadai potensi technical rebound sebelum membuka SHORT baru.",
            technical.rsi
        ));
    }

    kb.learned_rules = rules;
}

/// Pure function that renders the accumulated [`SkillKnowledgeBase`] and [`BotState`] into a human- & LLM-readable `skills.md` document.
pub fn format_skills_markdown(
    symbol: &str,
    kb: &SkillKnowledgeBase,
    state: &BotState,
) -> String {
    let total_closed = kb.winning_closes + kb.losing_closes;
    let realized_wr = if total_closed > 0 {
        (kb.winning_closes as f64 / total_closed as f64) * 100.0
    } else {
        0.0
    };
    let avg_ev = if kb.positive_ev_signals > 0 {
        kb.cumulative_ev_r / (kb.positive_ev_signals as f64)
    } else {
        0.0
    };

    let mut md = format!(
        "# 🧠 Trade-Bot Learned Skills & Context (`skills.md`)\n\n\
         - **Symbol:** `{symbol}` (`{}`)\n\
         - **Total Siklus Dipelajari:** `{}` siklus (Bullish: `{}`, Bearish: `{}`)\n\
         - **Rekam Jejak Posisi Tertutup:** `{}` Menang / `{}` Kalah (Win-Rate: `{:.1}%`)\n\
         - **Rata-Rata Expected Value (+EV):** `{:+.2}R` dari `{}` sinyal valid\n\
         - **Realized PnL Akumulasi:** `{:+.2} USDT` (Saldo Saat Ini: `{:.2} USDT`)\n\n\
         ## 1. Aturan & Heuristik yang Dipelajari Bot (`Learned Rules`)\n",
        state.mode,
        kb.total_cycles,
        kb.bullish_cycles,
        kb.bearish_cycles,
        kb.winning_closes,
        kb.losing_closes,
        realized_wr,
        avg_ev,
        kb.positive_ev_signals,
        state.realized_pnl_usdt,
        state.current_balance_usdt
    );

    if kb.learned_rules.is_empty() {
        md.push_str("- Belum ada aturan adaptif yang terbentuk.\n");
    } else {
        for (i, rule) in kb.learned_rules.iter().enumerate() {
            md.push_str(&format!("{}. {}\n", i + 1, rule));
        }
    }

    md.push_str("\n## 2. Memori Insight AI Terakhir (`Recent AI Context`)\n");
    if kb.recent_ai_insights.is_empty() {
        md.push_str("- *(Modul AI belum memiliki catatan insight baru atau `ai.enabled = false`)*\n");
    } else {
        for insight in &kb.recent_ai_insights {
            md.push_str(&format!("- {}\n", insight));
        }
    }

    md
}

/// Core orchestrator for the Hybrid Trading Agent, parameterized over injected I/O ports.
pub struct AgentEngine<E, A, S, C> {
    exchange: E,
    ai_advisor: A,
    state_store: S,
    clock: C,
}

impl<E, A, S, C> AgentEngine<E, A, S, C>
where
    E: ExchangePort,
    A: AiAdvisorPort,
    S: StateStorePort,
    C: ClockPort,
{
    /// Creates a new [`AgentEngine`] with injected dependencies.
    pub fn new(exchange: E, ai_advisor: A, state_store: S, clock: C) -> Self {
        Self {
            exchange,
            ai_advisor,
            state_store,
            clock,
        }
    }

    /// Executes a single deterministic perception-reasoning-action cycle.
    pub async fn execute_cycle(&self, cfg: &BotConfig) -> Result<CycleReport, DomainError> {
        let now_ms = self.clock.now_ms();
        let mut state = self
            .state_store
            .load_state()?
            .unwrap_or_else(|| BotState::new(cfg));

        let candles = self
            .exchange
            .fetch_klines(&cfg.symbol, &cfg.interval, cfg.kline_limit)
            .await?;
        let technical = evaluate_technical_snapshot(&candles, &cfg.strategy)?;
        let mut account = self.exchange.fetch_account(&cfg.symbol).await?;

        // Query AI advisor if enabled; fall back gracefully to None if advisor returns an error
        let ai_assessment = if cfg.ai.enabled {
            self.ai_advisor
                .assess_market(&cfg.symbol, &technical, &account.positions)
                .await
                .unwrap_or(None)
        } else {
            None
        };

        let decision = combine_signals(
            &technical,
            ai_assessment.as_ref(),
            cfg.ai.weight,
            cfg.strategy.min_confidence,
        );

        let mut closed_positions = 0usize;
        let mut closed_fills_vec = Vec::new();
        let has_opposite_position = account.positions.iter().any(|pos| {
            (pos.side == PositionSide::Long && decision.action == SignalAction::Short)
                || (pos.side == PositionSide::Short && decision.action == SignalAction::Long)
        });

        if (decision.action == SignalAction::Close && !account.positions.is_empty())
            || has_opposite_position
        {
            let close_fills = self.exchange.close_all_positions(&cfg.symbol).await?;
            closed_positions = close_fills.len();
            for fill in close_fills {
                state.realized_pnl_usdt += fill.realized_pnl;
                state.total_trades += 1;
                closed_fills_vec.push(fill.clone());
                state.recent_fills.push(fill);
            }
            account = self.exchange.fetch_account(&cfg.symbol).await?;
        }

        let mut executed_order: Option<OrderFill> = None;
        let mut pending_order: Option<PendingOrderProposal> = None;
        let mut probability: Option<ProbabilityAssessment> = None;
        let target_side = match decision.action {
            SignalAction::Long => Some(PositionSide::Long),
            SignalAction::Short => Some(PositionSide::Short),
            SignalAction::Hold | SignalAction::Close => None,
        };

        if let Some(pos_side) = target_side {
            if cfg.strategy.decision_policy != DecisionPolicy::Skip
                && validate_risk_limits(&state, &account, &cfg.risk).is_ok()
            {
                let (sl, tp) = compute_sl_tp_prices(
                    technical.last_price,
                    technical.atr,
                    pos_side,
                    &cfg.risk,
                )?;
                if let Ok(prob) = evaluate_trade_probability(
                    decision.confidence,
                    technical.last_price,
                    sl,
                    tp,
                    cfg.risk.require_positive_ev,
                ) {
                    probability = Some(prob.clone());
                    if let Ok(sizing) = calculate_position_size_with_policy(
                        account.available_balance,
                        technical.last_price,
                        sl,
                        &prob,
                        &cfg.risk,
                    ) {
                        let req = TradeOrderRequest {
                            symbol: cfg.symbol.clone(),
                            side: pos_side.entry_order_side(),
                            position_side: pos_side,
                            quantity: sizing.quantity,
                            entry_price: technical.last_price,
                            stop_loss: Some(sl),
                            take_profit: Some(tp),
                            leverage: sizing.leverage,
                            reduce_only: false,
                        };

                        match cfg.strategy.decision_policy {
                            DecisionPolicy::Auto => {
                                self.exchange
                                    .set_leverage(&cfg.symbol, sizing.leverage)
                                    .await?;
                                let fill = self.exchange.place_order(&req).await?;
                                state.total_trades += 1;
                                state.pending_order = None;
                                state.recent_fills.push(fill.clone());
                                if state.recent_fills.len() > 50 {
                                    let excess = state.recent_fills.len() - 50;
                                    state.recent_fills.drain(0..excess);
                                }
                                executed_order = Some(fill);
                                account = self.exchange.fetch_account(&cfg.symbol).await?;
                            }
                            DecisionPolicy::ConfirmByChat => {
                                let proposal = PendingOrderProposal {
                                    request: req,
                                    notional_usdt: sizing.notional_usdt,
                                    probability: prob,
                                    reason: decision.technical_reason.clone(),
                                    created_at_ms: now_ms,
                                };
                                state.pending_order = Some(proposal.clone());
                                pending_order = Some(proposal);
                            }
                            DecisionPolicy::Skip => {}
                        }
                    }
                }
            }
        }

        update_skill_knowledge(
            &mut state.skill_knowledge,
            &technical,
            ai_assessment.as_ref(),
            probability.as_ref(),
            &closed_fills_vec,
            now_ms,
        );
        self.sync_state_from_account(&mut state, &account, decision.clone(), now_ms);
        self.state_store.save_state(&state)?;

        Ok(CycleReport {
            timestamp_ms: now_ms,
            symbol: cfg.symbol.clone(),
            technical,
            ai: ai_assessment,
            decision,
            executed_order,
            pending_order,
            probability,
            closed_positions,
        })
    }

    fn sync_state_from_account(
        &self,
        state: &mut BotState,
        account: &AccountSnapshot,
        decision: AgentDecision,
        now_ms: i64,
    ) {
        state.last_tick_ms = Some(now_ms);
        state.current_balance_usdt = account.total_wallet_balance;
        state.open_positions = account.positions.clone();
        state.last_decision = Some(decision);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::{Candle, OrderSide, Position};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct FakeClock(i64);
    impl ClockPort for FakeClock {
        fn now_ms(&self) -> i64 {
            self.0
        }
    }

    struct FakeAiAdvisor(Option<AiAssessment>);
    #[async_trait]
    impl AiAdvisorPort for FakeAiAdvisor {
        async fn assess_market(
            &self,
            _symbol: &str,
            _signal: &TechnicalSignal,
            _open_positions: &[Position],
        ) -> Result<Option<AiAssessment>, DomainError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Default)]
    struct FakeStateStore {
        state: Mutex<Option<BotState>>,
    }
    impl StateStorePort for FakeStateStore {
        fn load_state(&self) -> Result<Option<BotState>, DomainError> {
            Ok(self.state.lock().unwrap().clone())
        }
        fn save_state(&self, state: &BotState) -> Result<(), DomainError> {
            *self.state.lock().unwrap() = Some(state.clone());
            Ok(())
        }
    }

    struct FakeExchange {
        candles: Vec<Candle>,
        account: Mutex<AccountSnapshot>,
    }

    #[async_trait]
    impl ExchangePort for FakeExchange {
        async fn fetch_klines(
            &self,
            _symbol: &str,
            _interval: &str,
            _limit: usize,
        ) -> Result<Vec<Candle>, DomainError> {
            Ok(self.candles.clone())
        }

        async fn fetch_account(&self, _symbol: &str) -> Result<AccountSnapshot, DomainError> {
            Ok(self.account.lock().unwrap().clone())
        }

        async fn set_leverage(&self, _symbol: &str, _leverage: u32) -> Result<(), DomainError> {
            Ok(())
        }

        async fn place_order(&self, req: &TradeOrderRequest) -> Result<OrderFill, DomainError> {
            let mut acct = self.account.lock().unwrap();
            acct.positions.push(Position {
                symbol: req.symbol.clone(),
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
                order_id: "ord-1".to_string(),
                symbol: req.symbol.clone(),
                side: req.side,
                position_side: req.position_side,
                executed_qty: req.quantity,
                avg_price: req.entry_price,
                stop_loss: req.stop_loss,
                take_profit: req.take_profit,
                realized_pnl: 0.0,
                timestamp_ms: 1_700_000_000_000,
            })
        }

        async fn close_all_positions(&self, symbol: &str) -> Result<Vec<OrderFill>, DomainError> {
            let mut acct = self.account.lock().unwrap();
            let drained: Vec<Position> = acct.positions.drain(..).collect();
            Ok(drained
                .into_iter()
                .map(|p| OrderFill {
                    order_id: "close-1".to_string(),
                    symbol: symbol.to_string(),
                    side: OrderSide::Sell,
                    position_side: p.side,
                    executed_qty: p.quantity,
                    avg_price: p.mark_price,
                    stop_loss: None,
                    take_profit: None,
                    realized_pnl: 12.5,
                    timestamp_ms: 1_700_000_000_000,
                })
                .collect())
        }
    }

    fn bullish_candles() -> Vec<Candle> {
        let closes = [
            100.0, 102.0, 101.0, 104.0, 103.0, 106.0, 105.0, 108.0, 107.0, 110.0, 112.0,
        ];
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Candle {
                open_time: i as i64 * 60_000,
                open: c - 1.0,
                high: c + 3.0,
                low: c - 3.0,
                close: c,
                volume: 50.0,
                close_time: (i as i64 + 1) * 60_000 - 1,
            })
            .collect()
    }

    #[test]
    fn should_combine_signals_with_and_without_ai_advisor() {
        let tech = TechnicalSignal {
            action: SignalAction::Long,
            fast_ema: 105.0,
            slow_ema: 100.0,
            rsi: 58.0,
            atr: 4.0,
            last_price: 106.0,
            confidence: 0.75,
            reason: "Bullish EMA".to_string(),
        };

        // Without AI -> follows technical signal
        let dec_no_ai = combine_signals(&tech, None, 0.35, 0.60);
        assert_eq!(dec_no_ai.action, SignalAction::Long);

        // With agreeing AI -> boosted/blended Long
        let ai_agree = AiAssessment {
            action: SignalAction::Long,
            confidence: 0.85,
            reasoning: "Strong momentum".to_string(),
        };
        let dec_agree = combine_signals(&tech, Some(&ai_agree), 0.40, 0.60);
        assert_eq!(dec_agree.action, SignalAction::Long);
        assert!(dec_agree.confidence > 0.75);

        // With disagreeing AI -> vetoed to Hold
        let ai_veto = AiAssessment {
            action: SignalAction::Short,
            confidence: 0.80,
            reasoning: "Macro resistance".to_string(),
        };
        let dec_veto = combine_signals(&tech, Some(&ai_veto), 0.40, 0.60);
        assert_eq!(dec_veto.action, SignalAction::Hold);

        // With AI Close -> triggers Close
        let ai_close = AiAssessment {
            action: SignalAction::Close,
            confidence: 0.90,
            reasoning: "Volatility spike".to_string(),
        };
        let dec_close = combine_signals(&tech, Some(&ai_close), 0.40, 0.60);
        assert_eq!(dec_close.action, SignalAction::Close);
    }

    #[tokio::test]
    async fn should_execute_cycle_and_open_long_position_when_signal_and_risk_valid() {
        let mut cfg = BotConfig::default();
        cfg.strategy.fast_ema_period = 3;
        cfg.strategy.slow_ema_period = 7;
        cfg.strategy.rsi_period = 5;
        cfg.strategy.atr_period = 5;
        cfg.strategy.rsi_overbought = 85.0;
        cfg.strategy.min_confidence = 0.55;

        let exchange = FakeExchange {
            candles: bullish_candles(),
            account: Mutex::new(AccountSnapshot {
                total_wallet_balance: 1_000.0,
                available_balance: 1_000.0,
                unrealized_pnl: 0.0,
                positions: vec![],
            }),
        };
        let state_store = FakeStateStore::default();
        let engine = AgentEngine::new(
            exchange,
            FakeAiAdvisor(None),
            state_store,
            FakeClock(1_700_000_000_000),
        );

        let report = engine.execute_cycle(&cfg).await.unwrap();
        assert_eq!(report.decision.action, SignalAction::Long);
        assert!(report.executed_order.is_some());
    }

    #[tokio::test]
    async fn should_close_opposite_position_before_reversing() {
        let mut cfg = BotConfig::default();
        cfg.strategy.fast_ema_period = 3;
        cfg.strategy.slow_ema_period = 7;
        cfg.strategy.rsi_period = 5;
        cfg.strategy.atr_period = 5;
        cfg.strategy.rsi_overbought = 85.0;
        cfg.strategy.min_confidence = 0.55;

        let exchange = FakeExchange {
            candles: bullish_candles(),
            account: Mutex::new(AccountSnapshot {
                total_wallet_balance: 1_000.0,
                available_balance: 900.0,
                unrealized_pnl: -5.0,
                positions: vec![Position {
                    symbol: "BTCUSDT".to_string(),
                    side: PositionSide::Short,
                    quantity: 0.1,
                    entry_price: 115.0,
                    mark_price: 112.0,
                    unrealized_pnl: 0.3,
                    leverage: 3,
                    stop_loss: Some(120.0),
                    take_profit: Some(105.0),
                }],
            }),
        };
        let state_store = FakeStateStore::default();
        let engine = AgentEngine::new(
            exchange,
            FakeAiAdvisor(None),
            state_store,
            FakeClock(1_700_000_000_000),
        );

        let report = engine.execute_cycle(&cfg).await.unwrap();
        assert_eq!(report.closed_positions, 1);
        assert!(report.executed_order.is_some());
    }

    #[tokio::test]
    async fn should_hold_order_in_confirm_by_chat_and_skip_in_skip_policy() {
        let mut cfg = BotConfig::default();
        cfg.strategy.fast_ema_period = 3;
        cfg.strategy.slow_ema_period = 7;
        cfg.strategy.rsi_period = 5;
        cfg.strategy.atr_period = 5;
        cfg.strategy.rsi_overbought = 85.0;
        cfg.strategy.min_confidence = 0.55;
        cfg.strategy.decision_policy = DecisionPolicy::ConfirmByChat;

        let exchange = FakeExchange {
            candles: bullish_candles(),
            account: Mutex::new(AccountSnapshot {
                total_wallet_balance: 1_000.0,
                available_balance: 1_000.0,
                unrealized_pnl: 0.0,
                positions: vec![],
            }),
        };
        let state_store = FakeStateStore::default();
        let engine = AgentEngine::new(
            exchange,
            FakeAiAdvisor(None),
            state_store,
            FakeClock(1_700_000_000_000),
        );

        let report = engine.execute_cycle(&cfg).await.unwrap();
        assert_eq!(report.decision.action, SignalAction::Long);
        assert!(report.executed_order.is_none());
        assert!(report.pending_order.is_some());
        assert!(report.probability.is_some());

        // Now test DecisionPolicy::Skip
        cfg.strategy.decision_policy = DecisionPolicy::Skip;
        let report_skip = engine.execute_cycle(&cfg).await.unwrap();
        assert!(report_skip.executed_order.is_none());
        assert!(report_skip.pending_order.is_none());
    }

    #[test]
    fn should_accumulate_continuous_learning_and_render_skills_markdown() {
        let cfg = BotConfig::default();
        let state = BotState::new(&cfg);
        let mut kb = SkillKnowledgeBase::default();
        let tech = TechnicalSignal {
            action: SignalAction::Long,
            confidence: 0.78,
            fast_ema: 51_000.0,
            slow_ema: 50_000.0,
            rsi: 66.5,
            atr: 250.0,
            last_price: 51_200.0,
            reason: "Uptrend strong".to_string(),
        };
        let ai = AiAssessment {
            action: SignalAction::Long,
            confidence: 0.85,
            reasoning: "Orderbook bid wall strong".to_string(),
        };
        let prob = ProbabilityAssessment {
            win_probability: 0.74,
            break_even_win_rate: 0.35,
            net_reward_to_risk: 2.0,
            expected_value_r: 1.14,
            half_kelly_fraction: 0.25,
            is_positive_ev: true,
        };
        let fill = OrderFill {
            order_id: "F-1".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Sell,
            position_side: PositionSide::Long,
            executed_qty: 0.01,
            avg_price: 51_500.0,
            stop_loss: None,
            take_profit: None,
            realized_pnl: 15.0,
            timestamp_ms: 1_700_000_000_000,
        };
        update_skill_knowledge(&mut kb, &tech, Some(&ai), Some(&prob), &[fill], 1_700_000_000_000);
        assert_eq!(kb.total_cycles, 1);
        assert_eq!(kb.bullish_cycles, 1);
        assert_eq!(kb.winning_closes, 1);
        assert!(!kb.learned_rules.is_empty());

        let md = format_skills_markdown("BTCUSDT", &kb, &state);
        assert!(md.contains("Trade-Bot Learned Skills & Context"));
        assert!(md.contains("Orderbook bid wall strong"));
        assert!(md.contains("Performa Stabil"));
    }
}
