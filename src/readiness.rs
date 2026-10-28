//! Readiness: the seam `/readyz` reads. The pool marks the service ready once an instance can
//! take a conversion and not ready while none can; shutdown ends readiness for good. While the platform probe
//! holds the pool the service is drained: not ready, so a Service routes around it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Shared readiness state; clones see the same state.
#[derive(Debug, Clone, Default)]
pub struct Readiness {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    ready: AtomicBool,
    shutting_down: AtomicBool,
    draining: AtomicBool,
}

impl Readiness {
    /// Not ready, not shutting down.
    pub fn new() -> Self {
        Self::default()
    }

    /// At least one instance can take a conversion.
    pub fn mark_ready(&self) {
        self.inner.ready.store(true, Ordering::SeqCst);
    }

    /// No instance can take a conversion.
    pub fn mark_not_ready(&self) {
        self.inner.ready.store(false, Ordering::SeqCst);
    }

    /// Shutdown has begun: not ready from now on, whatever the pool says.
    pub fn begin_shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
    }

    /// Whether shutdown has begun.
    pub fn is_shutting_down(&self) -> bool {
        self.inner.shutting_down.load(Ordering::SeqCst)
    }

    /// The pool is reserved for the platform probe: not ready until [`Readiness::end_drain`].
    pub fn begin_drain(&self) {
        self.inner.draining.store(true, Ordering::SeqCst);
    }

    /// The probe has given the pool back.
    pub fn end_drain(&self) {
        self.inner.draining.store(false, Ordering::SeqCst);
    }

    /// What `/readyz` answers: ready, not drained for the probe and not shutting down.
    pub fn is_ready(&self) -> bool {
        self.inner.ready.load(Ordering::SeqCst)
            && !self.inner.draining.load(Ordering::SeqCst)
            && !self.is_shutting_down()
    }
}
