use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Domain-level errors decoupled from any specific HTTP client, CLI, or storage layer.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum DomainError {
    #[error("insufficient data for indicator '{indicator}': required {required}, got {actual}")]
    InsufficientData {
        indicator: &'static str,
        required: usize,
        actual: usize,
    },

    #[error("invalid configuration parameter '{field}': {reason}")]
    InvalidConfig { field: &'static str, reason: String },

    #[error("risk limit violated: {reason}")]
    RiskLimitExceeded { reason: String },

    #[error("exchange operation failed: {message}")]
    ExchangeError { message: String },

    #[error("AI advisor failed: {message}")]
    AiAdvisorError { message: String },

    #[error("state store failure: {message}")]
    StateError { message: String },

    #[error("daemon process error: {message}")]
    DaemonError { message: String },
}

/// Execution environment mode for the trading bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    Paper,
    Testnet,
    Live,
}

impl std::fmt::Display for ExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionMode::Paper => write!(f, "paper"),
            ExecutionMode::Testnet => write!(f, "testnet"),
            ExecutionMode::Live => write!(f, "live"),
        }
    }
}

/// Order side on Binance USDⓈ-M Futures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Directional position side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PositionSide {
    Long,
    Short,
}

impl PositionSide {
    /// Returns the entry order side for opening this position.
    pub fn entry_order_side(self) -> OrderSide {
        match self {
            PositionSide::Long => OrderSide::Buy,
            PositionSide::Short => OrderSide::Sell,
        }
    }

    /// Returns the exit order side for closing this position.
    pub fn exit_order_side(self) -> OrderSide {
        match self {
            PositionSide::Long => OrderSide::Sell,
            PositionSide::Short => OrderSide::Buy,
        }
    }
}

/// High-level action produced by technical analysis, AI assessment, or the hybrid agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SignalAction {
    Long,
    Short,
    Hold,
    Close,
}

/// OHLCV candlestick bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    pub open_time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub close_time: i64,
}

/// Evaluated technical indicator snapshot and rule-based directional signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TechnicalSignal {
    pub action: SignalAction,
    pub fast_ema: f64,
    pub slow_ema: f64,
    pub rsi: f64,
    pub atr: f64,
    pub last_price: f64,
    pub confidence: f64,
    pub reason: String,
}

/// Optional AI/LLM market reasoning assessment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiAssessment {
    pub action: SignalAction,
    pub confidence: f64,
    pub reasoning: String,
}

/// Combined decision from the Hybrid Trading Agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentDecision {
    pub action: SignalAction,
    pub confidence: f64,
    pub technical_reason: String,
    pub ai_reason: Option<String>,
}

/// Calculated contract quantity and notional metrics from risk engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionSizing {
    pub quantity: f64,
    pub notional_usdt: f64,
    pub risk_amount_usdt: f64,
    pub leverage: u32,
}

/// Order submission payload sent to an exchange adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeOrderRequest {
    pub symbol: String,
    pub side: OrderSide,
    pub position_side: PositionSide,
    pub quantity: f64,
    pub entry_price: f64,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub leverage: u32,
    pub reduce_only: bool,
}

/// Executed order confirmation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderFill {
    pub order_id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub position_side: PositionSide,
    pub executed_qty: f64,
    pub avg_price: f64,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub realized_pnl: f64,
    pub timestamp_ms: i64,
}

/// Active futures position snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub side: PositionSide,
    pub quantity: f64,
    pub entry_price: f64,
    pub mark_price: f64,
    pub unrealized_pnl: f64,
    pub leverage: u32,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
}

impl Position {
    /// Calculates unrealized PnL at a given mark price.
    pub fn compute_unrealized_pnl(&self, current_price: f64) -> f64 {
        match self.side {
            PositionSide::Long => (current_price - self.entry_price) * self.quantity,
            PositionSide::Short => (self.entry_price - current_price) * self.quantity,
        }
    }
}

/// Account balance and open position snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub total_wallet_balance: f64,
    pub available_balance: f64,
    pub unrealized_pnl: f64,
    pub positions: Vec<Position>,
}

