use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Outcome token (YES or NO)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub id: String,
    pub outcome: String,
    pub price: Decimal,
}

/// Binary prediction market
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub id: String,
    pub condition_id: String,
    pub question: String,
    pub slug: String,
    pub yes_token: Token,
    pub no_token: Token,
    pub volume: Decimal,
    pub liquidity: Decimal,
    pub active: bool,
    pub neg_risk: bool,
    pub end_date: DateTime<Utc>,
}

/// A confirmed arbitrage opportunity, ready for execution
#[derive(Debug, Clone)]
pub struct ArbitrageOpportunity {
    /// Arc so analyzer → executor transfer is zero-copy
    pub market: Arc<Market>,
    pub yes_ask: Decimal,
    pub no_ask: Decimal,
    pub combined_cost: Decimal,
    pub profit_pct: Decimal,
    pub max_trade_size: Decimal,
    pub timestamp: DateTime<Utc>,
}

/// Minimal price update pushed from WebSocket hot path
/// Intentionally tiny: 1 heap-allocated String + 2 stack Decimals
#[derive(Debug)]
pub struct PriceUpdate {
    pub asset_id: String,
    pub best_ask: Decimal,
    pub size: Decimal,
}
