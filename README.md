# Polymarket Arbitrage Engine (Rust Version)

[English](README.md) | [简体中文](README_CN.md)

> [!TIP]
> **Open source isn't easy. If this project inspired you or helped you make a profit, feel free to buy me a coffee! ☕**
> 
*   **Donation Address (BNB Chain):** `0xb5cac4ecb1168053bba4f725b92a423ab48d7018`
*   **Buy me a coffee:** ![Donation QR](./donation_qr.png)
>
> **“This repo proves why 99% of X influencers selling Polymarket arbitrage strategies are pure BS.”**
>
> **“If this Rust engine doesn't tip the scales in your favor, then Go or Python versions aren't even worth the download—latency is the only metric that matters here.”**
>
> **“The code and strategy are now fully open. Consider this a final release; future updates are unlikely. Also, don't delude yourself: if dual-leg arbitrage fails, throwing in a third or fourth leg won't save you.”**

---
### 🚀 Deployment & Infrastructure

Arbitrage requires extreme execution speed, which is why this software is built with **Rust** to ensure millisecond-level processing. For optimal performance, consider the following deployment strategy:

*   **Server Location**: Polymarket's matching engine is physically located in **London, UK**. To minimize Round-Trip Time (RTT), deploy your bot in a data center close to London.
*   **Recommended Region**: We highly recommend using servers in **Ireland** (e.g., AWS `eu-west-1`). This provides the best balance between low latency and regulatory compliance.
*   **Geographic Restrictions (Geo-blocking)**: Please note that **UK IP addresses are blocked** from accessing Polymarket services. Even though the servers are in London, you cannot use a UK-based server IP.
*   **Compliance Check**: Before selecting a deployment region, check the [Polymarket Restricted Regions List](https://docs.polymarket.com/api-reference/geoblock) to verify availability in your chosen area.

## Key Technical Achievements

*   **Zero-Clone Hot Path**: Utilizes `serde_json` to dynamically parse WebSocket market prices directly from byte streams (`&[u8]`). Message passing uses move semantics (`tokio::sync::mpsc`), completely eliminating heap allocations and memory copies in the critical path.
*   **EIP-712 Signature Consistency**: Strict validation of typed data hashing and `secp256k1` ECDSA signatures. Generated hashes and raw signature bytes perfectly match the official Python `eth-account` SDK, including Polygon-specific `v + 27` recovery bit adjustments.
*   **Predictable Concurrency Model**: Replaces Go's `sync.Map` and pointer-based locking with highly optimized, lock-sharded `DashMap` for execution locks and cooldowns. Analysis state uses `std::sync::RwLock` for lightning-fast parallel reads.
*   **Batch Order FOK Execution**: Synchronously builds both legs (YES and NO) payloads before touching the network, then fires them via Polymarket's `POST /orders` batch endpoint to minimize single-leg fill risk.

## Arbitrage Principle

> **Mathematical Logic**: In a binary outcome market (YES/NO), the sum of the prices of both outcomes should theoretically equal the payout amount ($1.00 USDC).
> 
> **Opportunity**: If `Price(YES) + Price(NO) < 1.00` (after accounting for exchange fees and slippage), an arbitrage opportunity exists.
> - **Example**: If `YES = $0.48` and `NO = $0.50`, the total cost is `$0.98`.
> - **Payout**: Regardless of the outcome, one of your positions will be worth `$1.00`, yielding a `$0.02` (2%) risk-free profit.
> 
> **The Bot's Role**: The engine monitors WebSocket price feeds in real-time, detects when the combined price falls below the `MIN_PROFIT_THRESHOLD`, and executes a simultaneous batch order for both outcomes to capture the spread.

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

   > [!NOTE]
   > **How to obtain addresses (For Email Login users):**
   > *   **WALLET_ADDRESS**: Click your profile (top right) -> Gear icon -> "Private Key" -> "Start Export". The address is located on the first line below the warning text (starting with "Before you continue"). Please keep your exported private key strictly confidential.
   > *   **FUNDER_ADDRESS**: Click your profile -> Gear icon -> "Developer Mode" to obtain it.

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
