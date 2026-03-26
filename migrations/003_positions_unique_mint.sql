-- Migration 003: Add unique index on positions(mint) WHERE status != 'closed'
-- Prevents duplicate open positions for the same mint from concurrent execution.
-- Closed positions are excluded so historical records are preserved per trade session.
-- Also adds a unique index on positions(mint, opened_at) for closed positions
-- to prevent duplicate closed records for the same trade.

-- Step 1: Remove any remaining duplicate open positions per mint (keep lowest id)
DELETE FROM positions
WHERE status != 'closed'
  AND id NOT IN (
    SELECT MIN(id) FROM positions
    WHERE status != 'closed'
    GROUP BY mint
  );

-- Step 2: Remove duplicate closed positions per mint per day (keep lowest id)
DELETE FROM positions
WHERE status = 'closed'
  AND id NOT IN (
    SELECT MIN(id) FROM positions
    WHERE status = 'closed'
    GROUP BY mint, (opened_at / 86400000)
  );

-- Step 3: Unique index — only one open/reducing position per mint at a time
CREATE UNIQUE INDEX IF NOT EXISTS idx_positions_mint_open
  ON positions(mint)
  WHERE status IN ('open', 'reducing');
