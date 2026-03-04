import os
import sqlite3
from flask import Flask, jsonify, render_template
from datetime import datetime

app = Flask(__name__)

# Assumes this script runs in `polymarket-rust/dashboard` and DB is in `polymarket-rust/data/rarb.db`
DB_PATH = os.path.join(os.path.dirname(__file__), "..", "data", "rarb.db")

def query_db(query, args=(), one=False):
    if not os.path.exists(DB_PATH):
        return None if one else []
    con = sqlite3.connect(DB_PATH)
    con.row_factory = sqlite3.Row
    cur = con.cursor()
    cur.execute(query, args)
    rv = cur.fetchall()
    con.close()
    return (rv[0] if rv else None) if one else rv

@app.route("/")
def index():
    return render_template("index.html")

@app.route("/api/stats")
def stats():
    # Basic DB stats
    count, vol = 0, 0.0
    row = query_db("SELECT COUNT(*) as c, SUM(cost) as v FROM trades", one=True)
    if row and row['c']:
        count = row['c']
        vol = row['v'] or 0.0

    risk_count = 0
    row_risk = query_db("SELECT COUNT(*) as c FROM leg_risks", one=True)
    if row_risk: risk_count = row_risk['c']

    # Bot memory stats synced via bot_stats table
    def get_stat(key, default):
        try:
            r = query_db("SELECT value FROM bot_stats WHERE key = ?", (key,), one=True)
            return r['value'] if r else default
        except:
            return default

    markets = int(get_stat("markets_scanned", "0"))
    updates = int(get_stat("price_updates", "0"))
    alerts = int(get_stat("arbitrage_alerts", "0"))
    balance = float(get_stat("balance", "0.0"))

    return jsonify({
        "total_volume": vol,
        "trade_count": count,
        "markets_scanned": markets,
        "price_updates": updates,
        "arbitrage_alerts": alerts,
        "balance": balance,
        "last_sync": datetime.utcnow().strftime("%H:%M:%S"),
        "risk_count": risk_count,
    })

@app.route("/api/history")
def history():
    def to_dict(rows):
        return [dict(ix) for ix in rows] if rows else []

    trades_raw = query_db("SELECT timestamp, question as market_name, side as outcome, price, size, cost FROM trades ORDER BY id DESC LIMIT 15")
    alerts_raw = query_db("SELECT timestamp, question as market, profit_pct as profit, combined FROM alerts ORDER BY id DESC LIMIT 15")
    misses_raw = query_db("SELECT timestamp, question as market, reason, profit_pct as profit FROM near_misses ORDER BY id DESC LIMIT 15")
    risks_raw = query_db("SELECT timestamp, question as market, reason as outcome, order_id FROM leg_risks ORDER BY id DESC LIMIT 15")

    # Format exactly like Go returned
    trades = [{"t": r["timestamp"], "m": r["market_name"], "o": r["outcome"], "p": r["price"], "s": r["size"], "c": r["cost"]} for r in to_dict(trades_raw)]
    alerts = [{"t": r["timestamp"], "m": r["market"], "p": r["profit"], "c": r["combined"]} for r in to_dict(alerts_raw)]
    misses = [{"t": r["timestamp"], "m": r["market"], "r": r["reason"], "p": r["profit"]} for r in to_dict(misses_raw)]
    risks = [{"t": r["timestamp"], "m": r["market"], "o": r["outcome"], "id": r["order_id"]} for r in to_dict(risks_raw)]

    return jsonify({
        "trades": trades,
        "alerts": alerts,
        "misses": misses,
        "risks": risks
    })

@app.route("/api/portfolio")
def portfolio():
    # Read-only standalone missing memory state. Return empty.
    return jsonify([])

if __name__ == "__main__":
    app.run(host="127.0.0.1", port=8081)
