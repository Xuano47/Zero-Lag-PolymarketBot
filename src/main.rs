//! Polymarket Rust Arbitrage Bot — main entry point.
//!
//! Architecture:
//!   - tokio multi-thread runtime
//!   - N WebSocket tasks push PriceUpdates via mpsc::UnboundedSender (move semantics)
//!   - Single consumer task processes updates from channel, runs analyzer
//!   - Trade execution spawned as fire-and-forget tokio tasks
//!   - DashMap for per-market execution locks and cooldowns (replaces Go sync.Map)
//!   - Graceful shutdown on Ctrl-C

mod analyzer;
mod api;
mod config;
mod db;
mod executor;
mod models;
mod signer;

use crate::api::clob::ClobClient;
use crate::api::gamma::GammaClient;
use crate::api::websocket::spawn_ws_listener;
use crate::analyzer::ArbitrageAnalyzer;
use crate::config::Config;
use crate::db::DbManager;
use crate::executor::NativeExecutor;
use crate::models::{ArbitrageOpportunity, Market};
use crate::signer::Signer;

use anyhow::Result;
use chrono::Utc;
use dashmap::DashMap;
use rust_decimal::prelude::ToPrimitive;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Mutex, RwLock};
use tracing::{error, info, warn};

static TOTAL_PRICE_UPDATES: AtomicU64 = AtomicU64::new(0);
static TOTAL_ARB_ALERTS: AtomicU64 = AtomicU64::new(0);
static MARKET_COUNT: AtomicU64 = AtomicU64::new(0);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("=== Polymarket Engine (Rust) v1.0 ===");

    let cfg = Arc::new(Config::load()?);
    if cfg.dry_run {
        warn!("⚠️  DRY_RUN=true — orders will NOT be submitted");
    }

    let db = Arc::new(DbManager::new("data/rarb.db").await?);
    let signer = Arc::new(Signer::new(&cfg.private_key)?);
    info!("Signer address: {:#x}", signer.address);

    let clob = Arc::new(ClobClient::new(Arc::clone(&cfg), Arc::clone(&signer)));
    let gamma = GammaClient::new(Arc::clone(&cfg));
    let analyzer = Arc::new(ArbitrageAnalyzer::new(Arc::clone(&cfg)));
    let executor = Arc::new(NativeExecutor::new(Arc::clone(&clob)));

    // Per-market concurrency: DashMap replaces Go's sync.Map
    let exec_locks: Arc<DashMap<String, Arc<Mutex<()>>>> = Arc::new(DashMap::new());
    let cooldowns: Arc<DashMap<String, Instant>> = Arc::new(DashMap::new());
    let current_balance: Arc<Mutex<f64>> = Arc::new(Mutex::new(0.0));

    // Active markets shared state
    let active_markets: Arc<RwLock<Vec<Arc<Market>>>> = Arc::new(RwLock::new(Vec::new()));

    // WebSocket subscription channels (watch = latest-value-only, no queue build-up)
    let num_ws = cfg.num_ws_connections;
    let mut sub_txs: Vec<watch::Sender<Vec<String>>> = Vec::with_capacity(num_ws);
    let mut sub_rxs: Vec<watch::Receiver<Vec<String>>> = Vec::with_capacity(num_ws);
    for _ in 0..num_ws {
        let (tx, rx) = watch::channel(Vec::<String>::new());
        sub_txs.push(tx);
        sub_rxs.push(rx);
    }

    // Single price update channel: all WS tasks → one analyzer consumer
    let (price_tx, mut price_rx) = mpsc::unbounded_channel::<crate::models::PriceUpdate>();

    // Spawn WS listener tasks
    for (i, rx) in sub_rxs.into_iter().enumerate() {
        spawn_ws_listener(i, Vec::new(), price_tx.clone(), rx);
    }
    drop(price_tx); // tasks hold their own clones

    // ── Price update consumer ────────────────────────────────────────────────
    {
        let analyzer = Arc::clone(&analyzer);
        let exec_locks = Arc::clone(&exec_locks);
        let cooldowns = Arc::clone(&cooldowns);
        let current_balance = Arc::clone(&current_balance);
        let executor = Arc::clone(&executor);
        let db = Arc::clone(&db);
        let clob = Arc::clone(&clob);
        let dry_run = cfg.dry_run;

        tokio::spawn(async move {
            while let Some(update) = price_rx.recv().await {
                TOTAL_PRICE_UPDATES.fetch_add(1, Ordering::Relaxed);
                if let Some(opp) = analyzer.update_price(&update.asset_id, update.best_ask, update.size) {
                    spawn_trade(
                        opp,
                        Arc::clone(&clob),
                        Arc::clone(&executor),
                        Arc::clone(&db),
                        Arc::clone(&exec_locks),
                        Arc::clone(&cooldowns),
                        Arc::clone(&current_balance),
                        dry_run,
                    );
                }
            }
        });
    }

    // ── Periodic refresh task ────────────────────────────────────────────────
    {
        let gamma = gamma.clone();
        let analyzer = Arc::clone(&analyzer);
        let active_markets = Arc::clone(&active_markets);
        let sub_txs = Arc::new(sub_txs);
        let exec_locks = Arc::clone(&exec_locks);
        let cooldowns = Arc::clone(&cooldowns);
        let current_balance = Arc::clone(&current_balance);
        let executor = Arc::clone(&executor);
        let db = Arc::clone(&db);
        let clob = Arc::clone(&clob);
        let dry_run = cfg.dry_run;

        // Run initial refresh
        do_refresh(
            &gamma, &analyzer, &active_markets, &sub_txs, &clob,
            &exec_locks, &cooldowns, &current_balance, &executor, &db, dry_run,
        ).await;

        tokio::spawn(async move {
            let mut t_refresh = tokio::time::interval(Duration::from_secs(30));
            let mut t_balance = tokio::time::interval(Duration::from_secs(60));
            let mut t_stats = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = t_refresh.tick() => {
                        do_refresh(
                            &gamma, &analyzer, &active_markets, &sub_txs, &clob,
                            &exec_locks, &cooldowns, &current_balance, &executor, &db, dry_run,
                        ).await;
                    }
                    _ = t_balance.tick() => {
                        if let Ok(bal) = clob.get_balance().await {
                            *current_balance.lock().await = bal.to_f64().unwrap_or(0.0);
                        }
                    }
                    _ = t_stats.tick() => {
                        // Persist memory stats to DB for the dashboard
                        let _ = db.upsert_stat("markets_scanned", &MARKET_COUNT.load(Ordering::Relaxed).to_string()).await;
                        let _ = db.upsert_stat("price_updates", &TOTAL_PRICE_UPDATES.load(Ordering::Relaxed).to_string()).await;
                        let _ = db.upsert_stat("arbitrage_alerts", &TOTAL_ARB_ALERTS.load(Ordering::Relaxed).to_string()).await;
                        let bal = *current_balance.lock().await;
                        let _ = db.upsert_stat("balance", &bal.to_string()).await;
                    }
                }
            }
        });
    }

    // Graceful shutdown
    tokio::signal::ctrl_c().await?;
    info!("Shutting down — bye!");
    Ok(())
}

