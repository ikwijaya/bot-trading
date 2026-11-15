use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::adapters::ai_advisor::HttpAiAdvisor;
use crate::adapters::binance::{resolve_binance_base_url, BinanceFuturesClient};
use crate::adapters::daemon::{
    append_log_line, is_pid_alive, read_pid_file, read_recent_logs, spawn_background_daemon,
    stop_background_daemon,
};
use crate::adapters::paper::PaperExchangeAdapter;
use crate::adapters::state_store::{
    load_config_file, save_config_file, JsonStateStore, SystemClock,
};
use crate::core::agent::AgentEngine;
use crate::core::domain::{
    BotConfig, BotState, CycleReport, DomainError, ExecutionMode,
};
use crate::core::ports::{ExchangePort, StateStorePort};

/// Binance USDⓈ-M Futures Hybrid Trading Bot Agent CLI.
#[derive(Debug, Parser)]
#[command(
    name = "trade-bot",
    version,
    about = "Binance USDⓈ-M Futures Hybrid Trading Bot Agent (CLI & Background Daemon)"
)]
pub struct Cli {
    /// Path to configuration JSON file.
    #[arg(short, long, default_value = "config.json", global = true)]
    pub config: PathBuf,

    #[command(subcommand)]
    pub command: Commands,
}

/// Supported CLI subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Commands {
    /// Generate a default config.toml file.
    InitConfig {
        /// Overwrite existing config file if present.
        #[arg(short, long, default_value_t = false)]
        force: bool,
    },

    /// Start the trading bot agent (defaults to background daemon mode).
    Start {
        /// Run in background daemon mode (default: true unless --foreground is set).
        #[arg(short, long, default_value_t = true)]
        daemon: bool,

        /// Run in foreground terminal instead of spawning background daemon.
        #[arg(long, default_value_t = false)]
        foreground: bool,
    },

    /// Stop the running background trading bot daemon.
    Stop,

    /// Show current daemon process status, balance, and latest agent decision.
    Status,

    /// List open futures positions and recent order fills.
    Positions,

    /// Show recent lines from the background daemon log file.
    Logs {
        /// Number of recent log lines to display.
        #[arg(short = 'n', long, default_value_t = 30)]
        lines: usize,
    },

    /// Emergency action: stop daemon and immediately market-close all open positions.
    PanicClose,

    /// Execute a single evaluation cycle immediately and print the report.
    TickOnce,

    /// Internal worker loop invoked by the background daemon spawner.
    #[command(hide = true)]
    RunWorker,
}

/// Formats a human-readable status summary from [`BotConfig`], [`BotState`], and live PID check.
pub fn format_status_report(cfg: &BotConfig, state: &BotState, live_pid: Option<u32>) -> String {
    let status_label = match live_pid {
        Some(pid) => format!("RUNNING (PID {})", pid),
        None => "STOPPED".to_string(),
    };
    let decision_summary = match &state.last_decision {
        Some(d) => format!(
            "{:?} (confidence {:.2}) — {}",
            d.action, d.confidence, d.technical_reason
        ),
        None => "None yet".to_string(),
    };

    format!(
        "=== Binance Futures Trading Bot Status ===\n\
         Status          : {status_label}\n\
         Mode            : {}\n\
         Symbol          : {} ({})\n\
         Leverage        : {}x (Max {}x)\n\
         Wallet Balance  : {:.2} USDT (Day Start: {:.2} USDT)\n\
         Realized PnL    : {:+.2} USDT\n\
         Total Trades    : {}\n\
         Open Positions  : {}\n\
         Last Decision   : {}\n",
        cfg.mode,
        cfg.symbol,
        cfg.interval,
        cfg.risk.leverage,
        cfg.risk.max_leverage,
        state.current_balance_usdt,
        state.day_start_balance_usdt,
        state.realized_pnl_usdt,
        state.total_trades,
        state.open_positions.len(),
        decision_summary
    )
}

