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

    #[error("notification service error: {message}")]
    NotificationError { message: String },
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

/// Policy governing how the bot executes a valid entry signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DecisionPolicy {
    /// Automatically execute order immediately when signal & probability checks pass.
    #[default]
    Auto,
    /// Hold order as a pending proposal and notify via Telegram/CLI; execute only after `/approve`.
    ConfirmByChat,
    /// Skip all order executions (monitor/analysis only).
    Skip,
}

impl std::fmt::Display for DecisionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecisionPolicy::Auto => write!(f, "auto"),
            DecisionPolicy::ConfirmByChat => write!(f, "confirm_by_chat"),
            DecisionPolicy::Skip => write!(f, "skip"),
        }
    }
}

/// Position sizing mode determining how order notional USDT is calculated and bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SizingMode {
    /// Size proportionally using Half-Kelly & Expected Value (EV) probability edge.
    #[default]
    ProbabilityEv,
    /// Size based on `risk_per_trade_pct` and Stop-Loss distance.
    RiskPct,
    /// Allocate maximum affordable notional allowed by available balance, leverage, and `max_order_usdt`.
    MaxBalance,
    /// Allocate fixed minimum notional (`min_order_usdt`).
    FixedMin,
}

impl std::fmt::Display for SizingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SizingMode::ProbabilityEv => write!(f, "probability_ev"),
            SizingMode::RiskPct => write!(f, "risk_pct"),
            SizingMode::MaxBalance => write!(f, "max_balance"),
            SizingMode::FixedMin => write!(f, "fixed_min"),
        }
    }
}

/// Mathematical probability and Expected Value (EV) evaluation for a candidate trade.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbabilityAssessment {
    pub win_probability: f64,
    pub break_even_win_rate: f64,
    pub net_reward_to_risk: f64,
    pub expected_value_r: f64,
    pub half_kelly_fraction: f64,
    pub is_positive_ev: bool,
}

/// Pending order proposal held when `decision_policy = "confirm_by_chat"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingOrderProposal {
    pub request: TradeOrderRequest,
    pub notional_usdt: f64,
    pub probability: ProbabilityAssessment,
    pub reason: String,
    pub created_at_ms: i64,
}

fn default_min_order_usdt() -> f64 {
    10.0
}

fn default_require_positive_ev() -> bool {
    true
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
    #[serde(default)]
    pub decision_policy: DecisionPolicy,
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
            decision_policy: DecisionPolicy::Auto,
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
    #[serde(default)]
    pub sizing_mode: SizingMode,
    #[serde(default = "default_min_order_usdt")]
    pub min_order_usdt: f64,
    #[serde(default)]
    pub max_order_usdt: f64,
    #[serde(default = "default_require_positive_ev")]
    pub require_positive_ev: bool,
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
            sizing_mode: SizingMode::ProbabilityEv,
            min_order_usdt: 10.0,
            max_order_usdt: 0.0,
            require_positive_ev: true,
        }
    }
}

fn default_ai_system_prompt_env() -> String {
    "AI_SYSTEM_PROMPT".to_string()
}

/// Optional AI/LLM advisor settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiConfig {
    pub enabled: bool,
    pub endpoint_url: String,
    pub model: String,
    pub api_key_env: String,
    #[serde(default = "default_ai_system_prompt_env")]
    pub system_prompt_env: String,
    pub weight: f64,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint_url: "https://api.openai.com/v1/chat/completions".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key_env: "AI_API_KEY".to_string(),
            system_prompt_env: default_ai_system_prompt_env(),
            weight: 0.35,
        }
    }
}

/// Supported futures exchange providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExchangeProvider {
    #[default]
    Binance,
    Bybit,
    Okx,
}

impl std::fmt::Display for ExchangeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExchangeProvider::Binance => write!(f, "binance"),
            ExchangeProvider::Bybit => write!(f, "bybit"),
            ExchangeProvider::Okx => write!(f, "okx"),
        }
    }
}

/// Exchange environment variable names, provider, and endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExchangeCredentialsConfig {
    #[serde(default)]
    pub provider: ExchangeProvider,
    pub api_key_env: String,
    pub api_secret_env: String,
    pub testnet_base_url: String,
    pub live_base_url: String,
    pub recv_window_ms: u64,
}

impl Default for ExchangeCredentialsConfig {
    fn default() -> Self {
        Self {
            provider: ExchangeProvider::Binance,
            api_key_env: "BINANCE_API_KEY".to_string(),
            api_secret_env: "BINANCE_API_SECRET".to_string(),
            testnet_base_url: "https://testnet.binancefuture.com".to_string(),
            live_base_url: "https://fapi.binance.com".to_string(),
            recv_window_ms: 5_000,
        }
    }
}

/// Telegram Bot notification and remote command monitoring configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token_env: String,
    pub chat_id_env: String,
    pub api_base_url: String,
    pub notify_on_trade: bool,
    pub notify_on_cycle: bool,
    pub listen_commands: bool,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token_env: "TELEGRAM_BOT_TOKEN".to_string(),
            chat_id_env: "TELEGRAM_CHAT_ID".to_string(),
            api_base_url: "https://api.telegram.org".to_string(),
            notify_on_trade: true,
            notify_on_cycle: false,
            listen_commands: true,
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
    #[serde(default)]
    pub telegram: TelegramConfig,
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
            telegram: TelegramConfig::default(),
        }
    }
}

/// Accumulated continuous-learning context and auto-generated trading skills knowledge base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SkillKnowledgeBase {
    pub total_cycles: u64,
    pub bullish_cycles: u64,
    pub bearish_cycles: u64,
    pub winning_closes: u64,
    pub losing_closes: u64,
    pub cumulative_ev_r: f64,
    pub positive_ev_signals: u64,
    pub learned_rules: Vec<String>,
    pub recent_ai_insights: Vec<String>,
    pub last_updated_ms: i64,
}

/// Single validation checklist item produced when verifying configuration requirements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigCheckItem {
    pub field: String,
    pub passed: bool,
    pub message: String,
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
    #[serde(default)]
    pub pending_order: Option<PendingOrderProposal>,
    #[serde(default)]
    pub skill_knowledge: SkillKnowledgeBase,
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
            pending_order: None,
            skill_knowledge: SkillKnowledgeBase::default(),
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
    #[serde(default)]
    pub pending_order: Option<PendingOrderProposal>,
    #[serde(default)]
    pub probability: Option<ProbabilityAssessment>,
}
