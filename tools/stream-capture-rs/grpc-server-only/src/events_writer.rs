//! Events writer — writes the normalized `pump_event_v1` NDJSON file
//! (uncompressed, line-delimited JSON objects).
//!
//! This file is the causal, hindsight-free event stream for future labeling.
//! It is kept uncompressed for easy streaming/inspection during analysis.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

use crate::normalizer::PumpEventV1;

pub struct EventsWriter {
    inner: Mutex<BufWriter<File>>,
    count: std::sync::atomic::AtomicU64,
}

impl EventsWriter {
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            inner: Mutex::new(BufWriter::new(file)),
            count: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Write one event as a JSON line.
    pub fn write(&self, event: &PumpEventV1) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let json = serde_json::to_string(event)?;
        writeln!(inner, "{json}")?;
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Flush + sync (for graceful shutdown).
    pub fn flush(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.flush()?;
        inner.get_ref().sync_all()?;
        Ok(())
    }

    pub fn count(&self) -> u64 {
        self.count.load(std::sync::atomic::Ordering::Relaxed)
    }
}
