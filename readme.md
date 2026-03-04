# Polymarket Arbitrage Engine (Rust Version)

A high-performance, ultra-low-latency arbitrage sniper bot for Polymarket, written entirely in Rust.

This is a complete ground-up rewrite of the original Go-based engine, designed to eliminate Garbage Collection (GC) pauses and achieve absolute predictability in execution latency. In the millisecond-sensitive world of arbitrage, Rust's memory management ensures that market WebSocket stream processing is free from the latency spikes common in GC-managed languages.

## Key Technical Achievements

*   **Zero-Clone Hot Path**: Utilizes `serde_json` to dynamically parse WebSocket market prices directly from byte streams (`&[u8]`). Message passing uses move semantics (`tokio::sync::mpsc`), completely eliminating heap allocations and memory copies in the critical path.
*   **EIP-712 Signature Consistency**: Strict validation of typed data hashing and `secp256k1` ECDSA signatures. Generated hashes and raw signature bytes perfectly match the official Python `eth-account` SDK, including Polygon-specific `v + 27` recovery bit adjustments.
*   **Predictable Concurrency Model**: Replaces Go's `sync.Map` and pointer-based locking with highly optimized, lock-sharded `DashMap` for execution locks and cooldowns. Analysis state uses `std::sync::RwLock` for lightning-fast parallel reads.
*   **Batch Order FOK Execution**: Synchronously builds both legs (YES and NO) payloads before touching the network, then fires them via Polymarket's `POST /orders` batch endpoint to minimize single-leg fill risk.

## Project Structure

```text
polymarket-rust/
├── Cargo.toml
└── src/
    ├── main.rs              # Tokio scheduler (multi-threaded runtime)
    ├── config.rs            # .env-based core configuration loader
    ├── models.rs            # Models for markets and arbitrage ops
    ├── signer/mod.rs        # EIP-712 cryptography and typed hashing
    ├── api/
    │   ├── clob.rs          # REST HTTP requests & batch ordering
    │   ├── gamma.rs         # GraphQL API & market discovery
    │   └── websocket.rs     # Zero-copy WS subscriptions & heartbeats
    ├── analyzer/mod.rs      # Mathematical logic for arbitrage triggers
    ├── executor/mod.rs      # Build and execute FOK dual-leg orders
    └── db/mod.rs            # Async SQLite persistence via sqlx
```

## Setup & Configuration

1. **Install Rust** (if not already installed):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
2. **Environment Variables**:
   Configure the `.env` file in the `polymarket-rust` root directory. It is loaded automatically. Ensure the following parameters are defined:
   ```env
   # API & Auth
   POLY_API_KEY=your_key
   POLY_API_SECRET=your_secret
   POLY_API_PASSPHRASE=your_passphrase
   PRIVATE_KEY=your_private_key(no_0x)
   WALLET_ADDRESS=your_0x_address
   FUNDER_ADDRESS=your_funder_address (optional, supports Proxy mode)
   SIGNATURE_TYPE=1                   # 1=EOA Signature, 0=Default
   
   # Trading Strategy Thresholds
   MIN_PROFIT_THRESHOLD=0.02          # Min profit margin (2%)
   MAX_POSITION_SIZE=6                # Max investment per leg (USDC)
   SLIPPAGE_PADDING=0.0015            # Slippage buffer (ensures FOK fills)
   LIQUIDITY_COEFFICIENT=0.5          # Allow consuming X% of current top depth
   MIN_SHARE_THRESHOLD=5              # Min shares to buy (avoids dust)
   MIN_LIQUIDITY_USD=5000             # Filter markets with liquidity below this
   MAX_LIQUIDITY_USD=50000            # Filter hyper-liquid "red ocean" markets
   MAX_DAYS_UNTIL_RESOLUTION=1        # Filter markets resolving too far in the future
   
   # Special Filters & Performance
   EXCLUDE_CRYPTO_MINUTES=true       # Automatically skip fast-trap BTC/ETH markets
   NUM_WS_CONNECTIONS=4               # Concurrent WebSocket connections for all markets
   
   # Safety Switch
   DRY_RUN=true                       # Set to false for live trading
   ```

## Common Commands

### Compilation
Build binaries optimized for the current machine:
```bash
cargo build --release
```

### 1. Trading Engine
**Foreground (Debug Mode)**:
```bash
cargo run --release
```

**Background (Recommended)**:
```bash
nohup ./target/release/polymarket-rust > bot.log 2>&1 &
```

**Stop Engine**:
```bash
pkill polymarket-rust
# If pkill doesn't respond, use force mode:
ps aux | grep polymarket-rust | grep -v grep | awk '{print $2}' | xargs kill -9
```

### 2. Dashboard Program
**Start Dashboard**:
```bash
cd dashboard
source ../../venv/bin/activate  # Uses virtualenv from project root
nohup gunicorn -w 2 -b 127.0.0.1:8081 app:app > dashboard.log 2>&1 &
```

**Stop Dashboard**:
```bash
pkill gunicorn
```

### 3. Monitoring & Logs
**Watch Trading Logs**:
```bash
tail -f bot.log
```

**Watch Dashboard Logs**:
```bash
tail -f dashboard/dashboard.log
```

## Audit Logs (SQLite)

The engine persists all Alerts, Trades, Leg Risks, and Near Misses to a local SQLite file: `data/rarb.db`.

The schema is identical to the original Go version, allowing you to use existing dashboards or monitoring tools.

### Special Note on FOK Rejects & Failures
If you see entries for `FOK Rejects` or `Failures` in the logs/dashboard, **do not panic**. This is a safety feature:
*   **Audit logs, not losses**: These entries represent missed opportunities. They mean the bot detected an arb and attempted it, but failed to fill within slippage limits due to competition.
*   **FOK (Fill-Or-Kill) Protection**: The engine mandates FOK orders combined with Batch Execution. **Either both legs fill simultaneously, or the entire order is cancelled by the exchange.**
*   **No Single-Leg Risk**: When you see these rejects, **no funds have left your wallet, and no positions were opened**. It just means "I fired, but didn't hit."

### Status Diagnostic Table (Understanding Risk)
When you see `YES:xxx|NO:xxx` in the logs, use this table to assess status:

| Log Status | Risk Level | Meaning & Suggestion |
| :--- | :--- | :--- |
| `YES:FILLED\|NO:FILLED` | ✅ **Safe** | Hedged successfully. Profit locked in. |
| `YES:REJECTED\|NO:REJECTED` | ✅ **Safe** | FOK triggered. Nothing bought, 0 loss, 0 risk. |
| **`QUERY_ERR\|QUERY_ERR`** | 🟡 **Note** | **Unknown Status**. Usually network jitter. Likely no fill, but check balance to confirm. |
| **`FILLED\|REJECTED`** | 🔴 **Risk** | **Single-leg fill!** One side filled, the other failed. Manually hedge immediately. |
| **`FILLED\|QUERY_ERR`** | 💀 **Critical** | **Suspected single-leg!** Highly dangerous; verify your positions on the website immediately. |

> [!TIP]
> As long as one side is `FILLED` and the other **is not** `FILLED`, a single-leg risk exists. If both are `QUERY_ERR` or `REJECTED`, you are generally safe.
