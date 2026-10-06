//! Events writer — writes the normalized `pump_event_v1` NDJSON stream,
//! compressed with zstd (matching the raw recorder).
//!
//! This file is the causal, hindsight-free event stream for future labeling.
//! It is zstd-compressed to keep the dominant storage cost (~290 GB/day
//! uncompressed) down to ~10x smaller; the daily compaction job converts it to
//! Parquet for query + archive.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use zstd::stream::Encoder;

use crate::normalizer::PumpEventV1;

pub struct EventsWriter {
    inner: Mutex<Option<BufWriter<Encoder<'static, File>>>>,
    count: AtomicU64,
}

impl EventsWriter {
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let encoder = Encoder::new(file, 3)?; // level 3: good ratio + fast (matches raw)
        Ok(Self {
            inner: Mutex::new(Some(BufWriter::new(encoder))),
            count: AtomicU64::new(0),
        })
    }

    /// Write one event as a JSON line (into the zstd stream).
    pub fn write(&self, event: &PumpEventV1) -> std::io::Result<()> {
        let mut guard = self.inner.lock().unwrap();
        let writer = guard
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "events writer finalized"))?;
        let json = serde_json::to_string(event)?;
        writeln!(writer, "{json}")?;
        self.count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Flush + finalize the zstd stream (writes the end frame). Idempotent.
    pub fn flush(&self) -> std::io::Result<()> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(writer) = guard.take() {
            let encoder = writer
                .into_inner()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("into_inner: {e}")))?;
            encoder.finish()?; // flush remaining data + write zstd end frame
        }
        Ok(())
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}
