use anyhow::{Context, Result};
use rusqlite::{params, Connection};

// ── Trade log entry ──────────────────────────────────────────────────────────

pub struct TradeLogEntry {
    pub mint: String,
    pub entry_vsol: f64,
    pub exit_vsol: f64,
    pub entry_ts_ms: i64,
    pub exit_ts_ms: i64,
    pub hold_ms: i64,
    pub size_sol: f64,
    pub gross_pnl_sol: f64,
    pub net_pnl_sol: f64,
    pub fees_sol: f64,
    pub exit_reason: String,
    pub score: f64,
    pub is_paper: bool,
    pub engine_version: String,
}

// ── SQLite logger ────────────────────────────────────────────────────────────

pub struct SqliteLogger {
    conn: Connection,
}

impl SqliteLogger {
    /// Open (or create) the SQLite database at `db_path` and initialize schema.
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("failed to open SQLite at {db_path}"))?;

        // WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .context("failed to set WAL mode")?;

        let logger = Self { conn };
        logger.init_schema()?;
        Ok(logger)
    }

    /// Create the mev_trades table and index if they don't exist.
    fn init_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS mev_trades (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    mint TEXT NOT NULL,
                    entry_vsol REAL,
                    exit_vsol REAL,
                    entry_ts_ms INTEGER,
                    exit_ts_ms INTEGER,
                    hold_ms INTEGER,
                    size_sol REAL,
                    gross_pnl_sol REAL,
                    net_pnl_sol REAL,
                    fees_sol REAL,
                    exit_reason TEXT,
                    score REAL,
                    is_paper INTEGER,
                    engine_version TEXT,
                    created_at INTEGER DEFAULT (strftime('%s','now') * 1000)
                );
                CREATE INDEX IF NOT EXISTS idx_mev_trades_ts ON mev_trades(entry_ts_ms);",
            )
            .context("failed to initialize mev_trades schema")?;
        Ok(())
    }

    /// Insert a single trade log entry.
    pub fn log_trade(&self, e: &TradeLogEntry) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO mev_trades (
                    mint, entry_vsol, exit_vsol, entry_ts_ms, exit_ts_ms,
                    hold_ms, size_sol, gross_pnl_sol, net_pnl_sol, fees_sol,
                    exit_reason, score, is_paper, engine_version
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    e.mint,
                    e.entry_vsol,
                    e.exit_vsol,
                    e.entry_ts_ms,
                    e.exit_ts_ms,
                    e.hold_ms,
                    e.size_sol,
                    e.gross_pnl_sol,
                    e.net_pnl_sol,
                    e.fees_sol,
                    e.exit_reason,
                    e.score,
                    e.is_paper as i32,
                    e.engine_version,
                ],
            )
            .context("failed to insert trade log entry")?;
        Ok(())
    }

    /// Batch insert multiple trade log entries, wrapped in a single transaction
    /// for high-throughput scenarios.
    pub fn log_trades_batch(&self, entries: &[TradeLogEntry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let tx = self
            .conn
            .unchecked_transaction()
            .context("failed to begin batch transaction")?;

        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO mev_trades (
                        mint, entry_vsol, exit_vsol, entry_ts_ms, exit_ts_ms,
                        hold_ms, size_sol, gross_pnl_sol, net_pnl_sol, fees_sol,
                        exit_reason, score, is_paper, engine_version
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                )
                .context("failed to prepare batch insert statement")?;

            for e in entries {
                stmt.execute(params![
                    e.mint,
                    e.entry_vsol,
                    e.exit_vsol,
                    e.entry_ts_ms,
                    e.exit_ts_ms,
                    e.hold_ms,
                    e.size_sol,
                    e.gross_pnl_sol,
                    e.net_pnl_sol,
                    e.fees_sol,
                    e.exit_reason,
                    e.score,
                    e.is_paper as i32,
                    e.engine_version,
                ])
                .context("failed to insert entry in batch")?;
            }
        }

        tx.commit().context("failed to commit batch transaction")?;
        Ok(())
    }
}
