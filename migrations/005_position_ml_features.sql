-- Migration 005: Add ML training features to positions table
-- Fixes: MFE unit mismatch, missing entry signals, missing exit price
-- Data eng review 2026-03-26: 733 trades with no feature data = ML blind

-- Store all entry-time signals for supervised learning
ALTER TABLE positions ADD COLUMN entry_features TEXT; -- JSON blob of full signal vector at entry
ALTER TABLE positions ADD COLUMN feat_p_cont REAL;    -- p_continuation at entry decision
ALTER TABLE positions ADD COLUMN feat_bcd_score REAL; -- BCD composite score at entry
ALTER TABLE positions ADD COLUMN feat_manip_score REAL; -- manipulation distribution score
ALTER TABLE positions ADD COLUMN feat_creator_prior REAL; -- creator wallet prior
ALTER TABLE positions ADD COLUMN feat_velocity REAL;  -- buy_notional_velocity at entry
ALTER TABLE positions ADD COLUMN feat_breadth_score REAL; -- breadth topology score
ALTER TABLE positions ADD COLUMN feat_social_score REAL;  -- social cache score (0-1)
ALTER TABLE positions ADD COLUMN feat_curve_pct REAL; -- bonding curve progress % at entry
ALTER TABLE positions ADD COLUMN feat_mcap_sol REAL;  -- vSol in bonding curve at entry
ALTER TABLE positions ADD COLUMN feat_age_s INTEGER;  -- token age (s) at entry
ALTER TABLE positions ADD COLUMN feat_unique_buyers INTEGER; -- unique buyers at entry

-- Fix exit price (was always null)
-- Column already exists but is null — no schema change needed, just fix the code

-- Add entry/exit timestamps as proper integers (opened_at/closed_at exist but ensure ms precision)
ALTER TABLE positions ADD COLUMN entry_ts INTEGER;  -- unix epoch ms, redundant with opened_at but explicit
ALTER TABLE positions ADD COLUMN exit_ts INTEGER;   -- unix epoch ms, redundant with closed_at but explicit

-- Config snapshot: store the active stop/target at trade time so we can stratify by config epoch
ALTER TABLE positions ADD COLUMN active_stop_pct REAL;
ALTER TABLE positions ADD COLUMN active_target_pct REAL;
ALTER TABLE positions ADD COLUMN active_max_hold_s INTEGER;
