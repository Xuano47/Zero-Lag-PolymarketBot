//! SQLite persistence layer using sqlx (runtime queries, no DATABASE_URL required).
//!
//! Schema is identical to Go's rarb.db so data can be read by the
//! existing Go dashboard if needed.

use anyhow::Result;
use sqlx::{sqlite::SqlitePool, Pool, Sqlite};

pub struct DbManager {
    pool: Pool<Sqlite>,
}

impl DbManager {
    pub async fn new(db_path: &str) -> Result<Self> {
        // Create parent directory if needed
        if let Some(parent) = std::path::Path::new(db_path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let url = format!("sqlite://{}?mode=rwc", db_path);
        let pool = SqlitePool::connect(&url).await?;

        let mgr = DbManager { pool };
        mgr.migrate().await?;
        Ok(mgr)
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS alerts (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp  TEXT NOT NULL,
                question   TEXT NOT NULL,
                profit_pct REAL NOT NULL,
                combined   REAL NOT NULL
            );
            CREATE TABLE IF NOT EXISTS trades (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp  TEXT NOT NULL,
                question   TEXT NOT NULL,
                side       TEXT NOT NULL,
                price      REAL NOT NULL,
                size       REAL NOT NULL,
                cost       REAL NOT NULL,
                order_id   TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS leg_risks (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp  TEXT NOT NULL,
                question   TEXT NOT NULL,
                reason     TEXT NOT NULL,
                order_id   TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS near_misses (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp  TEXT NOT NULL,
                question   TEXT NOT NULL,
                reason     TEXT NOT NULL,
                profit_pct REAL NOT NULL
            );
            CREATE TABLE IF NOT EXISTS bot_stats (
                key        TEXT PRIMARY KEY,
                value      TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_alert(
        &self, ts: &str, question: &str, profit_pct: f64, combined: f64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO alerts (timestamp, question, profit_pct, combined) VALUES (?, ?, ?, ?)"
        )
        .bind(ts).bind(question).bind(profit_pct).bind(combined)
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn insert_trade(
        &self, ts: &str, question: &str, side: &str,
        price: f64, size: f64, cost: f64, order_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO trades (timestamp, question, side, price, size, cost, order_id) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ts).bind(question).bind(side).bind(price).bind(size).bind(cost).bind(order_id)
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn insert_leg_risk(
        &self, ts: &str, question: &str, reason: &str, order_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO leg_risks (timestamp, question, reason, order_id) VALUES (?, ?, ?, ?)"
        )
        .bind(ts).bind(question).bind(reason).bind(order_id)
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn insert_near_miss(
        &self, ts: &str, question: &str, reason: &str, profit_pct: f64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO near_misses (timestamp, question, reason, profit_pct) VALUES (?, ?, ?, ?)"
        )
        .bind(ts).bind(question).bind(reason).bind(profit_pct)
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn upsert_stat(&self, key: &str, value: &str) -> Result<()> {
        let ts = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO bot_stats (key, value, updated_at) 
            VALUES (?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at
            "#
        )
        .bind(key).bind(value).bind(ts)
        .execute(&self.pool).await?;
        Ok(())
    }
}
