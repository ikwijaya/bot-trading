use async_trait::async_trait;

use crate::core::domain::{
    AccountSnapshot, AiAssessment, BotState, Candle, DomainError, OrderFill, Position,
    TechnicalSignal, TradeOrderRequest,
};

/// Injected clock contract so core functions never read system time directly.
pub trait ClockPort: Send + Sync {
    /// Returns current UTC timestamp in milliseconds.
    fn now_ms(&self) -> i64;
}

/// Exchange abstraction for Binance USDⓈ-M Futures (Mainnet, Testnet, or Local Paper Simulator).
#[async_trait]
pub trait ExchangePort: Send + Sync {
    /// Fetches recent OHLCV klines for the given symbol and interval.
    async fn fetch_klines(
        &self,
        symbol: &str,
        interval: &str,
        limit: usize,
    ) -> Result<Vec<Candle>, DomainError>;

    /// Fetches current account balance and open positions.
    async fn fetch_account(&self, symbol: &str) -> Result<AccountSnapshot, DomainError>;

    /// Sets the target leverage on the exchange for the given symbol.
    async fn set_leverage(&self, symbol: &str, leverage: u32) -> Result<(), DomainError>;

    /// Places a futures trade order (entry with optional SL/TP or reduce-only exit).
    async fn place_order(&self, req: &TradeOrderRequest) -> Result<OrderFill, DomainError>;

    /// Closes all open positions for the given symbol immediately (emergency / panic close).
    async fn close_all_positions(&self, symbol: &str) -> Result<Vec<OrderFill>, DomainError>;
}

/// Optional AI/LLM market reasoning contract.
#[async_trait]
pub trait AiAdvisorPort: Send + Sync {
    /// Evaluates the technical indicator snapshot and recent price action using an AI advisor.
    async fn assess_market(
        &self,
        symbol: &str,
        signal: &TechnicalSignal,
        open_positions: &[Position],
    ) -> Result<Option<AiAssessment>, DomainError>;
}

/// Persistence contract for bot runtime state.
pub trait StateStorePort: Send + Sync {
    /// Loads the persisted bot state, or returns `None` if no state exists yet.
    fn load_state(&self) -> Result<Option<BotState>, DomainError>;

    /// Saves the updated bot state atomically.
    fn save_state(&self, state: &BotState) -> Result<(), DomainError>;
}
