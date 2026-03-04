//! Arbitrage opportunity analyzer.
//!
//! Maintains per-market state (YES/NO ask prices) and evaluates
//! whether the combined cost of buying both legs is < $1.00 (risk-free profit).
//!
//! Thresholds, padding, and rounding are identical to the Go version.
//! Concurrency model: std::sync::RwLock<HashMap> — fast for read-heavy workloads.
//! UpdatePrice holds a WRITE lock and calls analyze_unlocked within it,
//! eliminating the Unlock→RLock race window (Go FIX #6 equivalent).

use crate::config::Config;
use crate::models::{ArbitrageOpportunity, Market};
use chrono::Utc;
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// Rejection reason strings (mirrors Go constants)
pub const REASON_NONE: &str                = "";
pub const REASON_NOT_READY: &str           = "DATA_NOT_READY";
pub const REASON_MIN_PRICE: &str           = "PRICE_BELOW_0.08_SAFE_ZONE";
pub const REASON_NO_PROFIT: &str           = "NO_PROFIT";
pub const REASON_LOW_PROFIT: &str          = "PROFIT_BELOW_THRESHOLD";
pub const REASON_INSUFFICIENT_DEPTH: &str  = "INSUFFICIENT_LIQUIDITY";
pub const REASON_MIN_SHARES: &str          = "MIN_SHARES_REQUIREMENT";
pub const REASON_PADDING_BREACH: &str      = "SLIPPAGE_BUFFER_TOO_THIN";

struct MarketState {
    /// Shared (Arc) pointer — analyzer → executor with zero-copy
    market: Arc<Market>,
    yes_ask: Decimal,
    no_ask: Decimal,
    yes_size: Decimal,
    no_size: Decimal,
    yes_ready: bool,
    no_ready: bool,
    /// Has received at least one live WS price update
    armed: bool,
}

pub struct ArbitrageAnalyzer {
    cfg: Arc<Config>,
    /// marketId → MarketState
    states: RwLock<HashMap<String, MarketState>>,
    /// tokenId → marketId  (points into states)
    token_map: RwLock<HashMap<String, String>>,
    boot_time: chrono::DateTime<Utc>,
}

impl ArbitrageAnalyzer {
    pub fn new(cfg: Arc<Config>) -> Self {
        ArbitrageAnalyzer {
            cfg,
            states: RwLock::new(HashMap::new()),
            token_map: RwLock::new(HashMap::new()),
            boot_time: Utc::now(),
        }
    }

    /// Load/refresh the active market list.
    /// Markets that are no longer active are pruned from state.
    pub fn load_markets(&self, markets: &[Arc<Market>]) {
        let mut states   = self.states.write().unwrap();
        let mut tok_map  = self.token_map.write().unwrap();

        let active_ids: std::collections::HashSet<&str> =
            markets.iter().map(|m| m.id.as_str()).collect();

        // Prune stale markets
        states.retain(|id, state| {
            if !active_ids.contains(id.as_str()) {
                tok_map.remove(&state.market.yes_token.id);
                tok_map.remove(&state.market.no_token.id);
                false
            } else {
                true
            }
        });

        // Insert or update markets
        for m in markets {
            let entry = states.entry(m.id.clone()).or_insert_with(|| {
                let mut s = MarketState {
                    market:    Arc::clone(m),
                    yes_ask:   Decimal::ZERO,
                    no_ask:    Decimal::ZERO,
                    yes_size:  Decimal::from(100),
                    no_size:   Decimal::from(100),
                    yes_ready: false,
                    no_ready:  false,
                    armed:     false,
                };
                // Pre-seed from REST snapshot prices
                if !m.yes_token.price.is_zero() {
                    s.yes_ask   = m.yes_token.price;
                    s.yes_ready = true;
                }
                if !m.no_token.price.is_zero() {
                    s.no_ask  = m.no_token.price;
                    s.no_ready = true;
                }
                tok_map.insert(m.yes_token.id.clone(), m.id.clone());
                tok_map.insert(m.no_token.id.clone(), m.id.clone());
                s
            });

            // If market is not yet armed by live WS data, update its metadata
            if !entry.armed {
                entry.market = Arc::clone(m);
            }
        }
    }

