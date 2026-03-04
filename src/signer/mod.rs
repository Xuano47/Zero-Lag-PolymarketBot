//! EIP-712 order signing for Polymarket CLOB.
//!
//! Critical invariants (must match Go version and Python golden standard):
//!   - salt = Unix timestamp seconds (i64) — NOT random u128, JS 53-bit limit
//!   - takerAmount (shares) = Truncate(4) decimal places
//!   - makerAmount (USDC)   = RoundUp(2) decimal places
//!   - token decimals       = value × 1_000_000 (6 decimals), integer
//!   - EIP-712 domain chainId = 137 (Polygon)
//!   - signature v byte     += 27 (Ethereum legacy format)
//!   - expiration = 0 (FOK orders must use 0)
//!   - nonce = 0, feeRateBps = 0

use alloy_primitives::{keccak256, Address, B256, U256};
use anyhow::{Context, Result};
use chrono::Utc;
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use secp256k1::{Message, SecretKey, SECP256K1};
use serde_json::{json, Value};
use std::str::FromStr;

// Contract addresses — Polymarket CTF Exchange on Polygon
pub const CTF_EXCHANGE: &str        = "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E";
pub const NEG_RISK_CTF_EXCHANGE: &str = "0xC5d563A36AE78145C45a50134d48A1215220f80a";
pub const ZERO_ADDRESS: &str        = "0x0000000000000000000000000000000000000000";

pub const SIDE_BUY: u8  = 0;
pub const SIDE_SELL: u8 = 1;

// ── EIP-712 type hashes ───────────────────────────────────────────────────────
// Precomputed from the canonical type strings (same as Go apitypes.HashStruct logic)

const DOMAIN_TYPE_STR: &str =
    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
const ORDER_TYPE_STR: &str =
    "Order(uint256 salt,address maker,address signer,address taker,uint256 tokenId,\
     uint256 makerAmount,uint256 takerAmount,uint256 expiration,uint256 nonce,\
     uint256 feeRateBps,uint8 side,uint8 signatureType)";

// ── Data structures ──────────────────────────────────────────────────────────

/// Raw numeric order data (pre-signature)
pub struct OrderData {
    pub salt: u64,             // Unix timestamp seconds
    pub maker: Address,
    pub signer: Address,
    pub taker: Address,
    pub token_id: U256,
    pub maker_amount: U256,    // USDC micro-units (×1e6), rounded up 2dp
    pub taker_amount: U256,    // share micro-units (×1e6), truncated 4dp
    pub expiration: U256,      // always 0 for FOK
    pub nonce: U256,           // always 0
    pub fee_rate_bps: U256,    // always 0
    pub side: u8,
    pub signature_type: u8,
}

/// The main signer — holds the private key and derived address
pub struct Signer {
    secret: SecretKey,
    pub address: Address,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Convert a decimal to token micro-units (×1_000_000), as U256.
/// No rounding applied here — caller is responsible for rounding before calling.
fn to_token_decimals(amount: Decimal) -> U256 {
    let micro = amount * Decimal::from(1_000_000u64);
    // Truncate any sub-micro residue, then convert to u128
    let micro_trunc = micro.trunc();
    let value: u128 = micro_trunc.to_u128().expect("amount fits in u128");
    U256::from(value)
}

/// keccak256 of a UTF-8 string
fn keccak_str(s: &str) -> B256 {
    keccak256(s.as_bytes())
}

/// Encode an address as left-padded 32-byte ABI word
fn abi_encode_address(addr: &Address) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[12..].copy_from_slice(addr.as_slice());
    buf
}

/// Encode a U256 as big-endian 32-byte ABI word
fn abi_encode_u256(v: U256) -> [u8; 32] {
    v.to_be_bytes()
}

/// Encode a u8 as left-padded 32-byte ABI word (uint8 ABI encoding)
fn abi_encode_u8(v: u8) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[31] = v;
    buf
}

// ── EIP-712 hashing ──────────────────────────────────────────────────────────

fn hash_domain(exchange: &str) -> B256 {
    let type_hash = keccak_str(DOMAIN_TYPE_STR);
    let name_hash = keccak_str("Polymarket CTF Exchange");
    let version_hash = keccak_str("1");
    let chain_id = U256::from(137u64);
    let verifying = exchange
        .parse::<Address>()
        .expect("valid exchange address");

    // ABI encode: domainTypeHash ‖ name ‖ version ‖ chainId ‖ verifyingContract
    let mut encoded = [0u8; 32 * 5];
    encoded[0..32].copy_from_slice(type_hash.as_slice());
    encoded[32..64].copy_from_slice(name_hash.as_slice());
    encoded[64..96].copy_from_slice(version_hash.as_slice());
    encoded[96..128].copy_from_slice(&abi_encode_u256(chain_id));
    encoded[128..160].copy_from_slice(&abi_encode_address(&verifying));

    keccak256(&encoded)
}

