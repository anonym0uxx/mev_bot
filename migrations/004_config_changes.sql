-- Config change audit log
-- Records every config reload with old/new config JSON and session PnL at time of change.
-- Invaluable for post-mortems: correlate config changes with performance shifts.
CREATE TABLE IF NOT EXISTS config_changes (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  changed_at   INTEGER NOT NULL,
  old_config   TEXT    NOT NULL,
  new_config   TEXT    NOT NULL,
  session_pnl  REAL               -- SOL PnL at time of change (null if unavailable)
);

CREATE INDEX IF NOT EXISTS idx_config_changes_changed_at ON config_changes(changed_at);