// ── Market refresh ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn do_refresh(
    gamma: &GammaClient,
    analyzer: &Arc<ArbitrageAnalyzer>,
    active_markets: &Arc<RwLock<Vec<Arc<Market>>>>,
    sub_txs: &Arc<Vec<watch::Sender<Vec<String>>>>,
    clob: &Arc<ClobClient>,
    exec_locks: &Arc<DashMap<String, Arc<Mutex<()>>>>,
    cooldowns: &Arc<DashMap<String, Instant>>,
    current_balance: &Arc<Mutex<f64>>,
    executor: &Arc<NativeExecutor>,
    db: &Arc<DbManager>,
    dry_run: bool,
) {
    let markets: Vec<Arc<Market>> = match gamma.fetch_active_markets().await {
        Ok(m) => m,
        Err(e) => { warn!("refresh error: {}", e); return; }
    };

    MARKET_COUNT.store(markets.len() as u64, Ordering::Relaxed);
    info!("📊 Loaded {} markets", markets.len());

    analyzer.load_markets(&markets);

    // Snapshot analysis on all markets right after refresh
    for m in &markets {
        if let Some(opp) = analyzer.analyze(&m.id) {
            spawn_trade(
                opp,
                Arc::clone(clob),
                Arc::clone(executor),
                Arc::clone(db),
                Arc::clone(exec_locks),
                Arc::clone(cooldowns),
                Arc::clone(current_balance),
                dry_run,
            );
        }
    }

    // Shard markets across N WS connections
    let n = sub_txs.len();
    if n > 0 {
        let per = (markets.len() + n - 1) / n;
        for (i, tx) in sub_txs.iter().enumerate() {
            let start = i * per;
            let end = ((i + 1) * per).min(markets.len());
            if start >= markets.len() { break; }
            let ids: Vec<String> = markets[start..end]
                .iter()
                .flat_map(|m| [m.yes_token.id.clone(), m.no_token.id.clone()])
                .take(500)
                .collect();
            let _ = tx.send(ids);
        }
    }

    *active_markets.write().await = markets;

    // Async balance fetch
    let clob2 = Arc::clone(clob);
    let bal = Arc::clone(current_balance);
    tokio::spawn(async move {
        if let Ok(b) = clob2.get_balance().await {
            *bal.lock().await = b.to_f64().unwrap_or(0.0);
            info!("💰 Balance: ${:.2}", b);
        }
    });
}