/// Formats a human-readable report of open positions and recent fills.
pub fn format_positions_report(state: &BotState) -> String {
    let mut out = String::from("=== Active Futures Positions ===\n");
    if state.open_positions.is_empty() {
        out.push_str("No open positions.\n");
    } else {
        for (idx, pos) in state.open_positions.iter().enumerate() {
            out.push_str(&format!(
                "{}. {} {:?} | Qty: {:.3} | Entry: {:.2} | Mark: {:.2} | uPnL: {:+.2} USDT | Lev: {}x | SL: {:?} | TP: {:?}\n",
                idx + 1,
                pos.symbol,
                pos.side,
                pos.quantity,
                pos.entry_price,
                pos.mark_price,
                pos.unrealized_pnl,
                pos.leverage,
                pos.stop_loss,
                pos.take_profit
            ));
        }
    }

    out.push_str("\n=== Recent Order Fills (last 5) ===\n");
    if state.recent_fills.is_empty() {
        out.push_str("No recent fills.\n");
    } else {
        let start = state.recent_fills.len().saturating_sub(5);
        for fill in &state.recent_fills[start..] {
            out.push_str(&format!(
                "- [{}] {} {:?} {:?} qty={:.3} @ {:.2} (PnL={:+.2} USDT)\n",
                fill.order_id,
                fill.symbol,
                fill.position_side,
                fill.side,
                fill.executed_qty,
                fill.avg_price,
                fill.realized_pnl
            ));
        }
    }
    out
}

/// Formats a single [`CycleReport`] for console or log output.
pub fn format_cycle_report(report: &CycleReport) -> String {
    let order_info = match &report.executed_order {
        Some(fill) => format!(
            "EXECUTED {} {:?} qty={:.3} @ {:.2} (SL={:?}, TP={:?})",
            fill.order_id,
            fill.position_side,
            fill.executed_qty,
            fill.avg_price,
            fill.stop_loss,
            fill.take_profit
        ),
        None => "NO_NEW_ORDER".to_string(),
    };
    format!(
        "Symbol={} | Decision={:?} (conf={:.2}) | Tech={} | Closed={} | Order={}",
        report.symbol,
        report.decision.action,
        report.decision.confidence,
        report.technical.reason,
        report.closed_positions,
        order_info
    )
}

/// Executes a single cycle using the configured exchange mode (`paper`, `testnet`, or `live`).
pub async fn run_single_cycle(
    cfg: &BotConfig,
    store: Arc<JsonStateStore>,
) -> Result<CycleReport, DomainError> {
    let ai_key = std::env::var(&cfg.ai.api_key_env).ok();
    let ai_advisor = HttpAiAdvisor::new(cfg.ai.clone(), ai_key);

    match cfg.mode {
        ExecutionMode::Paper => {
            let exchange = PaperExchangeAdapter::new(cfg, Arc::clone(&store), SystemClock);
            let engine = AgentEngine::new(exchange, ai_advisor, (*store).clone(), SystemClock);
            engine.execute_cycle(cfg).await
        }
        ExecutionMode::Testnet | ExecutionMode::Live => {
            let api_key = std::env::var(&cfg.exchange.api_key_env).unwrap_or_default();
            let api_secret = std::env::var(&cfg.exchange.api_secret_env).unwrap_or_default();
            let base_url = resolve_binance_base_url(cfg.mode, &cfg.exchange);
            let exchange = BinanceFuturesClient::new(
                base_url,
                api_key,
                api_secret,
                cfg.exchange.recv_window_ms,
                SystemClock,
            );
            let engine = AgentEngine::new(exchange, ai_advisor, (*store).clone(), SystemClock);
            engine.execute_cycle(cfg).await
        }
    }
}

