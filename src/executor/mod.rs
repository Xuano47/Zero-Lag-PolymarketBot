//! Order executor — builds and fires dual-leg (YES + NO) batch FOK orders.

use crate::api::clob::ClobClient;
use crate::models::ArbitrageOpportunity;
use anyhow::Result;
use std::sync::Arc;
use tracing::{info, warn};

pub struct ExecutionResult {
    pub success: bool,
    pub id_yes: Option<String>,
    pub id_no: Option<String>,
    pub error: Option<String>,
}

pub struct NativeExecutor {
    clob: Arc<ClobClient>,
}

impl NativeExecutor {
    pub fn new(clob: Arc<ClobClient>) -> Self {
        NativeExecutor { clob }
    }

    /// Execute a dual-leg arbitrage opportunity via the batch order endpoint.
    ///
    /// Strategy:
    ///   1. Pre-build *both* order payloads (signing) before touching the network.
    ///      This minimises the time-gap between YES and NO order acceptance.
    ///   2. Fire both in a single POST /orders (batch).
    ///   3. Detect single-leg failure.
    pub async fn execute(&self, opp: &ArbitrageOpportunity, dry_run: bool) -> Result<ExecutionResult> {
        let neg_risk = opp.market.neg_risk;

        // Step 1: Build both payloads synchronously (CPU-bound, no await)
        let yes_payload = self.clob.create_order_payload(
            &opp.market.yes_token.id,
            opp.yes_ask,
            opp.max_trade_size,
            neg_risk,
            "FOK",
        )?;

        let no_payload = self.clob.create_order_payload(
            &opp.market.no_token.id,
            opp.no_ask,
            opp.max_trade_size,
            neg_risk,
            "FOK",
        )?;

        let profit_pct = opp.profit_pct.round_dp(4);
        info!(
            "🚀 Firing Batch Orders [YES: {}, NO: {}] | Size: {} | Profit: {}%",
            opp.yes_ask, opp.no_ask, opp.max_trade_size, profit_pct
        );

        // Dry-run mode: log payload but do not submit
        if dry_run {
            info!("DRY_RUN YES payload: {}", yes_payload);
            info!("DRY_RUN NO  payload: {}", no_payload);
            return Ok(ExecutionResult {
                success: true,
                id_yes: Some("DRY_RUN".into()),
                id_no:  Some("DRY_RUN".into()),
                error:  None,
            });
        }

        // Step 2: Submit both legs atomically via POST /orders
        let ids = self
            .clob
            .post_batch_orders(&[yes_payload, no_payload])
            .await
            .map_err(|e| {
                warn!("⚠️ Batch Execute Failed: {}", e);
                e
            })?;

        let id_yes = ids.get(0).and_then(|x| x.clone());
        let id_no  = ids.get(1).and_then(|x| x.clone());

        // Both legs rejected
        if id_yes.is_none() && id_no.is_none() {
            return Ok(ExecutionResult {
                success: false,
                id_yes:  None,
                id_no:   None,
                error:   Some("both legs rejected by matching engine".into()),
            });
        }

        // Detect partial failure
        let error = match (&id_yes, &id_no) {
            (None, Some(_)) => {
                warn!("⚠️ YES leg rejected natively");
                Some("YES leg rejected natively".into())
            }
            (Some(_), None) => {
                warn!("⚠️ NO leg rejected natively");
                Some("NO leg rejected natively".into())
            }
            _ => None,
        };

        let success = id_yes.is_some() && id_no.is_some();
        Ok(ExecutionResult { success, id_yes, id_no, error })
    }
}
