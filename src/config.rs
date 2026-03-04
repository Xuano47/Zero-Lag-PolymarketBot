use anyhow::{Context, Result};
use rust_decimal::Decimal;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Config {
    pub private_key: String,
    pub wallet_address: String,
    pub funder_address: String,
    pub signature_type: u8,
    pub poly_api_key: String,
    pub poly_api_secret: String,
    pub poly_api_passphrase: String,
    pub max_position_size: Decimal,
    pub min_profit_threshold: Decimal,
    pub min_liquidity_usd: Decimal,
    pub max_liquidity_usd: Decimal,
    pub max_days_until_resolution: i64,
    pub exclude_crypto_minutes: bool,
    pub num_ws_connections: usize,
    pub slippage_padding: Decimal,
    pub liquidity_coefficient: Decimal,
    pub min_share_threshold: Decimal,
    pub dry_run: bool,
}

impl Config {
    pub fn load() -> Result<Self> {
        // Try .env from exe dir or cwd, same as Go version
        let _ = dotenvy::dotenv();

        let get = |key: &str| std::env::var(key).unwrap_or_default();
        let dec = |key: &str| -> Decimal {
            Decimal::from_str(&get(key)).unwrap_or(Decimal::ZERO)
        };

        let mut slippage_padding = dec("SLIPPAGE_PADDING");
        let mut max_position_size = dec("MAX_POSITION_SIZE");
        let mut min_profit_threshold = dec("MIN_PROFIT_THRESHOLD");
        let mut liquidity_coefficient = dec("LIQUIDITY_COEFFICIENT");
        let mut min_share_threshold = dec("MIN_SHARE_THRESHOLD");
        let mut max_liquidity_usd = dec("MAX_LIQUIDITY_USD");

        // Default values — identical to Go
        if slippage_padding.is_zero()      { slippage_padding = Decimal::from_str("0.0015").unwrap(); }
        if max_position_size.is_zero()     { max_position_size = Decimal::from(6); }
        if min_profit_threshold.is_zero()  { min_profit_threshold = Decimal::from_str("0.010").unwrap(); }
        if liquidity_coefficient.is_zero() { liquidity_coefficient = Decimal::from_str("0.5").unwrap(); }
        if min_share_threshold.is_zero()   { min_share_threshold = Decimal::from(5); }
        if max_liquidity_usd.is_zero()     { max_liquidity_usd = Decimal::from(1_000_000_000i64); }

        let max_days_str = get("MAX_DAYS_UNTIL_RESOLUTION");
        let max_days = max_days_str.parse::<i64>().unwrap_or(3).max(1);

        let sig_type = get("SIGNATURE_TYPE").parse::<u8>().unwrap_or(0);
        let exclude_crypto = get("EXCLUDE_CRYPTO_MINUTES").to_lowercase() == "true";
        let dry_run = get("DRY_RUN").to_lowercase() == "true";

        let private_key = get("PRIVATE_KEY");
        anyhow::ensure!(!private_key.is_empty(), "PRIVATE_KEY not set in .env");

        Ok(Config {
            private_key,
            wallet_address: get("WALLET_ADDRESS"),
            funder_address: get("FUNDER_ADDRESS"),
            signature_type: sig_type,
            poly_api_key: get("POLY_API_KEY"),
            poly_api_secret: get("POLY_API_SECRET"),
            poly_api_passphrase: get("POLY_API_PASSPHRASE"),
            max_position_size,
            min_profit_threshold,
            min_liquidity_usd: dec("MIN_LIQUIDITY_USD"),
            max_liquidity_usd,
            max_days_until_resolution: max_days,
            exclude_crypto_minutes: exclude_crypto,
            num_ws_connections: 4,
            slippage_padding,
            liquidity_coefficient,
            min_share_threshold,
            dry_run,
        })
    }

    /// The effective maker address (funder if set, otherwise signer/wallet)
    pub fn maker_address(&self) -> &str {
        if self.funder_address.is_empty() {
            &self.wallet_address
        } else {
            &self.funder_address
        }
    }
}