/// Strategy indicator parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub fast_ema_period: usize,
    pub slow_ema_period: usize,
    pub rsi_period: usize,
    pub rsi_overbought: f64,
    pub rsi_oversold: f64,
    pub atr_period: usize,
    pub min_confidence: f64,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            fast_ema_period: 9,
            slow_ema_period: 21,
            rsi_period: 14,
            rsi_overbought: 70.0,
            rsi_oversold: 30.0,
            atr_period: 14,
            min_confidence: 0.60,
        }
    }
}

/// Risk guardrail parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskConfig {
    pub leverage: u32,
    pub max_leverage: u32,
    pub risk_per_trade_pct: f64,
    pub atr_sl_multiplier: f64,
    pub atr_tp_multiplier: f64,
    pub max_daily_drawdown_pct: f64,
    pub max_open_positions: usize,
    pub initial_paper_balance_usdt: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            leverage: 3,
            max_leverage: 10,
            risk_per_trade_pct: 0.01,
            atr_sl_multiplier: 1.5,
            atr_tp_multiplier: 3.0,
            max_daily_drawdown_pct: 0.05,
            max_open_positions: 1,
            initial_paper_balance_usdt: 1_000.0,
        }
    }
}

/// Optional AI/LLM advisor settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiConfig {
    pub enabled: bool,
    pub endpoint_url: String,
    pub model: String,
    pub api_key_env: String,
    pub weight: f64,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint_url: "https://api.openai.com/v1/chat/completions".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key_env: "AI_API_KEY".to_string(),
            weight: 0.35,
        }
    }
}

/// Exchange environment variable names and endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExchangeCredentialsConfig {
    pub api_key_env: String,
    pub api_secret_env: String,
    pub testnet_base_url: String,
    pub live_base_url: String,
    pub recv_window_ms: u64,
}

impl Default for ExchangeCredentialsConfig {
    fn default() -> Self {
        Self {
            api_key_env: "BINANCE_API_KEY".to_string(),
            api_secret_env: "BINANCE_API_SECRET".to_string(),
            testnet_base_url: "https://testnet.binancefuture.com".to_string(),
            live_base_url: "https://fapi.binance.com".to_string(),
            recv_window_ms: 5_000,
        }
    }
}

/// Complete configuration for the bot agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotConfig {
    pub mode: ExecutionMode,
    pub symbol: String,
    pub interval: String,
    pub kline_limit: usize,
    pub poll_interval_secs: u64,
    pub data_dir: String,
    pub strategy: StrategyConfig,
    pub risk: RiskConfig,
    pub ai: AiConfig,
    pub exchange: ExchangeCredentialsConfig,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::Paper,
            symbol: "BTCUSDT".to_string(),
            interval: "5m".to_string(),
            kline_limit: 100,
            poll_interval_secs: 15,
            data_dir: ".trade-bot".to_string(),
            strategy: StrategyConfig::default(),
            risk: RiskConfig::default(),
            ai: AiConfig::default(),
            exchange: ExchangeCredentialsConfig::default(),
        }
    }
}

/// Persisted state of the background bot agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotState {
    pub mode: ExecutionMode,
    pub symbol: String,
    pub running: bool,
    pub pid: Option<u32>,
    pub started_at_ms: Option<i64>,
    pub last_tick_ms: Option<i64>,
    pub day_start_balance_usdt: f64,
    pub current_balance_usdt: f64,
    pub realized_pnl_usdt: f64,
    pub total_trades: u64,
    pub open_positions: Vec<Position>,
    pub last_decision: Option<AgentDecision>,
    pub recent_fills: Vec<OrderFill>,
}

impl BotState {
    /// Creates a new initial state from configuration.
    pub fn new(cfg: &BotConfig) -> Self {
        let initial_balance = cfg.risk.initial_paper_balance_usdt;
        Self {
            mode: cfg.mode,
            symbol: cfg.symbol.clone(),
            running: false,
            pid: None,
            started_at_ms: None,
            last_tick_ms: None,
            day_start_balance_usdt: initial_balance,
            current_balance_usdt: initial_balance,
            realized_pnl_usdt: 0.0,
            total_trades: 0,
            open_positions: Vec::new(),
            last_decision: None,
            recent_fills: Vec::new(),
        }
    }
}

/// Outcome of a single agent evaluation cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CycleReport {
    pub timestamp_ms: i64,
    pub symbol: String,
    pub technical: TechnicalSignal,
    pub ai: Option<AiAssessment>,
    pub decision: AgentDecision,
    pub executed_order: Option<OrderFill>,
    pub closed_positions: usize,
}
