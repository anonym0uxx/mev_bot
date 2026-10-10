//! DURABLE mint -> bonding-curve BINDING records (R5 D3).
//!
//! The daemon binds a pump.fun mint to its bonding-curve account (process-local `pda_to_mint`) the moment a decoded
//! pump.fun buy / sell / create instruction names the mint: the curve address is `PDA("bonding-curve", mint)` under
//! the pump.fun program, so that derivation IS the ownership check. Under `CurveTradeSource::Events` the instruction's
//! own print is filtered before capture, so nothing in `event_stream.jsonl` recorded that binding and a restarted
//! process lost the routing of every market it had seen only through trade prints.
//!
//! This module writes ONE versioned JSON line per binding, at the moment the in-memory binding is installed, carrying
//! the causal evidence (instruction kind, transaction signature, slot, receive time, curve account). The log is an
//! append-only side file next to the capture and is SEALED together with it (capture seal schema 2,
//! `capture_rehydrate`). On restart only the sealed prefix is read, every record is re-validated (record version,
//! curve == PDA(mint)), and only bindings received at or before the restart instant are restored. Nothing is derived
//! from later data. Record format (`v` = [`BINDING_RECORD_VERSION`]):
//! `{"v":1,"mint":"<b58>","curve":"<b58>","source":"ix_buy|ix_sell|ix_launch","sig":"<b58>","slot":N,"recv_ms":N|null}`

use std::io::Write;
use std::path::Path;

/// Default binding log location (next to the capture).
pub const DEFAULT_CURVE_BINDING_FILE: &str = "data/curve_bindings.jsonl";
/// Version of one binding record line.
pub const BINDING_RECORD_VERSION: u64 = 1;

/// pump.fun bonding-curve program id (same bytes as `laserstream::PUMP_FUN_PROGRAM`).
const PUMP_PROGRAM: [u8; 32] = crate::laserstream::PUMP_FUN_PROGRAM;

/// The instruction that validated the binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingSource {
    IxBuy,
    IxSell,
    IxLaunch,
}

impl BindingSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IxBuy => "ix_buy",
            Self::IxSell => "ix_sell",
            Self::IxLaunch => "ix_launch",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ix_buy" => Some(Self::IxBuy),
            "ix_sell" => Some(Self::IxSell),
            "ix_launch" => Some(Self::IxLaunch),
            _ => None,
        }
    }
}

/// One validated binding and its evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurveBinding {
    pub mint: [u8; 32],
    pub curve: [u8; 32],
    pub source: BindingSource,
    pub signature: [u8; 64],
    pub slot: u64,
    pub recv_ms: Option<i64>,
}

/// `PDA("bonding-curve", mint)` under the pump.fun program.
#[must_use]
pub fn curve_pda(mint: &[u8; 32]) -> [u8; 32] {
    let program = solana_program::pubkey::Pubkey::new_from_array(PUMP_PROGRAM);
    let seeds: [&[u8]; 2] = [b"bonding-curve", mint];
    solana_program::pubkey::Pubkey::find_program_address(&seeds, &program)
        .0
        .to_bytes()
}

impl CurveBinding {
    /// The binding for `mint` validated by an instruction: the curve is DERIVED, never taken from the input.
    #[must_use]
    pub fn from_instruction(
        mint: [u8; 32],
        source: BindingSource,
        signature: [u8; 64],
        slot: u64,
        recv_ms: Option<i64>,
    ) -> Self {
        Self {
            curve: curve_pda(&mint),
            mint,
            source,
            signature,
            slot,
            recv_ms,
        }
    }
}

/// One record line (no trailing newline).
#[must_use]
pub fn encode(b: &CurveBinding) -> String {
    serde_json::json!({
        "v": BINDING_RECORD_VERSION,
        "mint": bs58::encode(b.mint).into_string(),
        "curve": bs58::encode(b.curve).into_string(),
        "source": b.source.as_str(),
        "sig": bs58::encode(b.signature).into_string(),
        "slot": b.slot,
        "recv_ms": b.recv_ms,
    })
    .to_string()
}

fn key32(v: &serde_json::Value, k: &str) -> Result<[u8; 32], String> {
    let s = v[k]
        .as_str()
        .ok_or(format!("curve_binding_field_missing:{k}"))?;
    bs58::decode(s)
        .into_vec()
        .ok()
        .and_then(|x| <[u8; 32]>::try_from(x).ok())
        .ok_or(format!("curve_binding_field_malformed:{k}"))
}

