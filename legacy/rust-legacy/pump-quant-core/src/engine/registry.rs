//! Engine registry — holds all trading engines and dispatches events via fan-out.
//!
//! Single concrete struct (not a trait). Registry is owned by `main()` and
//! passed by reference to `FeedRouter::dispatch()`.

use std::sync::Arc;
use super::trading_engine::{GraduationEvent, TradingEngine};

/// Registry of all trading engines. Dispatches events to enabled engines.
pub struct EngineRegistry {
    engines: Vec<Arc<dyn TradingEngine>>,
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self {
            engines: Vec::new(),
        }
    }

    /// Register a trading engine. Logs name + enabled/paper state.
    pub fn register(&mut self, engine: Arc<dyn TradingEngine>) {
        tracing::info!(
            engine = engine.name(),
            enabled = engine.enabled(),
            paper_mode = engine.paper_mode(),
            "Registered trading engine"
        );
        self.engines.push(engine);
    }

    /// All registered engines (enabled and disabled).
    pub fn engines(&self) -> &[Arc<dyn TradingEngine>] {
        &self.engines
    }

    /// Iterator over enabled engines only.
    pub fn enabled_engines(&self) -> impl Iterator<Item = &Arc<dyn TradingEngine>> {
        self.engines.iter().filter(|e| e.enabled())
    }

    /// Synchronous fan-out — non-async, must be fast (<1µs per engine).
    pub fn dispatch_token_created(&self, mint: [u8; 32], ts_ms: u64) {
        for engine in self.enabled_engines() {
            engine.on_token_created(mint, ts_ms);
        }
    }

    /// Each engine gets its own `tokio::spawn` — one slow engine doesn't block another.
    /// `GraduationEvent` is Clone (~170 bytes), cheap to copy per engine.
    pub fn dispatch_graduation(&self, event: GraduationEvent) {
        for engine in self.enabled_engines() {
            let engine = Arc::clone(engine);
            let event = event.clone();
            tokio::spawn(async move {
                engine.on_graduation(event).await;
            });
        }
    }

    /// Sequential tick dispatch. Each engine internally throttles (returns in <1ms when idle).
    /// No `tokio::spawn` overhead — 50ms tick, N engines all return immediately when idle.
    pub async fn dispatch_tick(&self, ts_ms: u64) {
        for engine in self.enabled_engines() {
            engine.on_tick(ts_ms).await;
        }
    }

    /// Trigger post-startup recovery for all enabled engines (5s delay, spawned).
    pub async fn trigger_startup_recovery(&self) {
        for engine in &self.engines {
            if engine.enabled() {
                let engine = Arc::clone(engine);
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    engine.on_startup_recovery().await;
                });
            }
        }
    }

    /// Graceful shutdown — sequential, awaits each engine.
    pub async fn shutdown_all(&self) {
        for engine in &self.engines {
            engine.shutdown().await;
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::trading_engine::{EngineHealthSnapshot, GraduationEvent, PumpSwapVaults};
    use crate::feeds::MigrationSource;
    use crate::momentum::types::GradEnrichment;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Mock trading engine with atomic counters for verifying dispatch behavior.
    struct MockEngine {
        is_enabled: bool,
        tick_count: Arc<AtomicU32>,
        graduation_count: Arc<AtomicU32>,
    }

    impl MockEngine {
        fn new(enabled: bool) -> Self {
            Self {
                is_enabled: enabled,
                tick_count: Arc::new(AtomicU32::new(0)),
                graduation_count: Arc::new(AtomicU32::new(0)),
            }
        }

        fn tick_count(&self) -> u32 {
            self.tick_count.load(Ordering::SeqCst)
        }

        fn graduation_count(&self) -> u32 {
            self.graduation_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl TradingEngine for MockEngine {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn paper_mode(&self) -> bool {
            true
        }

        fn enabled(&self) -> bool {
            self.is_enabled
        }

        fn on_token_created(&self, _mint: [u8; 32], _ts_ms: u64) {}

        async fn on_graduation(&self, _event: GraduationEvent) {
            self.graduation_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn on_tick(&self, _ts_ms: u64) {
            self.tick_count.fetch_add(1, Ordering::SeqCst);
        }

        fn health(&self) -> EngineHealthSnapshot {
            EngineHealthSnapshot::Unknown(serde_json::json!({}))
        }
    }

    /// Helper: create a test GraduationEvent fixture.
    fn test_graduation_event() -> GraduationEvent {
        GraduationEvent {
            mint: [1u8; 32],
            sig: [2u8; 64],
            ts_ms: 1000,
            source: MigrationSource::HeliusLogs,
            enrichment: GradEnrichment::UNKNOWN,
            pumpswap_vaults: None,
        }
    }

    #[tokio::test]
    async fn test_registry_dispatches_tick_to_enabled_engine() {
        let mock = Arc::new(MockEngine::new(true));
        let mut registry = EngineRegistry::new();
        registry.register(mock.clone());

        registry.dispatch_tick(1000).await;

        assert_eq!(mock.tick_count(), 1);
    }

    #[tokio::test]
    async fn test_registry_skips_disabled_engine() {
        let mock = Arc::new(MockEngine::new(false)); // disabled
        let mut registry = EngineRegistry::new();
        registry.register(mock.clone());

        registry.dispatch_tick(1000).await;

        assert_eq!(mock.tick_count(), 0);
    }

    #[tokio::test]
    async fn test_registry_fans_out_graduation_to_multiple_engines() {
        let a = Arc::new(MockEngine::new(true));
        let b = Arc::new(MockEngine::new(true));
        let c = Arc::new(MockEngine::new(false)); // disabled

        let mut registry = EngineRegistry::new();
        registry.register(a.clone());
        registry.register(b.clone());
        registry.register(c.clone());

        registry.dispatch_graduation(test_graduation_event());

        // Let spawned tasks complete
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(a.graduation_count(), 1);
        assert_eq!(b.graduation_count(), 1);
        assert_eq!(c.graduation_count(), 0); // disabled, skipped
    }
}