/// Dispatches CLI commands and returns user-facing output text.
pub async fn execute_cli(cli: Cli) -> Result<String, DomainError> {
    match cli.command {
        Commands::InitConfig { force } => {
            if cli.config.exists() && !force {
                return Err(DomainError::InvalidConfig {
                    field: "config",
                    reason: format!(
                        "config file '{}' already exists (use --force to overwrite)",
                        cli.config.display()
                    ),
                });
            }
            let default_cfg = BotConfig::default();
            save_config_file(&cli.config, &default_cfg)?;
            Ok(format!(
                "Initialized default configuration at '{}'",
                cli.config.display()
            ))
        }

        Commands::Start { daemon, foreground } => {
            let cfg = load_config_file(&cli.config)?;
            if !cli.config.exists() {
                save_config_file(&cli.config, &cfg)?;
            }
            let store = JsonStateStore::new(&cfg.data_dir);
            if daemon && !foreground {
                let pid = spawn_background_daemon(&cli.config, &cfg, &store)?;
                Ok(format!(
                    "Trading bot daemon started in background (PID: {}, Mode: {}, Symbol: {}, Log: {})",
                    pid,
                    cfg.mode,
                    cfg.symbol,
                    store.log_file_path().display()
                ))
            } else {
                run_worker_loop(&cfg, Arc::new(store)).await?;
                Ok("Foreground worker stopped.".to_string())
            }
        }

        Commands::Stop => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::new(&cfg.data_dir);
            match stop_background_daemon(&store)? {
                Some(pid) => Ok(format!("Stopped background trading bot daemon (PID: {}).", pid)),
                None => Ok("Trading bot daemon is not currently running.".to_string()),
            }
        }

        Commands::Status => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::new(&cfg.data_dir);
            let state = store.load_state()?.unwrap_or_else(|| BotState::new(&cfg));
            let live_pid = read_pid_file(&store.pid_file_path())?
                .filter(|&pid| is_pid_alive(pid));
            Ok(format_status_report(&cfg, &state, live_pid))
        }

        Commands::Positions => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::new(&cfg.data_dir);
            let state = store.load_state()?.unwrap_or_else(|| BotState::new(&cfg));
            Ok(format_positions_report(&state))
        }

        Commands::Logs { lines } => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::new(&cfg.data_dir);
            let entries = read_recent_logs(&store.log_file_path(), lines)?;
            if entries.is_empty() {
                Ok("No daemon logs found yet.".to_string())
            } else {
                Ok(entries.join("\n"))
            }
        }

        Commands::PanicClose => {
            let cfg = load_config_file(&cli.config)?;
            let store = Arc::new(JsonStateStore::new(&cfg.data_dir));
            let stopped_pid = stop_background_daemon(&store)?;

            let closed = match cfg.mode {
                ExecutionMode::Paper => {
                    let exchange =
                        PaperExchangeAdapter::new(&cfg, Arc::clone(&store), SystemClock);
                    exchange.close_all_positions(&cfg.symbol).await?
                }
                ExecutionMode::Testnet | ExecutionMode::Live => {
                    let api_key = std::env::var(&cfg.exchange.api_key_env).unwrap_or_default();
                    let api_secret =
                        std::env::var(&cfg.exchange.api_secret_env).unwrap_or_default();
                    let base_url = resolve_binance_base_url(cfg.mode, &cfg.exchange);
                    let exchange = BinanceFuturesClient::new(
                        base_url,
                        api_key,
                        api_secret,
                        cfg.exchange.recv_window_ms,
                        SystemClock,
                    );
                    exchange.close_all_positions(&cfg.symbol).await?
                }
            };

            let _ = append_log_line(
                &store.log_file_path(),
                &format!(
                    "PANIC CLOSE executed: daemon_stopped={:?}, closed_positions={}",
                    stopped_pid,
                    closed.len()
                ),
            );

            Ok(format!(
                "Panic close complete: stopped daemon PID {:?}, closed {} open position(s).",
                stopped_pid,
                closed.len()
            ))
        }

        Commands::TickOnce => {
            let cfg = load_config_file(&cli.config)?;
            let store = Arc::new(JsonStateStore::new(&cfg.data_dir));
            let report = run_single_cycle(&cfg, Arc::clone(&store)).await?;
            let line = format_cycle_report(&report);
            let _ = append_log_line(&store.log_file_path(), &line);
            Ok(line)
        }

        Commands::RunWorker => {
            let cfg = load_config_file(&cli.config)?;
            let store = Arc::new(JsonStateStore::new(&cfg.data_dir));
            run_worker_loop(&cfg, store).await?;
            Ok("Worker finished.".to_string())
        }
    }
}

