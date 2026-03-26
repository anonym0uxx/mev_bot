-- Migration 002: Add unique index on orders.tx_signature to prevent duplicate confirmed rows.
-- This ensures each on-chain transaction is recorded exactly once regardless of how many
-- times the confirmation poller fires. NULL signatures are excluded (UNIQUE in SQLite
-- allows multiple NULLs, so pending/sent orders without a sig are unaffected).

-- Step 1: Remove existing duplicate rows, keeping only the one with the highest realized_sol
-- per tx_signature (the most complete/accurate record of the on-chain result).
DELETE FROM orders
WHERE tx_signature IS NOT NULL
  AND id NOT IN (
    SELECT id FROM orders o1
    WHERE tx_signature IS NOT NULL
      AND realized_sol = (
        SELECT MAX(o2.realized_sol)
        FROM orders o2
        WHERE o2.tx_signature = o1.tx_signature
      )
      AND id = (
        SELECT MIN(o3.id)
        FROM orders o3
        WHERE o3.tx_signature = o1.tx_signature
          AND o3.realized_sol = (
            SELECT MAX(o4.realized_sol)
            FROM orders o4
            WHERE o4.tx_signature = o1.tx_signature
          )
      )
  );

-- Step 2: Now that duplicates are gone, create the unique index.
CREATE UNIQUE INDEX IF NOT EXISTS idx_orders_tx_signature_unique
  ON orders(tx_signature)
  WHERE tx_signature IS NOT NULL;
