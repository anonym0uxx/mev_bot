//! §56 **Chaos / fault-injection tests** — adversarial and randomised inputs
//! against the LaserStream parser, trade journal, and memory bank to prove
//! the system fails closed under malformed, truncated, and random data.
//!
//! Constitution refs:
//! * §56 — chaos testing (adversarial inputs must not panic or corrupt).
//! * §18.2 — fail closed on unknown, never guess benign.
//! * §22 — integer-only, deterministic, no float / clock / RNG / I/O.

use crate::laserstream::{parse_ndjson_line, LaserStreamUpdate, classify_pump_instructions};
use crate::trade_journal::{
    TradeRecord, TradeOutcome, TradeSide, RunMode,
};
use crate::memory_bank::{MemoryBank, MemoryBankConfig};
use crate::ProvenanceSource;

// ---------------------------------------------------------------------------
// LaserStream parser chaos tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod chaos_parser {
    use super::*;

    /// Empty string must not panic and must return None.
    #[test]
    fn chaos_empty_string() {
        assert!(parse_ndjson_line("").is_none());
    }

    /// Random bytes must not panic.
    #[test]
    fn chaos_random_bytes() {
        let mut bytes = [0u8; 256];
        for i in 0u8..=255 {
            bytes[i as usize] = i.wrapping_mul(7).wrapping_add(13);
        }
        let s = String::from_utf8_lossy(&bytes);
        let _ = parse_ndjson_line(&s); // must not panic
    }

    /// Truncated JSON at every byte position must not panic.
    #[test]
    fn chaos_truncated_json() {
        let pump_b58 = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let full = format!(
            "{{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":42,\"recv_unix_ms\":0,\"signature_b58\":\"\",\"account_keys\":[\"{}\"],\"instructions\":[]}}",
            pump_b58
        );
        for cut in 1..full.len() {
            let truncated = &full[..cut];
            let _ = parse_ndjson_line(truncated); // must not panic
        }
    }

    /// Null bytes embedded in JSON must not panic.
    #[test]
    fn chaos_null_bytes_in_json() {
        let mut bad = b"{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":42}".to_vec();
        bad.insert(10, 0);
        let s = String::from_utf8_lossy(&bad);
        let _ = parse_ndjson_line(&s); // must not panic
    }

    /// Extremely large slot numbers must not panic.
    #[test]
    fn chaos_huge_slot() {
        let line = "{\"lane\":\"laserstream\",\"kind\":\"slot\",\"slot\":18446744073709551615}";
        let result = parse_ndjson_line(line);
        match result {
            Some(LaserStreamUpdate::Slot { slot }) => assert_eq!(slot, u64::MAX),
            _ => panic!("expected Slot with u64::MAX"),
        }
    }

    /// Slot overflow beyond u64 must not panic.
    #[test]
    fn chaos_slot_overflow() {
        let line = "{\"lane\":\"laserstream\",\"kind\":\"slot\",\"slot\":18446744073709551616}";
        let _ = parse_ndjson_line(line); // must not panic, returns None
    }

    /// Wrong kind field must not panic.
    #[test]
    fn chaos_wrong_kind() {
        let kinds = ["", "unknown", "T", "transaction ", " account", "SLOT", "null", "123", "true"];
        for k in &kinds {
            let line = format!("{{\"lane\":\"laserstream\",\"kind\":\"{}\",\"slot\":1}}", k);
            let _ = parse_ndjson_line(&line); // must not panic
        }
    }

    /// Missing required fields must not panic.
    #[test]
    fn chaos_missing_fields() {
        let lines = [
            "{}",
            "{\"lane\":\"laserstream\"}",
            "{\"lane\":\"laserstream\",\"kind\":\"transaction\"}",
            "{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":1}",
            "{\"lane\":\"laserstream\",\"kind\":\"account\",\"slot\":1}",
            "{\"lane\":\"laserstream\",\"kind\":\"account\",\"slot\":1,\"pubkey_b58\":\"abc\"}",
        ];
        for line in &lines {
            let _ = parse_ndjson_line(line); // must not panic
        }
    }

    /// Invalid base58 in account_keys must not panic.
    #[test]
    fn chaos_invalid_base58() {
        let bad_keys = ["", "0", "O", "l", "/", "@@@@", "!!!!", "abc def", "11111 1"];
        for key in &bad_keys {
            let line = format!(
                "{{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":1,\"account_keys\":[\"{}\"],\"instructions\":[]}}",
                key
            );
            let _ = parse_ndjson_line(&line); // must not panic
        }
    }

    /// Invalid base64 in instruction data must not panic.
    #[test]
    fn chaos_invalid_base64() {
        let pump_b58 = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let bad_b64 = ["", "!", "@@@", "....", "abc def", "AAAA!!", "===="];
        for b in &bad_b64 {
            let line = format!(
                "{{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":1,\"account_keys\":[\"{}\"],\"instructions\":[{{\"program_b58\":\"{}\",\"data_b64\":\"{}\",\"accounts\":[0]}}]}}",
                pump_b58, pump_b58, b
            );
            let _ = parse_ndjson_line(&line); // must not panic
        }
    }

    /// Deeply nested JSON must not panic (stack depth).
    #[test]
    fn chaos_deeply_nested() {
        let mut deep = String::new();
        for _ in 0..100 {
            deep.push_str("{\"a\":");
        }
        deep.push_str("1");
        for _ in 0..100 {
            deep.push_str("}");
        }
        let _ = parse_ndjson_line(&deep); // must not panic
    }

    /// Instruction with account index out of bounds must not panic.
    #[test]
    fn chaos_account_index_out_of_bounds() {
        let pump_b58 = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let mint_b58 = "11111111111111111111111111111111";
        let mut ix_data = pump_quant_protocol::ix::BUY_DISCRIMINATOR.to_vec();
        ix_data.extend_from_slice(&100u64.to_le_bytes());
        ix_data.extend_from_slice(&10u64.to_le_bytes());
        let ix_b64 = base64::encode(&ix_data);
        let line = format!(
            "{{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":1,\"account_keys\":[\"{}\",\"{}\"],\"instructions\":[{{\"program_b58\":\"{}\",\"data_b64\":\"{}\",\"accounts\":[255]}}]}}",
            pump_b58, mint_b58, pump_b58, ix_b64
        );
        let result = parse_ndjson_line(&line);
        if let Some(LaserStreamUpdate::Transaction(tx)) = result {
            let classified = classify_pump_instructions(&tx);
            assert!(classified.is_empty());
        }
    }

    /// Instruction data shorter than discriminator must not panic.
    #[test]
    fn chaos_short_instruction_data() {
        let pump_b58 = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let short_data_b64 = base64::encode(&[0x66, 0x06, 0x3d]);
        let line = format!(
            "{{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":1,\"account_keys\":[\"{}\"],\"instructions\":[{{\"program_b58\":\"{}\",\"data_b64\":\"{}\",\"accounts\":[0]}}]}}",
            pump_b58, pump_b58, short_data_b64
        );
        let result = parse_ndjson_line(&line);
        if let Some(LaserStreamUpdate::Transaction(tx)) = result {
            let classified = classify_pump_instructions(&tx);
            assert!(classified.is_empty());
        }
    }

    /// Fuzzy: 100 random byte sequences as instruction data — none must panic.
    #[test]
    fn chaos_fuzzy_instruction_data() {
        let pump_b58 = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let mint_b58 = "11111111111111111111111111111111";
        for i in 0u32..100 {
            let mut data = vec![
                ((i * 7) % 256) as u8,
                ((i * 13) % 256) as u8,
                ((i * 29) % 256) as u8,
                ((i * 97) % 256) as u8,
                ((i * 211) % 256) as u8,
                ((i * 31) % 256) as u8,
                ((i * 67) % 256) as u8,
                ((i * 103) % 256) as u8,
            ];
            for j in 0..16 {
                data.push(((i * 127 + j) % 256) as u8);
            }
            let ix_b64 = base64::encode(&data);
            let line = format!(
                "{{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":{},\"account_keys\":[\"{}\",\"{}\"],\"instructions\":[{{\"program_b58\":\"{}\",\"data_b64\":\"{}\",\"accounts\":[0,1]}}]}}",
                i, pump_b58, mint_b58, pump_b58, ix_b64
            );
            let result = parse_ndjson_line(&line);
            if let Some(LaserStreamUpdate::Transaction(tx)) = result {
                let _ = classify_pump_instructions(&tx);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Trade journal chaos tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod chaos_journal {
    use super::*;

    fn make_record(seed: u64) -> TradeRecord {
        TradeRecord {
            slot: seed % 1_000_000,
            mint_b58: format!("mint_{}", seed % 100),
            side: if seed % 2 == 0 { TradeSide::Buy } else { TradeSide::Sell },
            entry_price_fp: (seed as i128) * 1_000_000,
            exit_price_fp: if seed % 3 == 0 { 0 } else { (seed as i128) * 1_000_001 },
            size_lamports: seed % 1_000_000_000,
            strategy_id: seed % 10,
            source: ProvenanceSource::LaserStream,
            outcome: if seed % 3 == 0 {
                TradeOutcome::OnChainFailure
            } else if seed % 3 == 1 {
                TradeOutcome::Pending
            } else {
                TradeOutcome::Filled
            },
            realized_pnl_lamports: if seed % 2 == 0 { (seed % 100_000) as i64 } else { -((seed % 50_000) as i64) },
            fees_lamports: seed % 10_000,
            slippage_lamports: seed % 5_000,
            decision_latency_us: seed % 10_000,
            confirm_latency_us: seed % 50_000,
            run_mode: if seed % 2 == 0 { RunMode::Paper } else { RunMode::Live },
            error_code: if seed % 3 == 0 { 6001 } else { 0 },
            seq: seed,
            lane: None,
        }
    }

    /// 1000 random trades — each must serialise without panic.
    #[test]
    fn chaos_journal_1000_random_trades() {
        for i in 0u64..1000 {
            let rec = make_record(i);
            let jsonl = rec.to_jsonl();
            assert!(!jsonl.is_empty());
            // Verify it starts with { and ends with }.
            assert!(jsonl.starts_with('{'));
            assert!(jsonl.ends_with('}'));
        }
    }

    /// A trade record with all-zero fields must not panic.
    #[test]
    fn chaos_zero_trade_record() {
        let rec = TradeRecord {
            slot: 0,
            mint_b58: String::new(),
            side: TradeSide::Buy,
            entry_price_fp: 0,
            exit_price_fp: 0,
            size_lamports: 0,
            strategy_id: 0,
            source: ProvenanceSource::LaserStream,
            outcome: TradeOutcome::Filled,
            realized_pnl_lamports: 0,
            fees_lamports: 0,
            slippage_lamports: 0,
            decision_latency_us: 0,
            confirm_latency_us: 0,
            run_mode: RunMode::Paper,
            error_code: 0,
            seq: 0,
            lane: None,
        };
        let jsonl = rec.to_jsonl();
        assert!(!jsonl.is_empty());
    }

    /// A trade record with u64::MAX / i64::MAX fields must not panic.
    #[test]
    fn chaos_max_trade_record() {
        let rec = TradeRecord {
            slot: u64::MAX,
            mint_b58: "x".repeat(44),
            side: TradeSide::Sell,
            entry_price_fp: i128::MAX,
            exit_price_fp: i128::MIN,
            size_lamports: u64::MAX,
            strategy_id: u64::MAX,
            source: ProvenanceSource::LaserStream,
            outcome: TradeOutcome::FilledWithSlippage,
            realized_pnl_lamports: i64::MAX,
            fees_lamports: u64::MAX,
            slippage_lamports: u64::MAX,
            decision_latency_us: u64::MAX,
            confirm_latency_us: u64::MAX,
            run_mode: RunMode::Live,
            error_code: u32::MAX,
            seq: u64::MAX,
            lane: None,
        };
        let jsonl = rec.to_jsonl();
        assert!(!jsonl.is_empty());
    }

    /// Deterministic: the same seed must always produce the same JSONL.
    #[test]
    fn chaos_journal_deterministic_serialization() {
        let rec1 = make_record(42);
        let rec2 = make_record(42);
        assert_eq!(rec1.to_jsonl(), rec2.to_jsonl());
    }
}

// ---------------------------------------------------------------------------
// Memory bank chaos tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod chaos_memory {
    use super::*;
    use crate::trade_journal::{TradeRecord, TradeOutcome, TradeSide, RunMode};
    use crate::ProvenanceSource;

    fn make_record(seed: u64) -> TradeRecord {
        TradeRecord {
            slot: seed % 1_000_000,
            mint_b58: format!("mint_{}", seed % 100),
            side: if seed % 2 == 0 { TradeSide::Buy } else { TradeSide::Sell },
            entry_price_fp: (seed as i128) * 1_000_000,
            exit_price_fp: if seed % 3 == 0 { 0 } else { (seed as i128) * 1_000_001 },
            size_lamports: seed % 1_000_000_000,
            strategy_id: seed % 10,
            source: ProvenanceSource::LaserStream,
            outcome: if seed % 3 == 0 {
                TradeOutcome::OnChainFailure
            } else if seed % 3 == 1 {
                TradeOutcome::Pending // non-terminal — must be ignored by ingest
            } else {
                TradeOutcome::Filled
            },
            realized_pnl_lamports: if seed % 2 == 0 { (seed % 100_000) as i64 } else { -((seed % 50_000) as i64) },
            fees_lamports: seed % 10_000,
            slippage_lamports: seed % 5_000,
            decision_latency_us: seed % 10_000,
            confirm_latency_us: seed % 50_000,
            run_mode: if seed % 2 == 0 { RunMode::Paper } else { RunMode::Live },
            error_code: if seed % 3 == 0 { 6001 } else { 0 },
            seq: seed,
            lane: None,
        }
    }

    /// 100 random trades — memory bank must remain consistent.
    #[test]
    fn chaos_memory_bank_100_trades() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());
        for i in 0u64..100 {
            bank.ingest(&make_record(i));
        }
        // Global summary must have trades (non-pending outcomes only).
        let global = bank.global_summary();
        // 100 trades: 33 OnChainFailure, 33 Pending (ignored), 34 Filled
        // → 67 terminal trades should be counted.
        assert!(global.total_trades > 0);
        assert!(global.total_trades <= 100);
        // Wins + losses must equal total trades.
        assert_eq!(global.total_wins + global.total_losses, global.total_trades);
    }

    /// Memory bank with a single trade must not panic.
    #[test]
    fn chaos_memory_bank_single_trade() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());
        bank.ingest(&make_record(0));
        let global = bank.global_summary();
        // Outcome for seed=0 is OnChainFailure (terminal) → counted.
        assert_eq!(global.total_trades, 1);
    }

    /// Memory bank with zero trades must not panic.
    #[test]
    fn chaos_memory_bank_empty() {
        let bank = MemoryBank::new(MemoryBankConfig::default());
        let global = bank.global_summary();
        assert_eq!(global.total_trades, 0);
    }

    /// Memory bank with all-pending trades must not count them.
    #[test]
    fn chaos_memory_bank_all_pending() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());
        for i in 0u64..50 {
            // Force all outcomes to Pending (non-terminal).
            let mut rec = make_record(i);
            rec.outcome = TradeOutcome::Pending;
            bank.ingest(&rec);
        }
        let global = bank.global_summary();
        assert_eq!(global.total_trades, 0);
    }

    /// Memory bank with all-fail trades must not panic.
    #[test]
    fn chaos_memory_bank_all_failures() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());
        for i in 0u64..50 {
            let mut rec = make_record(i);
            rec.outcome = TradeOutcome::OnChainFailure;
            rec.realized_pnl_lamports = 0;
            bank.ingest(&rec);
        }
        let global = bank.global_summary();
        assert_eq!(global.total_trades, 50);
        assert_eq!(global.total_wins, 0);
    }

    /// Deterministic: the same sequence must always produce the same summary.
    #[test]
    fn chaos_memory_bank_deterministic() {
        let mut bank1 = MemoryBank::new(MemoryBankConfig::default());
        let mut bank2 = MemoryBank::new(MemoryBankConfig::default());
        for i in 0u64..100 {
            bank1.ingest(&make_record(i));
            bank2.ingest(&make_record(i));
        }
        assert_eq!(bank1.global_summary(), bank2.global_summary());
    }
}
