//! WebSocket price feed from Polymarket's market subscription endpoint.
//!
//! Zero-clone hot path:
//!   - Raw bytes from WS frame are parsed in-place via serde_json::from_slice(&[u8])
//!   - PriceUpdate carries only asset_id (String move), best_ask, size — ~40 bytes
//!   - PriceUpdate is sent via mpsc::UnboundedSender (move semantics, zero copy)
//!   - Only SELL-side (ASK) events trigger analysis (Go FIX #1 equivalent)

use crate::models::PriceUpdate;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_tungstenite::{
    connect_async,
    tungstenite::Message,
    MaybeTlsStream, WebSocketStream,
};
use tokio::net::TcpStream;
use tracing::{error, info, warn};

const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
const PING_INTERVAL_SECS: u64 = 20;

// ── Wire format (only fields we read) ────────────────────────────────────────

#[derive(Deserialize)]
struct WsEnvelope {
    #[serde(rename = "event_type")]
    event_type: Option<String>,
    #[serde(rename = "price_changes")]
    price_changes: Option<Vec<PriceChange>>,
}

#[derive(Deserialize)]
struct PriceChange {
    asset_id: String,
    side: String,
    price: Option<String>,
    size: Option<String>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Spawn a self-reconnecting WebSocket listener for a shard of asset IDs.
///
/// Price updates are pushed into `tx` via move semantics — no clone on hot path.
/// The `sub_rx` watch channel carries the latest subscription list — the listener
/// reconnects when the list changes, clearing stale zombie subscriptions.
pub fn spawn_ws_listener(
    conn_id: usize,
    initial_ids: Vec<String>,
    tx: mpsc::UnboundedSender<PriceUpdate>,
    mut sub_rx: tokio::sync::watch::Receiver<Vec<String>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut current_ids = initial_ids;

        loop {
            // Connect
            let ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>> =
                match connect_async(WS_URL).await {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        warn!("[WS {}] connect error: {}, retrying in 1s", conn_id, e);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

            info!("[WS {}] Connected", conn_id);
            let (mut write, mut read) = ws_stream.split();

            // Subscribe to current shard
            if !current_ids.is_empty() {
                let sub_msg = serde_json::json!({
                    "assets_ids": current_ids,
                    "type": "market"
                });
                if let Ok(txt) = serde_json::to_string(&sub_msg) {
                    let _ = write.send(Message::Text(txt.into())).await;
                }
            }

            let mut ping_ticker = interval(Duration::from_secs(PING_INTERVAL_SECS));

            'inner: loop {
                tokio::select! {
                    // Heartbeat
                    _ = ping_ticker.tick() => {
                        if write.send(Message::Ping(vec![].into())).await.is_err() {
                            break 'inner;
                        }
                    }

                    // Re-subscription: new market shard from orchestrator
                    _ = sub_rx.changed() => {
                        let new_ids = sub_rx.borrow().clone();
                        if new_ids != current_ids {
                            current_ids = new_ids;
                            // Reconnect to clear stale subscriptions (P0-1 equivalent)
                            break 'inner;
                        }
                    }

                    // Incoming message
                    msg = read.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                parse_and_dispatch(text.as_bytes(), &tx);
                            }
                            Some(Ok(Message::Binary(bytes))) => {
                                parse_and_dispatch(&bytes, &tx);
                            }
                            Some(Ok(Message::Ping(data))) => {
                                let _ = write.send(Message::Pong(data)).await;
                            }
                            Some(Err(e)) => {
                                error!("[WS {}] read error: {}", conn_id, e);
                                break 'inner;
                            }
                            None => break 'inner,
                            _ => {}
                        }
                    }
                }
            }

            info!("[WS {}] Reconnecting in 1s...", conn_id);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
}

// ── Hot path: parse and dispatch ─────────────────────────────────────────────

/// Parse raw WS bytes in-place, dispatch PriceUpdates by move.
/// No allocation of the original byte buffer; field Strings are moved into PriceUpdate.
#[inline]
fn parse_and_dispatch(raw: &[u8], tx: &mpsc::UnboundedSender<PriceUpdate>) {
    if raw.first() == Some(&b'[') {
        if let Ok(events) = serde_json::from_slice::<Vec<WsEnvelope>>(raw) {
            for event in events {
                dispatch_event(event, tx);
            }
        }
    } else if let Ok(event) = serde_json::from_slice::<WsEnvelope>(raw) {
        dispatch_event(event, tx);
    }
}

#[inline]
fn dispatch_event(event: WsEnvelope, tx: &mpsc::UnboundedSender<PriceUpdate>) {
    if event.event_type.as_deref() != Some("price_change") {
        return;
    }
    let Some(changes) = event.price_changes else { return };

    for change in changes {
        // ⚠️ FIX #1: Only SELL-side (ASK) events trigger analysis
        if change.side != "SELL" {
            continue;
        }

        let best_ask = change.price.as_deref()
            .and_then(|p| p.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);
        let size = change.size.as_deref()
            .and_then(|s| s.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);

        // Move asset_id String into PriceUpdate — no clone
        let _ = tx.send(PriceUpdate { asset_id: change.asset_id, best_ask, size });
    }
}
