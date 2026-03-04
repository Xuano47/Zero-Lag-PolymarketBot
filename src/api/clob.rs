//! Polymarket CLOB REST API client.
//!
//! All requests carry L2 authentication headers (HMAC-SHA256, base64url),
//! exactly matching the Go implementation in clob.go.

use crate::config::Config;
use crate::signer::{build_buy_order, order_to_json, Signer};
use alloy_primitives::Address;
use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE, Engine};
use hmac::{Hmac, Mac};
use reqwest::{Client, StatusCode};
use rust_decimal::Decimal;
use serde_json::Value;
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;

const CLOB_HOST: &str = "https://clob.polymarket.com";

pub struct ClobClient {
    cfg: Arc<Config>,
    http: Client,          // reqwest::Client is Arc internally — share freely
    signer: Arc<Signer>,
}

impl ClobClient {
    pub fn new(cfg: Arc<Config>, signer: Arc<Signer>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)           // disable Nagle — critical for latency
            .build()
            .expect("failed to build reqwest client");

        ClobClient { cfg, http, signer }
    }

    // ── L2 Authentication ────────────────────────────────────────────────────

    /// Build Polymarket L2 HMAC-SHA256 auth headers.
    /// Message format: timestamp ‖ method ‖ path ‖ body  (identical to Go)
    fn l2_headers(
        &self,
        method: &str,
        sign_path: &str,
        body: &str,
    ) -> Result<reqwest::header::HeaderMap> {
        let ts = chrono::Utc::now().timestamp().to_string();
        let msg = format!("{}{}{}{}", ts, method, sign_path, body);

        // Pad base64url secret to multiple of 4 (same as Go)
        let mut secret_str = self.cfg.poly_api_secret.clone();
        while secret_str.len() % 4 != 0 {
            secret_str.push('=');
        }
        let secret = URL_SAFE
            .decode(&secret_str)
            .context("invalid base64 API secret")?;

        let mut mac = Hmac::<Sha256>::new_from_slice(&secret)
            .context("HMAC key error")?;
        mac.update(msg.as_bytes());
        let sig = URL_SAFE.encode(mac.finalize().into_bytes());

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("POLY_ADDRESS",    self.cfg.wallet_address.parse()?);
        headers.insert("POLY_SIGNATURE",  sig.parse()?);
        headers.insert("POLY_TIMESTAMP",  ts.parse()?);
        headers.insert("POLY_API_KEY",    self.cfg.poly_api_key.parse()?);
        headers.insert("POLY_PASSPHRASE", self.cfg.poly_api_passphrase.parse()?);
        headers.insert("Content-Type",    "application/json".parse()?);
        Ok(headers)
    }

    // ── Public API methods ───────────────────────────────────────────────────

    pub async fn get_balance(&self) -> Result<Decimal> {
        let sign_path = "/balance-allowance";
        // ⚠️ Polymarket signs only the path, not query params (same as Go comment)
        let full_path = format!(
            "{}?asset_type=COLLATERAL&signature_type={}",
            sign_path, self.cfg.signature_type
        );
        let headers = self.l2_headers("GET", sign_path, "")?;
        let resp = self
            .http
            .get(format!("{}{}", CLOB_HOST, full_path))
            .headers(headers)
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;
        let balance_str = body["balance"].as_str().unwrap_or("0");
        let raw: Decimal = balance_str.parse().unwrap_or(Decimal::ZERO);
        // Convert from micro-USDC to USDC
        Ok(raw / Decimal::from(1_000_000u64))
    }

    /// Returns (status, size_matched)
    pub async fn get_order_status(&self, order_id: &str) -> Result<(String, Decimal)> {
        let sign_path = format!("/order/{}", order_id);
        let headers = self.l2_headers("GET", &sign_path, "")?;

        let resp = self
            .http
            .get(format!("{}{}", CLOB_HOST, sign_path))
            .headers(headers)
            .send()
            .await?;

        if resp.status() != StatusCode::OK {
            anyhow::bail!("http_{}", resp.status().as_u16());
        }

        let body: serde_json::Value = resp.json().await?;
        let status = body["status"].as_str().unwrap_or("").to_string();
        let matched: Decimal = body["size_matched"]
            .as_str()
            .unwrap_or("0")
            .parse()
            .unwrap_or(Decimal::ZERO);

        Ok((status, matched))
    }

    /// Build a signed order payload without submitting it.
    /// Separating build from submit lets executor pre-build both legs
    /// before ANY network call, minimising the time between YES and NO submission.
    pub fn create_order_payload(
        &self,
        token_id: &str,
        price: Decimal,
        size: Decimal,
        neg_risk: bool,
        order_type: &str,
    ) -> Result<Value> {
        let maker_addr: Address = if self.cfg.funder_address.is_empty() {
            self.signer.address
        } else {
            self.cfg.funder_address.parse().context("invalid funder address")?
        };

        let order = build_buy_order(
            price,
            size,
            token_id,
            maker_addr,
            self.signer.address,
            self.cfg.signature_type,
        );

        let sig = self.signer.sign_order(&order, neg_risk)?;
        let payload = order_to_json(&order, &sig, &self.cfg.poly_api_key, order_type);
        Ok(payload)
    }

    /// Submit a single order (POST /order). Used for testing only.
    pub async fn post_order(
        &self,
        token_id: &str,
        price: Decimal,
        size: Decimal,
        neg_risk: bool,
        order_type: &str,
    ) -> Result<String> {
        let payload = self.create_order_payload(token_id, price, size, neg_risk, order_type)?;
        let body_str = serde_json::to_string(&payload)?;

        let headers = self.l2_headers("POST", "/order", &body_str)?;
        let resp = self
            .http
            .post(format!("{}/order", CLOB_HOST))
            .headers(headers)
            .body(body_str)
            .send()
            .await?;

        let result: Value = resp.json().await?;
        extract_order_id(&result)
            .ok_or_else(|| anyhow::anyhow!("reject: {}", result))
    }

    /// Submit multiple orders atomically (POST /orders — batch endpoint).
    ///
    /// Payloads are pre-built by the caller before this call so the only
    /// latency here is network + exchange matching, not signing.
    ///
    /// Returns a Vec of Option<String>: Some(id) on acceptance, None on per-leg rejection.
    pub async fn post_batch_orders(&self, payloads: &[Value]) -> Result<Vec<Option<String>>> {
        if payloads.is_empty() {
            return Ok(vec![]);
        }

        let body_str = serde_json::to_string(payloads)?;
        let headers = self.l2_headers("POST", "/orders", &body_str)?;

        let resp = self
            .http
            .post(format!("{}/orders", CLOB_HOST))
            .headers(headers)
            .body(body_str)
            .send()
            .await?;

        let resp_body = resp.bytes().await?;

        // Batch API returns an array of per-order results.
        // If top-level error (e.g. bad auth), it returns a JSON object instead.
        let batch: Vec<Value> = serde_json::from_slice(&resp_body)
            .with_context(|| format!("batch_reject: {}", String::from_utf8_lossy(&resp_body)))?;

        let ids = batch
            .into_iter()
            .map(|v| extract_order_id(&v))
            .collect();

        Ok(ids)
    }
}

/// Extract order ID from either {"orderID": ...} or {"id": ...} response shape.
fn extract_order_id(v: &Value) -> Option<String> {
    v["orderID"]
        .as_str()
        .or_else(|| v["id"].as_str())
        .map(str::to_owned)
}