/// Decode and RE-VALIDATE one record: known version, known source, curve == PDA(mint). Errors are named.
pub fn decode(line: &str) -> Result<CurveBinding, String> {
    let v: serde_json::Value =
        serde_json::from_str(line).map_err(|_| "curve_binding_malformed".to_string())?;
    match v["v"].as_u64() {
        Some(BINDING_RECORD_VERSION) => {}
        Some(n) => return Err(format!("curve_binding_version_unsupported:{n}")),
        None => return Err("curve_binding_version_missing".into()),
    }
    let mint = key32(&v, "mint")?;
    let curve = key32(&v, "curve")?;
    let source = v["source"]
        .as_str()
        .and_then(BindingSource::parse)
        .ok_or("curve_binding_source_unknown")?;
    let signature = v["sig"]
        .as_str()
        .and_then(|s| bs58::decode(s).into_vec().ok())
        .and_then(|x| <[u8; 64]>::try_from(x).ok())
        .ok_or("curve_binding_field_malformed:sig")?;
    let slot = v["slot"]
        .as_u64()
        .ok_or("curve_binding_field_missing:slot")?;
    let recv_ms = match &v["recv_ms"] {
        serde_json::Value::Null => None,
        x => Some(x.as_i64().ok_or("curve_binding_field_malformed:recv_ms")?),
    };
    if curve != curve_pda(&mint) {
        return Err("curve_binding_pda_mismatch".into());
    }
    Ok(CurveBinding {
        mint,
        curve,
        source,
        signature,
        slot,
        recv_ms,
    })
}

/// Append-only binding log writer (each record is flushed on write; the capture sealer hashes the file at each seal).
pub struct BindingWriter {
    w: std::io::BufWriter<std::fs::File>,
    pub written: u64,
}

impl BindingWriter {
    /// Open (or create) in append mode. `None` if the file cannot be opened (the caller names that).
    #[must_use]
    pub fn open(path: &Path) -> Option<Self> {
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(Self {
            w: std::io::BufWriter::with_capacity(16 * 1024, f),
            written: 0,
        })
    }

    /// Append one record and flush it to the file at once: a binding is rare (one per newly routed market) and must
    /// be in the file before any later seal can name the barrier it precedes.
    pub fn record(&mut self, b: &CurveBinding) -> std::io::Result<()> {
        self.w.write_all(encode(b).as_bytes())?;
        self.w.write_all(b"\n")?;
        self.w.flush()?;
        self.written = self.written.saturating_add(1);
        Ok(())
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

/// What a restart recovered from the binding log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingRestore {
    /// The seal predates binding records (capture seal schema 1, an older file): nothing recorded, nothing restored.
    /// Routing of print-only markets stays lost until a post-restart instruction (the pre-D3 behaviour), by name.
    NotRecordedSchema1,
    /// Sealed prefix verified; `bindings` = records received at or before the restart instant, in log order.
    Restored {
        bindings: Vec<CurveBinding>,
        after_restart_skipped: u64,
        undated_skipped: u64,
    },
    /// Named refusal: nothing restored from the log.
    Refused(String),
}

/// Records of a verified sealed prefix with receive time `<= restart_ms`. A record with no receive time cannot be
/// placed before the restart and is skipped (counted), never assumed.
pub fn bindings_through(
    text: &str,
    restart_ms: i64,
) -> Result<(Vec<CurveBinding>, u64, u64), String> {
    let mut out = Vec::new();
    let (mut after, mut undated) = (0u64, 0u64);
    for (i, l) in text.lines().enumerate() {
        let b = decode(l)
            .map_err(|e| format!("curve_binding_line_undecodable:{}:{e}", i.saturating_add(1)))?;
        match b.recv_ms {
            None => undated = undated.saturating_add(1),
            Some(t) if t > restart_ms => after = after.saturating_add(1),
            Some(_) => out.push(b),
        }
    }
    Ok((out, after, undated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_and_a_forged_curve_is_refused() {
        let b =
            CurveBinding::from_instruction([7; 32], BindingSource::IxSell, [9; 64], 42, Some(5));
        assert_eq!(decode(&encode(&b)), Ok(b.clone()));
        let forged = encode(&b).replace(
            &bs58::encode(b.curve).into_string(),
            &bs58::encode([3u8; 32]).into_string(),
        );
        assert_eq!(decode(&forged), Err("curve_binding_pda_mismatch".into()));
        let future = encode(&b).replace("\"v\":1", "\"v\":2");
        assert_eq!(
            decode(&future),
            Err("curve_binding_version_unsupported:2".into())
        );
    }
}