async fn run_worker_loop(cfg: &BotConfig, store: Arc<JsonStateStore>) -> Result<(), DomainError> {
    let poll_secs = cfg.poll_interval_secs.max(1);
    let _ = append_log_line(
        &store.log_file_path(),
        &format!(
            "Worker loop active (mode={}, symbol={}, poll={}s)",
            cfg.mode, cfg.symbol, poll_secs
        ),
    );

    loop {
        match run_single_cycle(cfg, Arc::clone(&store)).await {
            Ok(report) => {
                let summary = format_cycle_report(&report);
                let _ = append_log_line(&store.log_file_path(), &summary);
            }
            Err(err) => {
                let _ = append_log_line(&store.log_file_path(), &format!("Cycle error: {}", err));
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(poll_secs)) => {}
            _ = tokio::signal::ctrl_c() => {
                let _ = append_log_line(&store.log_file_path(), "Received shutdown signal; exiting worker loop.");
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::{
        AgentDecision, OrderFill, OrderSide, Position, PositionSide, SignalAction, TechnicalSignal,
    };

    #[test]
    fn should_format_status_positions_and_cycle_reports() {
        let cfg = BotConfig::default();
        let mut state = BotState::new(&cfg);
        state.last_decision = Some(AgentDecision {
            action: SignalAction::Long,
            confidence: 0.78,
            technical_reason: "EMA9 > EMA21".to_string(),
            ai_reason: None,
        });
        state.open_positions.push(Position {
            symbol: "BTCUSDT".to_string(),
            side: PositionSide::Long,
            quantity: 0.02,
            entry_price: 65_000.0,
            mark_price: 65_500.0,
            unrealized_pnl: 10.0,
            leverage: 3,
            stop_loss: Some(64_200.0),
            take_profit: Some(66_600.0),
        });
        state.recent_fills.push(OrderFill {
            order_id: "paper-1".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            position_side: PositionSide::Long,
            executed_qty: 0.02,
            avg_price: 65_000.0,
            stop_loss: Some(64_200.0),
            take_profit: Some(66_600.0),
            realized_pnl: 0.0,
            timestamp_ms: 1_700_000_000_000,
        });

        let status_txt = format_status_report(&cfg, &state, Some(4321));
        assert!(status_txt.contains("RUNNING (PID 4321)"));
        assert!(status_txt.contains("BTCUSDT"));

        let pos_txt = format_positions_report(&state);
        assert!(pos_txt.contains("Qty: 0.020"));
        assert!(pos_txt.contains("paper-1"));

        let cycle = CycleReport {
            timestamp_ms: 1_700_000_000_000,
            symbol: "BTCUSDT".to_string(),
            technical: TechnicalSignal {
                action: SignalAction::Long,
                fast_ema: 65_100.0,
                slow_ema: 64_800.0,
                rsi: 56.0,
                atr: 150.0,
                last_price: 65_200.0,
                confidence: 0.75,
                reason: "Bullish".to_string(),
            },
            ai: None,
            decision: state.last_decision.unwrap(),
            executed_order: state.recent_fills.first().cloned(),
            closed_positions: 0,
        };
        let cycle_txt = format_cycle_report(&cycle);
        assert!(cycle_txt.contains("EXECUTED paper-1"));
    }

    #[tokio::test]
    async fn should_execute_cli_commands_init_tick_status_positions_logs_and_panic_close() {
        let temp_dir = std::env::temp_dir().join(format!(
            "trade_bot_cli_test_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(1)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg_path = temp_dir.join("config.toml");

        // 1. InitConfig
        let out_init = execute_cli(Cli {
            config: cfg_path.clone(),
            command: Commands::InitConfig { force: false },
        })
        .await
        .unwrap();
        assert!(out_init.contains("Initialized default configuration"));

        // Customize data_dir & unreachable public_base_url so paper tick runs deterministically offline
        let mut cfg = load_config_file(&cfg_path).unwrap();
        cfg.data_dir = temp_dir.join(".trade-bot").to_string_lossy().to_string();
        cfg.exchange.live_base_url = "http://127.0.0.1:9".to_string();
        cfg.strategy.min_confidence = 0.50;
        save_config_file(&cfg_path, &cfg).unwrap();

        // 2. TickOnce
        let out_tick = execute_cli(Cli {
            config: cfg_path.clone(),
            command: Commands::TickOnce,
        })
        .await
        .unwrap();
        assert!(out_tick.contains("Symbol=BTCUSDT"));

        // 3. Status
        let out_status = execute_cli(Cli {
            config: cfg_path.clone(),
            command: Commands::Status,
        })
        .await
        .unwrap();
        assert!(out_status.contains("STOPPED"));

        // 4. Positions
        let out_pos = execute_cli(Cli {
            config: cfg_path.clone(),
            command: Commands::Positions,
        })
        .await
        .unwrap();
        assert!(out_pos.contains("Active Futures Positions"));

        // 5. Logs
        let out_logs = execute_cli(Cli {
            config: cfg_path.clone(),
            command: Commands::Logs { lines: 10 },
        })
        .await
        .unwrap();
        assert!(out_logs.contains("Symbol=BTCUSDT"));

        // 6. PanicClose
        let out_panic = execute_cli(Cli {
            config: cfg_path.clone(),
            command: Commands::PanicClose,
        })
        .await
        .unwrap();
        assert!(out_panic.contains("Panic close complete"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