fn hash_order(order: &OrderData) -> B256 {
    let type_hash = keccak_str(ORDER_TYPE_STR);

    // Exact field order from Go implementation:
    // salt, maker, signer, taker, tokenId, makerAmount, takerAmount,
    // expiration, nonce, feeRateBps, side, signatureType
    let mut encoded = [0u8; 32 * 13];
    let chunks: &mut [[u8; 32]] = bytemuck::cast_slice_mut(&mut encoded);
    chunks[0]  = *type_hash.as_ref();
    chunks[1]  = abi_encode_u256(U256::from(order.salt));
    chunks[2]  = abi_encode_address(&order.maker);
    chunks[3]  = abi_encode_address(&order.signer);
    chunks[4]  = abi_encode_address(&order.taker);
    chunks[5]  = abi_encode_u256(order.token_id);
    chunks[6]  = abi_encode_u256(order.maker_amount);
    chunks[7]  = abi_encode_u256(order.taker_amount);
    chunks[8]  = abi_encode_u256(order.expiration);
    chunks[9]  = abi_encode_u256(order.nonce);
    chunks[10] = abi_encode_u256(order.fee_rate_bps);
    chunks[11] = abi_encode_u8(order.side);
    chunks[12] = abi_encode_u8(order.signature_type);

    keccak256(&encoded)
}

fn eip712_digest(domain_hash: B256, order_hash: B256) -> B256 {
    // \x19\x01 ‖ domainSeparator ‖ structHash
    let mut raw = [0u8; 66];
    raw[0] = 0x19;
    raw[1] = 0x01;
    raw[2..34].copy_from_slice(domain_hash.as_slice());
    raw[34..66].copy_from_slice(order_hash.as_slice());
    keccak256(&raw)
}

// ── Signer impl ──────────────────────────────────────────────────────────────

impl Signer {
    pub fn new(hex_key: &str) -> Result<Self> {
        let key = hex_key.strip_prefix("0x").unwrap_or(hex_key);
        let bytes = hex::decode(key).context("invalid hex private key")?;
        let secret = SecretKey::from_slice(&bytes).context("invalid secp256k1 private key")?;

        // Derive Ethereum address from public key
        let pk = secret.public_key(SECP256K1);
        let pk_bytes = pk.serialize_uncompressed(); // 65 bytes: 04 ‖ x ‖ y
        let hash = keccak256(&pk_bytes[1..]);       // keccak of x‖y (64 bytes)
        let address = Address::from_slice(&hash[12..]);

        Ok(Signer { secret, address })
    }

    pub fn sign_order(&self, order: &OrderData, neg_risk: bool) -> Result<String> {
        let exchange = if neg_risk { NEG_RISK_CTF_EXCHANGE } else { CTF_EXCHANGE };

        let domain_hash = hash_domain(exchange);
        let order_hash  = hash_order(order);
        let digest      = eip712_digest(domain_hash, order_hash);

        let msg = Message::from_digest(*digest.as_ref());
        let (rec_id, sig) = SECP256K1
            .sign_ecdsa_recoverable(&msg, &self.secret)
            .serialize_compact();

        // Reconstruct 65-byte signature: r ‖ s ‖ v
        let mut sig65 = [0u8; 65];
        sig65[..64].copy_from_slice(&sig);
        // v += 27 (Ethereum legacy, identical to Go: signature[64] += 27)
        sig65[64] = rec_id.to_i32() as u8 + 27;

        Ok(format!("0x{}", hex::encode(sig65)))
    }
}

// ── Order construction ───────────────────────────────────────────────────────

/// Generate salt: Unix timestamp seconds (NOT random — JS 53-bit int limit)
pub fn generate_salt() -> u64 {
    Utc::now().timestamp() as u64
}

/// Build a BUY order.
///
/// Rounding rules match Go exactly:
///   - size  → Truncate(4): floor to 4 decimal places (Go's .Truncate(4) = truncate toward zero)
///   - maker → RoundUp(2): always round away from zero (Go's .RoundUp(2))
pub fn build_buy_order(
    price: Decimal,
    size: Decimal,
    token_id: &str,
    maker: Address,
    signer_addr: Address,
    sig_type: u8,
) -> OrderData {
    // Truncate(4) = floor toward zero for 4 decimal places
    // IMPORTANT: This is NOT round-to-nearest. 3.14159 → 3.1415 (not 3.1416)
    let size_trunc = (size * Decimal::from(10000)).trunc() / Decimal::from(10000);

    // Calculate USDC cost at high-precision price, then round UP to 2dp
    // AwayFromZero for positive numbers = always rounds up = matches Go's .RoundUp(2)
    let raw_maker = (size_trunc * price).round_dp_with_strategy(
        2,
        RoundingStrategy::AwayFromZero,
    );

    let maker_amount = to_token_decimals(raw_maker);
    let taker_amount = to_token_decimals(size_trunc);

    let token_id_u256 = U256::from_str_radix(token_id, 10)
        .expect("token_id must be decimal string");

    let taker_addr = ZERO_ADDRESS.parse::<Address>().unwrap();

    OrderData {
        salt:           generate_salt(),
        maker,
        signer:         signer_addr,
        taker:          taker_addr,
        token_id:       token_id_u256,
        maker_amount,
        taker_amount,
        expiration:     U256::ZERO, // ⚠️ FOK must be 0
        nonce:          U256::ZERO,
        fee_rate_bps:   U256::ZERO,
        side:           SIDE_BUY,
        signature_type: sig_type,
    }
}

