//! Polymarket Gamma API client — market discovery and portfolio positions.

use crate::config::Config;
use crate::models::{Market, Token};
use anyhow::{Result, anyhow};
use chrono::{Duration, Utc};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::sync::Arc;
use std::time;
use tracing::{warn, debug};

const GAMMA_HOST: &str = "https://gamma-api.polymarket.com";

#[derive(Clone)]
pub struct GammaClient {
    cfg: Arc<Config>,
    http: Client,
}

// ── Raw deserialization shapes ────────────────────────────────────────────────
// We deserialize only the fields we need.

#[derive(Deserialize, Debug)]
pub struct RawMarket {
    pub id: Option<String>,
    #[serde(rename = "conditionId")]
    pub condition_id: Option<String>,
    pub question: Option<String>,
    pub slug: Option<String>,
    pub volume: Option<serde_json::Value>,
    pub liquidity: Option<serde_json::Value>,
    pub active: Option<bool>,
    #[serde(rename = "negRisk")]
    pub neg_risk: Option<serde_json::Value>, 
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(rename = "groupItemEndDate")]
    pub group_item_end_date: Option<String>,
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: Option<String>,
    #[serde(rename = "outcomePrices")]
    pub outcome_prices: Option<String>,
}

impl GammaClient {
    pub fn new(cfg: Arc<Config>) -> Self {
        let http = Client::builder()
            .timeout(time::Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()
            .expect("failed to build gamma http client");
        GammaClient { cfg, http }
    }

    /// Fetch all active binary markets matching configured filters.
    pub async fn fetch_active_markets(&self) -> Result<Vec<Arc<Market>>> {
        let cutoff = Utc::now() + Duration::days(self.cfg.max_days_until_resolution);
        let mut markets = Vec::new();
        let mut offset = 0usize;
        let limit = 100usize;

        loop {
            let url = format!(
                "{}/markets?active=true&closed=false&limit={}&offset={}",
                GAMMA_HOST, limit, offset
            );
            let resp = self.http.get(&url).send().await?;
            let status = resp.status();
            let text = resp.text().await?;
            
            if !status.is_success() {
                return Err(anyhow!("Gamma API error {}: {}", status, text));
            }

            let raw: Vec<RawMarket> = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    warn!("Failed to decode Gamma response: {}. Body snippet: {}", e, &text[..1000.min(text.len())]);
                    return Err(anyhow!("Gamma decode error: {}", e));
                }
            };
            
            let count = raw.len();

            for rm in raw {
                if let Some(m) = self.convert_market(rm, cutoff) {
                    markets.push(Arc::new(m));
                }
            }

            if count < limit {
                break;
            }
            offset += limit;
        }

        Ok(markets)
    }

    fn convert_market(&self, rm: RawMarket, cutoff: chrono::DateTime<Utc>) -> Option<Market> {
        let id = rm.id?;
        let active = rm.active.unwrap_or(false);
        if !active {
            return None;
        }

        // Parse token IDs from stringified JSON array
        let clob_tokens_raw = rm.clob_token_ids?;
        let clob_token_ids: Vec<String> = serde_json::from_str(&clob_tokens_raw).ok()?;
        if clob_token_ids.len() < 2 {
            return None;
        }

        // Parse prices if available
        let mut prices = Vec::new();
        if let Some(prices_raw) = rm.outcome_prices {
            let price_strs: Vec<String> = serde_json::from_str(&prices_raw).unwrap_or_default();
            for ps in price_strs {
                if let Ok(p) = ps.parse::<Decimal>() {
                    prices.push(p);
                }
            }
        }

        // Parse end_date for resolution filter
        let end_date_str = rm.group_item_end_date
            .as_deref()
            .or(rm.end_date.as_deref())
            .unwrap_or("");

        if !end_date_str.is_empty() {
            let safe_str = end_date_str.replace("Z", "+00:00");
            let parsed = chrono::DateTime::parse_from_rfc3339(&safe_str)
                .map(|dt| dt.with_timezone(&Utc))
                .or_else(|_| {
                    chrono::NaiveDate::parse_from_str(end_date_str, "%Y-%m-%d")
                        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
                });
                
            if let Ok(end) = parsed {
                let now = Utc::now();
                let diff = end.signed_duration_since(now).num_hours();
                if diff > self.cfg.max_days_until_resolution * 24 || diff < -24 {
                    return None; 
                }
            } else {
                return None; 
            }
        }

        let liquidity: Decimal = match rm.liquidity {
            Some(serde_json::Value::String(s)) => s.parse().unwrap_or(Decimal::ZERO),
            Some(serde_json::Value::Number(n)) => Decimal::from_str_exact(&n.to_string()).unwrap_or(Decimal::ZERO),
            _ => Decimal::ZERO,
        };
        
        let volume: Decimal = match rm.volume {
            Some(serde_json::Value::String(s)) => s.parse().unwrap_or(Decimal::ZERO),
            Some(serde_json::Value::Number(n)) => Decimal::from_str_exact(&n.to_string()).unwrap_or(Decimal::ZERO),
            _ => Decimal::ZERO,
        };

        if liquidity < self.cfg.min_liquidity_usd || (self.cfg.max_liquidity_usd > Decimal::ZERO && liquidity > self.cfg.max_liquidity_usd) {
            return None;
        }

        // Binary markets: Index 0 is YES, Index 1 is NO
        let yes_token = Token {
            id: clob_token_ids[0].clone(),
            outcome: "Yes".to_string(),
            price: if prices.len() > 0 { prices[0] } else { Decimal::ZERO },
        };
        let no_token = Token {
            id: clob_token_ids[1].clone(),
            outcome: "No".to_string(),
            price: if prices.len() > 1 { prices[1] } else { Decimal::ZERO },
        };

        let question = rm.question.unwrap_or_default();
        let slug = rm.slug.unwrap_or_default();

        if self.cfg.exclude_crypto_minutes && is_crypto_minute(&question, &slug) {
            return None;
        }

        let neg_risk = match rm.neg_risk {
            Some(serde_json::Value::Bool(b)) => b,
            Some(serde_json::Value::String(s)) => s == "true" || s == "1",
            Some(serde_json::Value::Number(n)) => n.as_f64() == Some(1.0),
            _ => false,
        };

        Some(Market {
            id,
            condition_id: rm.condition_id.unwrap_or_default(),
            question,
            slug,
            yes_token,
            no_token,
            volume,
            liquidity,
            active,
            neg_risk,
            end_date: chrono::DateTime::parse_from_rfc3339(&end_date_str.replace("Z", "+00:00"))
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        })
    }

    pub async fn get_positions(&self, address: &str) -> Result<Vec<serde_json::Value>> {
        let url = format!("https://data-api.polymarket.com/positions?user={}", address);
        let resp = self.http.get(&url).send().await?;
        let data: Vec<serde_json::Value> = resp.json().await.unwrap_or_default();
        Ok(data)
    }
}

fn is_crypto_minute(question: &str, slug: &str) -> bool {
    let q = question.to_lowercase();
    let s = slug.to_lowercase();
    
    let is_fast = q.contains("5 minute") || q.contains("15 minute") || q.contains("up or down") ||
                  s.contains("5m") || s.contains("15m") || s.contains("up-or-down");
                  
    if !is_fast { return false; }
    
    q.contains("bitcoin") || q.contains("btc") || q.contains("ethereum") || q.contains("eth") ||
    q.contains("solana") || q.contains("sol") || q.contains("ripple") || q.contains("xrp") ||
    s.contains("btc") || s.contains("eth") || s.contains("sol") || s.contains("xrp")
}