    /// Process a live price update from WebSocket.
    ///
    /// Holds the WRITE lock for the entire call including analysis,
    /// eliminating the Unlock→RLock race (Go FIX #6 equivalent).
    /// Returns Some(opportunity) if profitable trade detected.
    pub fn update_price(
        &self,
        asset_id: &str,
        best_ask: Decimal,
        size: Decimal,
    ) -> Option<ArbitrageOpportunity> {
        let mut states  = self.states.write().unwrap();
        let tok_map     = self.token_map.read().unwrap();

        let market_id = tok_map.get(asset_id)?;
        let state = states.get_mut(market_id)?;
        state.armed = true;

        if asset_id == state.market.yes_token.id {
            if !best_ask.is_zero() {
                state.yes_ask  = best_ask;
                state.yes_size = size;
                state.yes_ready = true;
            }
        } else {
            if !best_ask.is_zero() {
                state.no_ask  = best_ask;
                state.no_size = size;
                state.no_ready = true;
            }
        }

        analyze_unlocked(state, &self.cfg, self.boot_time)
    }

    /// On-demand analysis for a specific market (called during refresh).
    pub fn analyze(&self, market_id: &str) -> Option<ArbitrageOpportunity> {
        let states = self.states.read().unwrap();
        let state  = states.get(market_id)?;
        analyze_unlocked(state, &self.cfg, self.boot_time)
    }
}

/// Core arbitrage logic — caller holds any lock (R or W).
/// All thresholds identical to Go v12.5 analyzeUnlocked.
fn analyze_unlocked(
    state: &MarketState,
    cfg: &Config,
    boot_time: chrono::DateTime<Utc>,
) -> Option<ArbitrageOpportunity> {
    let grace_period = Utc::now().signed_duration_since(boot_time) < chrono::Duration::minutes(5);

    if !state.yes_ready || !state.no_ready {
        return None;
    }
    if !state.armed && !grace_period {
        return None;
    }

    // V11.6 Rational Redline: prune anything below $0.08
    let min_price = dec!(0.08);
    if state.yes_ask < min_price || state.no_ask < min_price {
        return None;
    }

    let raw_combined = state.yes_ask + state.no_ask;
    if raw_combined.is_zero() || raw_combined >= Decimal::ONE {
        return None;
    }

    // Check raw profit threshold FIRST before applying padding
    let raw_profit_pct = (Decimal::ONE - raw_combined) / raw_combined;
    if raw_profit_pct < cfg.min_profit_threshold {
        return None;
    }

    // Trade size: min(available_shares * coeff, max_affordable_at_raw_price)
    let available_shares = state.yes_size.min(state.no_size) * cfg.liquidity_coefficient;
    let max_affordable   = cfg.max_position_size / raw_combined;
    let trade_size       = available_shares.min(max_affordable);

    if trade_size < cfg.min_share_threshold {
        return None;
    }

    // Truncate to 4 decimal places (matches Go's Truncate(4))
    let final_size = (trade_size * Decimal::from(10000)).trunc() / Decimal::from(10000);
    if final_size.is_zero() {
        return None;
    }

    // Apply slippage padding (50% unified coefficient)
    let padding_factor = Decimal::ONE + cfg.slippage_padding;
    let padded_yes = (state.yes_ask * padding_factor)
        .round_dp_with_strategy(4, RoundingStrategy::AwayFromZero);
    let padded_no  = (state.no_ask  * padding_factor)
        .round_dp_with_strategy(4, RoundingStrategy::AwayFromZero);

    // Final safety check: even with full slippage, must not pay >= $1.00
    let padded_combined = padded_yes + padded_no;
    if padded_combined >= Decimal::ONE {
        return None;
    }

    let actual_profit_pct = (Decimal::ONE - padded_combined) / padded_combined;
    let actual_cost = (padded_yes * final_size) + (padded_no * final_size);

    Some(ArbitrageOpportunity {
        market:       Arc::clone(&state.market),   // Arc clone — just a reference count bump
        yes_ask:      padded_yes,
        no_ask:       padded_no,
        combined_cost: actual_cost,
        profit_pct:   actual_profit_pct,
        max_trade_size: final_size,
        timestamp:    Utc::now(),
    })
}