// ── Trade execution (fire-and-forget tokio task) ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn spawn_trade(
    opp: ArbitrageOpportunity,
    clob: Arc<ClobClient>,
    executor: Arc<NativeExecutor>,
    db: Arc<DbManager>,
    exec_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    cooldowns: Arc<DashMap<String, Instant>>,
    current_balance: Arc<Mutex<f64>>,
    dry_run: bool,
) {
    let mid = opp.market.id.clone();

    // Cooldown check (5s per market, identical to Go)
    if let Some(last) = cooldowns.get(&mid) {
        if last.elapsed() < Duration::from_secs(5) {
            return;
        }
    }

    // Per-market mutex — TryLock (non-blocking, identical to Go's lock.TryLock())
    let lock = exec_locks
        .entry(mid.clone())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();

    tokio::spawn(async move {
        let Ok(_guard) = lock.try_lock() else { return };
        cooldowns.insert(mid.clone(), Instant::now());

        let ts = Utc::now().to_rfc3339();
        let profit_pct_f = opp.profit_pct.to_f64().unwrap_or(0.0);
        let combined_f = opp.combined_cost.to_f64().unwrap_or(0.0);

        // Balance guard — Go FIX #9
        {
            let bal = *current_balance.lock().await;
            if bal > 0.0 && combined_f > 0.0 && bal < combined_f {
                warn!("⚠️ SKIP: balance {:.2} < cost {:.2}", bal, combined_f);
                return;
            }
        }

        TOTAL_ARB_ALERTS.fetch_add(1, Ordering::Relaxed);
        let _ = db.insert_alert(&ts, &opp.market.question, profit_pct_f, combined_f).await;
        info!("🔥 FIRE: {} ({:.2}%)", mid, profit_pct_f * 100.0);

        let result = match executor.execute(&opp, dry_run).await {
            Ok(r) => r,
            Err(e) => { error!("execute error: {}", e); return; }
        };

        tokio::time::sleep(Duration::from_secs(3)).await;

        let yes_f = opp.yes_ask.to_f64().unwrap_or(0.0);
        let no_f = opp.no_ask.to_f64().unwrap_or(0.0);
        let sz_f = opp.max_trade_size.to_f64().unwrap_or(0.0);

        let (filled_yes, status_yes) = check_fill(&clob, result.id_yes.as_deref()).await;
        let (filled_no, status_no) = check_fill(&clob, result.id_no.as_deref()).await;

        if filled_yes {
            if let Some(id) = &result.id_yes {
                let _ = db.insert_trade(&ts, &opp.market.question, "YES",
                    yes_f, sz_f, yes_f * sz_f, id).await;
            }
        }
        if filled_no {
            if let Some(id) = &result.id_no {
                let _ = db.insert_trade(&ts, &opp.market.question, "NO",
                    no_f, sz_f, no_f * sz_f, id).await;
            }
        }

        if !filled_yes || !filled_no {
            record_leg_risks(
                &db, &ts, &opp.market.question,
                filled_yes, filled_no,
                status_yes.as_deref(), status_no.as_deref(),
                result.id_yes.as_deref(), result.id_no.as_deref(),
            ).await;
            let reason = format!(
                "YES:{}|NO:{}",
                status_yes.as_deref().unwrap_or(""),
                status_no.as_deref().unwrap_or("")
            );
            let _ = db.insert_near_miss(&ts, &opp.market.question, &reason, profit_pct_f).await;
        }
    });
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn check_fill(clob: &ClobClient, order_id: Option<&str>) -> (bool, Option<String>) {
    let Some(id) = order_id else {
        return (false, Some("NO_ID".into()));
    };
    match clob.get_order_status(id).await {
        Err(_) => (false, Some("QUERY_ERR".into())),
        Ok((status, matched)) => {
            let s = status.to_uppercase();
            let filled = s == "FILLED" || s == "MATCHED"
                || (!matched.is_zero() && matched.is_sign_positive());
            (filled, Some(s))
        }
    }
}

async fn record_leg_risks(
    db: &DbManager,
    ts: &str, question: &str,
    filled_yes: bool, filled_no: bool,
    status_yes: Option<&str>, status_no: Option<&str>,
    id_yes: Option<&str>, id_no: Option<&str>,
) {
    // Go FIX #4: use correct order ID for each leg's leg_risk record
    if status_yes == Some("QUERY_ERR") {
        if let Some(id) = id_yes {
            let _ = db.insert_leg_risk(ts, question, "YES_UNCONFIRMED", id).await;
        }
    } else if filled_yes && !filled_no && status_no != Some("QUERY_ERR") {
        if let Some(id) = id_no {
            let _ = db.insert_leg_risk(ts, question, "NO_FAILED", id).await;
        }
    }

    if status_no == Some("QUERY_ERR") {
        if let Some(id) = id_no {
            let _ = db.insert_leg_risk(ts, question, "NO_UNCONFIRMED", id).await;
        }
    } else if filled_no && !filled_yes && status_yes != Some("QUERY_ERR") {
        if let Some(id) = id_yes {
            let _ = db.insert_leg_risk(ts, question, "YES_FAILED", id).await;
        }
    }
}