/// Serialize order + signature into the JSON payload Polymarket CLOB expects.
///
/// ⚠️  salt must remain a JSON number (integer), NOT a string.
///     Polymarket API expects an integer; stringifying breaks order payload.
pub fn order_to_json(order: &OrderData, signature: &str, owner: &str, order_type: &str) -> Value {
    let side = if order.side == SIDE_SELL { "SELL" } else { "BUY" };

    json!({
        "order": {
            "salt":          order.salt,            // ← number, not string
            "maker":         format!("{:#x}", order.maker),
            "signer":        format!("{:#x}", order.signer),
            "taker":         format!("{:#x}", order.taker),
            "tokenId":       order.token_id.to_string(),
            "makerAmount":   order.maker_amount.to_string(),
            "takerAmount":   order.taker_amount.to_string(),
            "expiration":    order.expiration.to_string(),
            "nonce":         order.nonce.to_string(),
            "feeRateBps":    order.fee_rate_bps.to_string(),
            "side":          side,
            "signatureType": order.signature_type as i32,
            "signature":     signature,
        },
        "owner":     owner,
        "orderType": order_type,
        "postOnly":  false,
    })
}

// ── Unit tests ───────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_to_token_decimals_usdc_round_up() {
        // 0.4708 × 10 = 4.708 shares → maker = 4.708 * 0.4708 price
        // Test that RoundUp(2) behaves correctly for USDC (makerAmount)
        let price = dec!(0.4708);
        let size  = dec!(10);
        let size_trunc = size.round_dp(4);
        let raw_maker  = (size_trunc * price)
            .round_dp_with_strategy(2, RoundingStrategy::AwayFromZero);
        // 4.708 → rounds up to 4.71
        assert_eq!(raw_maker, dec!(4.71));
    }

    #[test]
    fn test_size_truncate_is_floor_not_round() {
        // Go's Truncate(4) truncates toward zero (floor), not round-to-nearest.
        // 3.14159 → 3.1415 (floor), NOT 3.1416 (round-to-nearest)
        let size = Decimal::from_str("3.14159").unwrap();
        let floored = (size * Decimal::from(10000)).trunc() / Decimal::from(10000);
        assert_eq!(floored.to_string(), "3.1415");
    }

    #[test]
    fn test_salt_is_reasonable() {
        let salt = generate_salt();
        // Must be a reasonable Unix timestamp (after 2024-01-01)
        assert!(salt > 1_704_067_200u64, "salt must be Unix seconds, not random big int");
    }

    #[test]
    fn test_signer_address_derivation() {
        let pk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let signer = Signer::new(pk).unwrap();
        // matches eth_account: 0xFCAd0B19bB29D4674531d6f115237E16AfCE377c
        assert_eq!(
            signer.address.to_string().to_lowercase(),
            "0xfcad0b19bb29d4674531d6f115237e16afce377c"
        );
    }

    #[test]
    fn test_python_golden_standard() {
        // Inputs perfectly matching py_signer.py
        let pk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let signer = Signer::new(pk).unwrap();

        let token_id = U256::from(12345678901234567890u128);
        let maker_amount = U256::from(1000000u64);
        let taker_amount = U256::from(2000000u64);
        
        let order = OrderData {
            salt: 1700000000u64, // Changed from U256::from(1700000000u64) to u64
            maker: signer.address,
            signer: signer.address,
            taker: Address::ZERO,
            token_id,
            maker_amount,
            taker_amount,
            expiration: U256::ZERO,
            nonce: U256::ZERO,
            fee_rate_bps: U256::ZERO,
            side: 0u8, // BUY
            signature_type: 0u8, // EOA
        };

        let sig = signer.sign_order(&order, false).unwrap();

        // Python expected hash: 0xe749e388dfd2aaf7eb60cdca139d67109038d8de6fa09f374d2476259740e53a
        // The alloy hash struct should match it strictly
        let hash = hash_order(&order); // Changed from order.alloy_hash_struct() to hash_order(&order)
        let expected_hash = "e749e388dfd2aaf7eb60cdca139d67109038d8de6fa09f374d2476259740e53a"
            .parse::<B256>().unwrap();
        assert_eq!(hash, expected_hash, "Hash mismatch!");

        // Python expected sig: 0x61422d946e504a16d5a164390abf80cb5f7cc5b635f86e8e645c94abb3a7d20706b76414e33b015e9c9034e0658555f420f71d4dafb30f9c3efbf532cfb94baf1b
        // '1b' hex corresponds to 27 decimal
        let expected_sig = "0x61422d946e504a16d5a164390abf80cb5f7cc5b635f86e8e645c94abb3a7d20706b76414e33b015e9c9034e0658555f420f71d4dafb30f9c3efbf532cfb94baf1b";
        assert_eq!(sig, expected_sig, "Signature mismatch!");
    }
}
