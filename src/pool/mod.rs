//! The pool: a fixed number of LibreOfficeKit instances, each a child process the server owns (the timeout, the kill,
//! the replacement, the retry, the recycling and the RSS read are all here, none in the instance), started in
//! parallel at boot, where a failed start is retried like a replacement's, a bounded number of times, since a boot
//! that dies on one transient failure gives a crash loop rather than a slower start; a conversion borrows one with a
//! bounded wait and gives it back; an instance that dies, or is retired, is killed and replaced in the background
//! while the rest keep serving; a conversion that meets a dead instance is retried once on another; `/readyz` is
//! ready while at least one instance is alive.
//!
//! The instance limits are the pool's too, because reliability is part of the function: [`Pool::convert`] bounds each
//! attempt by the conversion timeout, and past it retires the instance with the reason `timeout` (killed with its
//! process group, its profile deleted, replaced); an instance given back after [`InstanceLimits::max_conversions`]
//! conversions, or with its RSS above [`InstanceLimits::max_memory`], is recycled the same way with the reason
//! `recycle`, between conversions. During a conversion the instance's RSS is read every [`HARD_MEMORY_POLL`]; above
//! [`InstanceLimits::hard_memory`] the conversion is cut short at once with [`PoolError::MemoryLimit`] and the
//! instance retired with the reason `memory_limit`, so a content bomb costs its own request and never the pod's
//! memory; it is not retried. A [`Lease`] dropped in the middle of a conversion (the request future dropped by a
//! client disconnect) kills and replaces its instance with the reason `cancelled`, so its slot is free at once
//! (LibreOfficeKit cannot interrupt a running load or export). The HTTP answers are `crate::conversion`'s.
//!
//! Instruments: `docx_to_pdf.pool.wait.duration`, `docx_to_pdf.pool.instances`,
//! `docx_to_pdf.pool.size`, `docx_to_pdf.instance.starts`, `docx_to_pdf.instance.start.duration`,
//! `docx_to_pdf.instance.memory.usage`. Spans: `pool.acquire` and `instance.start`; a death, a
//! retirement and a replacement are events.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter, ObservableGauge};
use tokio::sync::{Semaphore, TryAcquireError};
use tracing::Instrument as _;

use crate::config::Config;
use crate::instance::driver::{ConvertError, Instance, Launcher, StartError};
use crate::instance::protocol::Answer;
use crate::readiness::Readiness;

#[cfg(test)]
mod tests;

/// How the pool is sized and bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSettings {
    /// Instances, i.e. conversions in parallel.
    pub size: usize,
    /// The longest a borrower waits for a free instance.
    pub wait_timeout: Duration,
    /// Borrowers allowed to wait at once; the next one is refused at once.
    pub max_waiting: usize,
    /// The longest an instance may take from spawn to its ready line.
    pub start_timeout: Duration,
}

impl PoolSettings {
    /// The settings `config` names.
    pub fn from_config(config: &Config) -> Self {
        Self {
            size: config.pool_size,
            wait_timeout: config.pool_wait_timeout,
            max_waiting: config.pool_max_waiting,
            start_timeout: config.instance_start_timeout,
        }
    }
}

/// What an instance may do before it is replaced (the defaults, in `crate::config`, are measured).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceLimits {
    /// The longest one conversion attempt may run; past it the instance is retired with `timeout`.
    pub conversion_timeout: Duration,
    /// Conversions after which an instance is recycled; 0 = never.
    pub max_conversions: u32,
    /// Resident bytes above which an instance is recycled when it is given back; 0 = no ceiling.
    pub max_memory: u64,
    /// Resident bytes above which a conversion is cut short and its instance killed, read every
    /// [`HARD_MEMORY_POLL`] while it converts; 0 = no hard limit. With no ceiling it is also
    /// the ceiling, so an instance given back above it never starts another conversion.
    pub hard_memory: u64,
}

/// How often a converting instance's RSS is read against [`InstanceLimits::hard_memory`]: a `/proc/<pid>/statm`
/// read, so 10 a second cost nothing measurable, and the limit is passed by at most what the instance allocates
/// in 100 ms before it is killed.
pub const HARD_MEMORY_POLL: Duration = Duration::from_millis(100);

impl Default for InstanceLimits {
    /// The defaults of `crate::config`.
    fn default() -> Self {
        Self {
            conversion_timeout: crate::config::DEFAULT_CONVERSION_TIMEOUT,
            max_conversions: crate::config::DEFAULT_INSTANCE_MAX_CONVERSIONS,
            max_memory: crate::config::DEFAULT_INSTANCE_MAX_MEMORY,
            hard_memory: crate::config::DEFAULT_INSTANCE_HARD_MEMORY,
        }
    }
}

impl InstanceLimits {
    /// The limits `config` names.
    pub fn from_config(config: &Config) -> Self {
        Self {
            conversion_timeout: config.conversion_timeout,
            max_conversions: config.instance_max_conversions,
            max_memory: config.instance_max_memory,
            hard_memory: config.instance_hard_memory,
        }
    }
}

/// Why an instance was started: the `reason` of `docx_to_pdf.instance.starts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartReason {
    /// At boot.
    Initial,
    /// In place of one that died (its pipe closed or it exited) or answered garbage.
    Crash,
    /// In place of one killed past the conversion timeout.
    Timeout,
    /// In place of one killed because its conversion was abandoned: the client disconnected.
    Cancelled,
    /// In place of one recycled after a number of conversions or above the memory ceiling, so a leak in
    /// LibreOffice does not grow without bound.
    Recycle,
    /// In place of one killed mid-conversion above the hard memory limit.
    MemoryLimit,
    /// Again, after the previous start of the slot failed or timed out: a replacement's, or one at boot.
    Failed,
}

impl StartReason {
    /// The attribute value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Crash => "crash",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Recycle => "recycle",
            Self::MemoryLimit => "memory_limit",
            Self::Failed => "failed",
        }
    }
}

/// Why no instance was lent. Each is a 503 with `Retry-After`: the service is busy, and a retry later may succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireError {
    /// No instance came free within the wait limit.
    Timeout,
    /// As many borrowers as allowed are already waiting.
    QueueFull,
    /// The pool is shutting down.
    Closed,
    /// The platform probe holds the pool.
    Reserved,
}

impl AcquireError {
    /// A low-cardinality identifier, for `error.type`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "pool_wait_timeout",
            Self::QueueFull => "pool_queue_full",
            Self::Closed => "pool_closed",
            Self::Reserved => "pool_reserved",
        }
    }
}

impl fmt::Display for AcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Timeout => "no instance came free within the wait limit",
            Self::QueueFull => "too many requests are already waiting for an instance",
            Self::Closed => "the service is shutting down",
            Self::Reserved => "the service is measuring its platform",
        })
    }
}

impl std::error::Error for AcquireError {}

/// Why an instance did not start.
#[derive(Debug)]
pub enum StartFailure {
    /// It ended, or could not be spawned, before its ready line.
    Start(StartError),
    /// No ready line within the start timeout; it has been killed.
    TimedOut(Duration),
}

impl fmt::Display for StartFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start(e) => e.fmt(f),
            Self::TimedOut(t) => {
                write!(f, "the instance was not ready within {} s", t.as_secs_f64())
            }
        }
    }
}

impl std::error::Error for StartFailure {}

/// Why a conversion through [`Pool::convert`] got no answer.
#[derive(Debug)]
pub enum PoolError {
    /// No instance was lent (503).
    Acquire(AcquireError),
    /// The instance gave no answer; for [`ConvertError::Dead`], the retry's instance died too.
    Convert(ConvertError),
    /// No answer within the conversion timeout; the instance has been retired (504).
    Timeout(Duration),
    /// The instance's RSS passed the hard memory limit during the conversion; the instance has been retired
    /// and the conversion is not retried (413: the same document would only kill another instance).
    MemoryLimit {
        /// The RSS read above the limit, in bytes.
        resident: u64,
        /// The hard limit, in bytes.
        limit: u64,
    },
}

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acquire(e) => e.fmt(f),
            Self::Convert(e) => e.fmt(f),
            Self::Timeout(t) => {
                write!(f, "the conversion did not end within {} s", t.as_secs_f64())
            }
            Self::MemoryLimit { resident, limit } => write!(
                f,
                "the instance reached {resident} bytes during the conversion, above the hard limit of {limit}"
            ),
        }
    }
}

impl std::error::Error for PoolError {}

impl From<AcquireError> for PoolError {
    fn from(e: AcquireError) -> Self {
        Self::Acquire(e)
    }
}

impl From<ConvertError> for PoolError {
    fn from(e: ConvertError) -> Self {
        Self::Convert(e)
    }
}

/// A conversion's answer, and whether it took the retry.
#[derive(Debug)]
pub struct Conversion {
    /// The instance's answer, including its typed failures.
    pub answer: Answer,
    /// The first instance died and this answer came from the retry (`retried` of
    /// `docx_to_pdf.conversions`).
    pub retried: bool,
}

/// How many instances are in each state now: `docx_to_pdf.pool.instances`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Ready and not lent.
    pub idle: usize,
    /// Lent.
    pub busy: usize,
    /// Being started, at boot or as a replacement.
    pub starting: usize,
}

/// The pool. Clones share it.
#[derive(Clone)]
pub struct Pool {
    inner: Arc<Inner>,
}

struct Inner {
    launcher: Launcher,
    settings: PoolSettings,
    limits: InstanceLimits,
    readiness: Readiness,
    /// One permit per idle instance.
    available: Semaphore,
    state: Arc<Mutex<State>>,
    waiting: AtomicUsize,
    closed: AtomicBool,
    /// The platform probe holds the pool: other borrowers are refused.
    reserved: AtomicBool,
    metrics: Metrics,
}

#[derive(Default)]
struct State {
    idle: Vec<Pooled>,
    /// The lent instances, as (slot, pid).
    lent: Vec<(usize, u32)>,
    starting: usize,
}

impl State {
    fn counts(&self) -> Counts {
        Counts {
            idle: self.idle.len(),
            busy: self.lent.len(),
            starting: self.starting,
        }
    }

    /// Every live instance's slot and pid, idle or lent.
    fn slots(&self) -> Vec<(usize, u32)> {
        let idle = self.idle.iter().map(|p| (p.slot, p.instance.pid()));
        idle.chain(self.lent.iter().copied()).collect()
    }

    fn unlend(&mut self, slot: usize) {
        if let Some(at) = self.lent.iter().position(|&(s, _)| s == slot) {
            self.lent.swap_remove(at);
        }
    }
}

/// An idle instance, its pool slot, and the conversions it has done, for recycling.
struct Pooled {
    /// `0..size`; a replacement takes the slot of the instance it replaces.
    slot: usize,
    instance: Instance,
    conversions: u32,
}

impl Pooled {
    fn new(slot: usize, instance: Instance) -> Self {
        Self {
            slot,
            instance,
            conversions: 0,
        }
    }
}

struct Metrics {
    wait: Histogram<f64>,
    starts: Counter<u64>,
    start_duration: Histogram<f64>,
    _instances: ObservableGauge<u64>,
    _size: ObservableGauge<u64>,
    _memory: ObservableGauge<u64>,
    peak: Histogram<u64>,
}

/// Bytes, for the peak-memory histogram: from an idle instance's size to past any container's memory.
const MEMORY_BUCKETS: &[f64] = &[
    67_108_864.0,
    134_217_728.0,
    268_435_456.0,
    536_870_912.0,
    1_073_741_824.0,
    2_147_483_648.0,
    4_294_967_296.0,
    8_589_934_592.0,
    17_179_869_184.0,
];

/// Seconds, for the wait and start histograms: from a free instance at once to the start timeout's range.
const SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

impl Metrics {
    fn new(meter: &Meter, size: usize, state: Weak<Mutex<State>>) -> Self {
        let size = u64::try_from(size).unwrap_or(u64::MAX);
        let memory_state = state.clone();
        Self {
            wait: meter
                .f64_histogram("docx_to_pdf.pool.wait.duration")
                .with_unit("s")
                .with_description("How long a conversion waited for an instance, by outcome")
                .with_boundaries(SECONDS_BUCKETS.to_vec())
                .build(),
            starts: meter
                .u64_counter("docx_to_pdf.instance.starts")
                .with_description("Instance starts, by the reason for the start")
                .build(),
            start_duration: meter
                .f64_histogram("docx_to_pdf.instance.start.duration")
                .with_unit("s")
                .with_description("From spawning an instance to its ready line")
                .with_boundaries(SECONDS_BUCKETS.to_vec())
                .build(),
            _instances: meter
                .u64_observable_gauge("docx_to_pdf.pool.instances")
                .with_description("Instances now, by state")
                .with_callback(move |observer| {
                    let Some(state) = state.upgrade() else {
                        return;
                    };
                    let counts = lock(&state).counts();
                    for (name, n) in [
                        ("idle", counts.idle),
                        ("busy", counts.busy),
                        ("starting", counts.starting),
                    ] {
                        observer.observe(n as u64, &[KeyValue::new("state", name)]);
                    }
                })
                .build(),
            _size: meter
                .u64_observable_gauge("docx_to_pdf.pool.size")
                .with_description("The configured number of instances")
                .with_callback(move |observer| observer.observe(size, &[]))
                .build(),
            _memory: meter
                .u64_observable_gauge("docx_to_pdf.instance.memory.usage")
                .with_unit("By")
                .with_description("Each instance process's resident set size, by its pool slot")
                .with_callback(move |observer| {
                    let Some(state) = memory_state.upgrade() else {
                        return;
                    };
                    let slots = lock(&state).slots();
                    for (slot, pid) in slots {
                        if let Some(rss) = crate::process_metrics::resident_bytes_of(pid) {
                            let slot = i64::try_from(slot).unwrap_or(i64::MAX);
                            observer.observe(rss, &[KeyValue::new("instance", slot)]);
                        }
                    }
                })
                .build(),
            peak: meter
                .u64_histogram("docx_to_pdf.instance.memory.peak")
                .with_unit("By")
                .with_description(
                    "Each conversion's peak resident set size of the instance that ran it, whatever ended the \
                     conversion, where the instance was alive to read",
                )
                .with_boundaries(MEMORY_BUCKETS.to_vec())
                .build(),
        }
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A panic while holding the lock leaves counts that are still the best there is.
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Pool {
    /// [`Pool::start_with_limits`] with the default [`InstanceLimits::default`].
    pub async fn start(
        launcher: Launcher,
        settings: PoolSettings,
        readiness: Readiness,
        meter: &Meter,
    ) -> Result<Self, StartFailure> {
        Self::start_with_limits(
            launcher,
            settings,
            InstanceLimits::default(),
            readiness,
            meter,
        )
        .await
    }

    /// Starts `settings.size` instances in parallel, each bounded by the start timeout, and marks `readiness` ready
    /// once all are: not before, so a boot still retrying a start is not ready. A start that fails or times out is
    /// retried like a replacement's (the same growing pause, counted `failed`), up to [`INITIAL_START_ATTEMPTS`]
    /// attempts in all, since a first start can fail for a transient cause; a slot that runs out of attempts fails
    /// the boot with its last failure, the other slots stop retrying, and every started instance is killed. `limits`
    /// bound each conversion and each instance's life.
    pub async fn start_with_limits(
        launcher: Launcher,
        settings: PoolSettings,
        limits: InstanceLimits,
        readiness: Readiness,
        meter: &Meter,
    ) -> Result<Self, StartFailure> {
        let state = Arc::new(Mutex::new(State {
            starting: settings.size,
            ..State::default()
        }));
        let metrics = Metrics::new(meter, settings.size, Arc::downgrade(&state));
        let inner = Arc::new(Inner {
            launcher,
            available: Semaphore::new(0),
            state,
            waiting: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            reserved: AtomicBool::new(false),
            metrics,
            readiness,
            settings,
            limits,
        });
        let starts = (0..inner.settings.size).map(|slot| inner.boot_slot(slot));
        let mut started = Vec::with_capacity(inner.settings.size);
        let mut failure = None;
        for result in futures_util::future::join_all(starts).await {
            match result {
                Ok(instance) => started.push(instance),
                Err(e) => failure = failure.or(Some(e)),
            }
        }
        if let Some(e) = failure {
            lock(&inner.state).starting = 0;
            inner.closed.store(true, Ordering::SeqCst);
            drop(started);
            return Err(e);
        }
        {
            let mut state = lock(&inner.state);
            state.starting = 0;
            state.idle = started
                .into_iter()
                .enumerate()
                .map(|(slot, instance)| Pooled::new(slot, instance))
                .collect();
            inner.readiness.mark_ready();
        }
        inner.available.add_permits(inner.settings.size);
        tracing::info!(size = inner.settings.size, "the pool has started");
        Ok(Self { inner })
    }

    /// The pool's settings.
    pub fn settings(&self) -> &PoolSettings {
        &self.inner.settings
    }

    /// The instance limits.
    pub fn limits(&self) -> &InstanceLimits {
        &self.inner.limits
    }

    /// How many instances are in each state now.
    pub fn counts(&self) -> Counts {
        lock(&self.inner.state).counts()
    }

    /// Borrows an idle instance: at once if one is free, else after waiting in line for at most the wait
    /// limit, unless as many borrowers as allowed already wait. Under the span `pool.acquire`.
    pub async fn acquire(&self) -> Result<Lease, AcquireError> {
        self.acquire_as(Borrower::Client).await
    }

    async fn acquire_as(&self, borrower: Borrower) -> Result<Lease, AcquireError> {
        let span = tracing::info_span!(
            "pool.acquire",
            outcome = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
        );
        async {
            let began = Instant::now();
            let permit = self.wait_for_permit(borrower).await;
            let outcome = if permit.is_ok() {
                "acquired"
            } else {
                "rejected"
            };
            self.inner.metrics.wait.record(
                began.elapsed().as_secs_f64(),
                &[KeyValue::new("outcome", outcome)],
            );
            let current = tracing::Span::current();
            current.record("outcome", outcome);
            if let Err(e) = permit {
                current.record("otel.status_code", "ERROR");
                current.record("error.type", e.as_str());
                return Err(e);
            }
            let pooled = {
                let mut state = lock(&self.inner.state);
                let pooled = state
                    .idle
                    .pop()
                    .expect("the pool keeps one permit per idle instance");
                state.lent.push((pooled.slot, pooled.instance.pid()));
                pooled
            };
            Ok(Lease {
                pool: self.inner.clone(),
                slot: pooled.slot,
                instance: Some(pooled.instance),
                conversions: pooled.conversions,
                timed_out: false,
                over_memory: None,
                peak_pending: false,
                in_flight: false,
                dead: false,
                retire: None,
            })
        }
        .instrument(span)
        .await
    }

    async fn wait_for_permit(&self, borrower: Borrower) -> Result<(), AcquireError> {
        let inner = &self.inner;
        if borrower == Borrower::Client && inner.reserved.load(Ordering::SeqCst) {
            return Err(AcquireError::Reserved);
        }
        match inner.available.try_acquire() {
            Ok(permit) => {
                permit.forget();
                return Ok(());
            }
            Err(TryAcquireError::Closed) => return Err(AcquireError::Closed),
            Err(TryAcquireError::NoPermits) => {}
        }
        // The probe never queues behind clients (none are let in) and waits out a replacement's start.
        let wait = match borrower {
            Borrower::Client => {
                let in_line = InLine::join(&inner.waiting);
                if in_line.ahead >= inner.settings.max_waiting {
                    return Err(AcquireError::QueueFull);
                }
                Some(in_line)
            }
            Borrower::Probe => None,
        };
        let limit = match wait {
            Some(_) => inner.settings.wait_timeout,
            None => inner
                .settings
                .wait_timeout
                .max(inner.settings.start_timeout),
        };
        match tokio::time::timeout(limit, inner.available.acquire()).await {
            Ok(Ok(permit)) => {
                permit.forget();
                Ok(())
            }
            Ok(Err(_)) => Err(AcquireError::Closed),
            Err(_) => Err(AcquireError::Timeout),
        }
    }

    /// Converts `input` to a PDF at `output` on a borrowed instance; if that instance dies during the
    /// conversion it is replaced in the background and the conversion is retried once on another instance
    /// (upstream's one retry, kept), which waits for one like any borrower. Each attempt is bounded by
    /// the conversion timeout: past it the instance is retired with the reason `timeout` and the answer is
    /// [`PoolError::Timeout`]. Each is watched against the hard memory limit: past it the instance is retired
    /// with the reason `memory_limit` and the answer is [`PoolError::MemoryLimit`], with no retry, since the
    /// same document would only kill another instance.
    pub async fn convert(&self, input: &Path, output: &Path) -> Result<Conversion, PoolError> {
        self.convert_as(Borrower::Client, input, output).await
    }

    async fn convert_as(
        &self,
        borrower: Borrower,
        input: &Path,
        output: &Path,
    ) -> Result<Conversion, PoolError> {
        let mut lease = self.acquire_as(borrower).await?;
        match self.attempt(&mut lease, input, output).await {
            Err(ConvertError::Dead(status)) => {
                tracing::warn!(
                    instance_pid = lease.pid(),
                    status = ?status,
                    "the instance died during a conversion; retrying once on another instance"
                );
                drop(lease);
                let mut lease = self.acquire_as(borrower).await?;
                let answer = self.attempt(&mut lease, input, output).await;
                if answer.is_err()
                    && let Some(cut) = self.cut_short(&mut lease)
                {
                    return Err(cut);
                }
                Ok(Conversion {
                    answer: answer?,
                    retried: true,
                })
            }
            Err(_) if lease.timed_out || lease.over_memory.is_some() => Err(self
                .cut_short(&mut lease)
                .expect("an attempt that timed out or passed the memory limit was cut short")),
            answered => Ok(Conversion {
                answer: answered?,
                retried: false,
            }),
        }
    }

    /// One conversion on `lease`, bounded by the conversion timeout and watched against the hard memory
    /// limit. Past the timeout `lease.timed_out` is set, above the limit `lease.over_memory`, and the error is
    /// [`ConvertError::Unusable`]; the caller retires the lease ([`Pool::cut_short`]). An answer that is ready
    /// when the limit is read as passed wins.
    async fn attempt(
        &self,
        lease: &mut Lease,
        input: &Path,
        output: &Path,
    ) -> Result<Answer, ConvertError> {
        enum Ended {
            Answered(Result<Answer, ConvertError>),
            TimedOut,
            OverMemory(u64),
        }
        let timeout = self.inner.limits.conversion_timeout;
        let limit = self.inner.limits.hard_memory;
        let pid = lease.pid();
        let ended = {
            let converted = tokio::time::timeout(timeout, lease.convert(input, output));
            tokio::select! {
                biased;
                converted = converted => match converted {
                    Ok(answered) => Ended::Answered(answered),
                    Err(_) => Ended::TimedOut,
                },
                resident = watch_memory(pid, limit) => Ended::OverMemory(resident),
            }
        };
        match ended {
            Ended::Answered(answered) => answered,
            Ended::TimedOut => {
                tracing::warn!(
                    instance_pid = pid,
                    timeout_seconds = timeout.as_secs_f64(),
                    "the conversion did not end within the timeout; killing its instance"
                );
                lease.timed_out = true;
                lease.record_peak();
                Err(ConvertError::Unusable)
            }
            Ended::OverMemory(resident) => {
                tracing::warn!(
                    instance_pid = pid,
                    instance_slot = lease.slot(),
                    reason = StartReason::MemoryLimit.as_str(),
                    rss_bytes = resident,
                    hard_memory_bytes = limit,
                    "the instance passed the hard memory limit during a conversion; killing it, the request \
                     fails"
                );
                lease.over_memory = Some(resident);
                lease.record_peak();
                Err(ConvertError::Unusable)
            }
        }
    }

    /// The error for an attempt the pool cut short, its lease set to be retired with the matching reason:
    /// `memory_limit` above the hard memory limit, `timeout` past the conversion timeout. `None` when the
    /// attempt was not cut short.
    fn cut_short(&self, lease: &mut Lease) -> Option<PoolError> {
        let limits = &self.inner.limits;
        if let Some(resident) = lease.over_memory {
            lease.retire = Some(StartReason::MemoryLimit);
            return Some(PoolError::MemoryLimit {
                resident,
                limit: limits.hard_memory,
            });
        }
        if lease.timed_out {
            lease.retire = Some(StartReason::Timeout);
            return Some(PoolError::Timeout(limits.conversion_timeout));
        }
        None
    }

    /// Reserves the whole pool for the platform probe: from now on every other borrower is
    /// refused with [`AcquireError::Reserved`] (503) and `/readyz` is not ready, so a Service routes around
    /// the pod; then waits until every instance is idle (the conversions under way end, a replacement
    /// starts), bounded by the conversion timeout plus the start timeout. Dropping the reservation gives the
    /// pool back: readiness follows the instances again and borrowers are let in.
    pub async fn reserve(&self) -> Result<Reservation, ReserveError> {
        let inner = &self.inner;
        if inner.closed.load(Ordering::SeqCst) {
            return Err(ReserveError::Closed);
        }
        if inner
            .reserved
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(ReserveError::Busy);
        }
        inner.readiness.begin_drain();
        let reservation = Reservation { pool: self.clone() };
        tracing::info!("the pool is reserved for the platform probe; draining it");
        let size = u32::try_from(inner.settings.size).unwrap_or(u32::MAX);
        let limit = inner.limits.conversion_timeout + inner.settings.start_timeout;
        match tokio::time::timeout(limit, inner.available.acquire_many(size)).await {
            // Every instance is idle: the permits go straight back, for the probe's own borrowing.
            Ok(Ok(permits)) => drop(permits),
            Ok(Err(_)) => return Err(ReserveError::Closed),
            Err(_) => return Err(ReserveError::Timeout(limit)),
        }
        Ok(reservation)
    }

    /// Stops lending (waiting and later borrowers get [`AcquireError::Closed`]), ends readiness, and stops
    /// every idle instance, each bounded by `stop_timeout` (past it, killed). A lent instance is killed when
    /// its lease is dropped; no replacement starts any more.
    pub async fn shutdown(&self, stop_timeout: Duration) {
        let inner = &self.inner;
        inner.closed.store(true, Ordering::SeqCst);
        inner.available.close();
        let idle = {
            let mut state = lock(&inner.state);
            inner.readiness.mark_not_ready();
            std::mem::take(&mut state.idle)
        };
        let stops = idle.into_iter().map(|Pooled { instance, .. }| async move {
            let pid = instance.pid();
            if tokio::time::timeout(stop_timeout, instance.stop())
                .await
                .is_err()
            {
                tracing::warn!(
                    instance_pid = pid,
                    "the instance did not stop in time; killed"
                );
            }
        });
        futures_util::future::join_all(stops).await;
        tracing::info!("the pool has stopped");
    }
}

/// Who borrows: a client's conversion, or the platform probe holding a [`Reservation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Borrower {
    Client,
    Probe,
}

/// Why the pool could not be reserved for the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveError {
    /// Another probe holds it.
    Busy,
    /// The pool is shutting down.
    Closed,
    /// The instances did not all come idle within this long; the pool has been given back.
    Timeout(Duration),
}

impl fmt::Display for ReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("a probe is already running"),
            Self::Closed => f.write_str("the service is shutting down"),
            Self::Timeout(t) => write!(
                f,
                "the instances did not all come free within {} s",
                t.as_secs_f64()
            ),
        }
    }
}

impl std::error::Error for ReserveError {}

/// The whole pool, held by the platform probe ([`Pool::reserve`]): its conversions are the only ones while it
/// lives. Dropped, the pool is given back.
pub struct Reservation {
    pool: Pool,
}

impl Reservation {
    /// The pool's size: the conversions the probe may run at once.
    pub fn size(&self) -> usize {
        self.pool.inner.settings.size
    }

    /// [`Pool::convert`] for the probe: refused to no one, and with no queue limit.
    pub async fn convert(&self, input: &Path, output: &Path) -> Result<Conversion, PoolError> {
        self.pool.convert_as(Borrower::Probe, input, output).await
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let inner = &self.pool.inner;
        inner.reserved.store(false, Ordering::SeqCst);
        inner.readiness.end_drain();
        tracing::info!("the platform probe has given the pool back");
    }
}

/// A place in the line of borrowers waiting for an instance; leaving it (on any path) frees the place.
struct InLine<'a> {
    waiting: &'a AtomicUsize,
    /// Borrowers already in line when this one joined.
    ahead: usize,
}

impl<'a> InLine<'a> {
    fn join(waiting: &'a AtomicUsize) -> Self {
        let ahead = waiting.fetch_add(1, Ordering::SeqCst);
        InLine { waiting, ahead }
    }
}

impl Drop for InLine<'_> {
    fn drop(&mut self) {
        self.waiting.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Inner {
    /// One start, bounded by the start timeout, counted and timed, under the span `instance.start`.
    /// `slot` is the pool slot it fills (the memory gauge's `instance` label), logged as `instance_slot`.
    async fn start_instance(
        &self,
        slot: usize,
        reason: StartReason,
    ) -> Result<Instance, StartFailure> {
        let span = tracing::info_span!(
            "instance.start",
            reason = reason.as_str(),
            instance_slot = slot,
            instance_pid = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
        );
        async {
            let began = Instant::now();
            let timeout = self.settings.start_timeout;
            let started = tokio::time::timeout(timeout, Instance::start(&self.launcher)).await;
            self.metrics
                .starts
                .add(1, &[KeyValue::new("reason", reason.as_str())]);
            let current = tracing::Span::current();
            match started {
                Ok(Ok(instance)) => {
                    self.metrics
                        .start_duration
                        .record(began.elapsed().as_secs_f64(), &[]);
                    current.record("instance_pid", instance.pid());
                    tracing::info!(
                        instance_pid = instance.pid(),
                        instance_slot = slot,
                        reason = reason.as_str(),
                        init_ms = instance.ready().init_ms,
                        spawn_to_ready_ms = u64::try_from(instance.spawn_to_ready().as_millis())
                            .unwrap_or(u64::MAX),
                        "instance ready"
                    );
                    Ok(instance)
                }
                Ok(Err(e)) => {
                    current.record("otel.status_code", "ERROR");
                    current.record("error.type", "start_failed");
                    tracing::error!(reason = reason.as_str(), error = %e, "instance start failed");
                    Err(StartFailure::Start(e))
                }
                Err(_) => {
                    current.record("otel.status_code", "ERROR");
                    current.record("error.type", "start_timeout");
                    tracing::error!(
                        reason = reason.as_str(),
                        timeout_seconds = timeout.as_secs_f64(),
                        "instance start timed out; killed"
                    );
                    Err(StartFailure::TimedOut(timeout))
                }
            }
        }
        .instrument(span)
        .await
    }

    /// Fills `slot` at boot: a start with the reason `initial`, then, after each failure, a pause and
    /// another start with the reason `failed`, up to [`INITIAL_START_ATTEMPTS`] in all. The pauses are a
    /// replacement's. A slot that runs out of attempts closes the pool, so the other slots stop retrying
    /// and the boot fails at once.
    async fn boot_slot(&self, slot: usize) -> Result<Instance, StartFailure> {
        let mut reason = StartReason::Initial;
        let mut pause = REPLACEMENT_RETRY_FIRST;
        let mut attempt = 1;
        loop {
            let failure = match self.start_instance(slot, reason).await {
                Ok(instance) => return Ok(instance),
                Err(e) => e,
            };
            if attempt >= INITIAL_START_ATTEMPTS || self.closed.load(Ordering::SeqCst) {
                self.closed.store(true, Ordering::SeqCst);
                return Err(failure);
            }
            tracing::warn!(
                instance_slot = slot,
                attempt,
                attempts = INITIAL_START_ATTEMPTS,
                pause_ms = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX),
                "retrying a failed first start"
            );
            tokio::time::sleep(pause).await;
            if self.closed.load(Ordering::SeqCst) {
                return Err(failure);
            }
            reason = StartReason::Failed;
            pause = (pause * 2).min(REPLACEMENT_RETRY_MAX);
            attempt += 1;
        }
    }

    /// A lent instance comes back: usable and not retired, to the idle list; otherwise killed and, unless
    /// the pool is closed, replaced in the background.
    fn give_back(
        self: &Arc<Self>,
        slot: usize,
        instance: Instance,
        conversions: u32,
        retire: Option<StartReason>,
    ) {
        let closed = self.closed.load(Ordering::SeqCst);
        let pid = instance.pid();
        if retire.is_none() && !closed {
            {
                let mut state = lock(&self.state);
                state.unlend(slot);
                state.idle.push(Pooled {
                    slot,
                    instance,
                    conversions,
                });
            }
            self.available.add_permits(1);
            return;
        }
        {
            let mut state = lock(&self.state);
            state.unlend(slot);
            if !closed {
                state.starting += 1;
            }
            if state.idle.len() + state.lent.len() == 0 {
                self.readiness.mark_not_ready();
            }
        }
        if let Some(reason) = retire {
            tracing::warn!(
                instance_pid = pid,
                reason = reason.as_str(),
                "instance retired; killing and replacing it"
            );
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // No runtime to replace it on: dropping it kills its group and deletes its profile.
            drop(instance);
            if !closed {
                lock(&self.state).starting -= 1;
            }
            return;
        };
        let pool = self.clone();
        runtime.spawn(async move {
            let status = instance.kill().await;
            tracing::info!(instance_pid = pid, status = ?status, "instance ended");
            if let Some(reason) = retire.filter(|_| !closed) {
                pool.replace(slot, reason).await;
            }
        });
    }

    /// Whether an instance given back usable is due for recycling: enough conversions, or its RSS above the
    /// ceiling (the hard memory limit when there is no ceiling, so an instance that passed it between the last
    /// read and its answer is not killed at the start of the next conversion). The RSS read is a small file
    /// read (`/proc/<pid>/statm`), done between conversions.
    fn due_for_recycling(&self, instance: &Instance, conversions: u32) -> bool {
        let limits = &self.limits;
        if limits.max_conversions > 0 && conversions >= limits.max_conversions {
            tracing::info!(
                instance_pid = instance.pid(),
                conversions,
                "the instance has done its maximum conversions; recycling it"
            );
            return true;
        }
        let ceiling = if limits.max_memory > 0 {
            limits.max_memory
        } else {
            limits.hard_memory
        };
        if ceiling > 0
            && let Some(rss) = crate::process_metrics::resident_bytes_of(instance.pid())
            && rss > ceiling
        {
            tracing::info!(
                instance_pid = instance.pid(),
                rss_bytes = rss,
                max_memory_bytes = ceiling,
                "the instance is above the memory ceiling; recycling it"
            );
            return true;
        }
        false
    }

    /// Starts a replacement until one starts or the pool closes; a failed start is retried with a growing
    /// pause and counted with the reason `failed`. The replacement takes `slot`.
    async fn replace(self: Arc<Self>, slot: usize, reason: StartReason) {
        let mut reason = reason;
        let mut pause = REPLACEMENT_RETRY_FIRST;
        loop {
            if self.closed.load(Ordering::SeqCst) {
                lock(&self.state).starting -= 1;
                return;
            }
            if let Ok(instance) = self.start_instance(slot, reason).await {
                let mut state = lock(&self.state);
                state.starting -= 1;
                if self.closed.load(Ordering::SeqCst) {
                    drop(state);
                    drop(instance);
                    return;
                }
                state.idle.push(Pooled::new(slot, instance));
                self.readiness.mark_ready();
                drop(state);
                self.available.add_permits(1);
                return;
            }
            reason = StartReason::Failed;
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(REPLACEMENT_RETRY_MAX);
        }
    }
}

/// Starts a boot tries for one slot before it fails: the first and two retries, after the replacement's pauses
/// (0.5 s, then 1 s), so a transient failure (two first starts racing for a shared folder was one) costs a second
/// and a persistent one still fails the boot within seconds rather than retrying forever.
pub const INITIAL_START_ATTEMPTS: u32 = 3;

/// Resolves with `pid`'s RSS once a read finds it above `limit`, reading at once and then every
/// [`HARD_MEMORY_POLL`]; never, with `limit` 0. A read that finds no process (it died) waits for the next: the
/// conversion's own answer reports the death.
async fn watch_memory(pid: u32, limit: u64) -> u64 {
    if limit == 0 {
        return std::future::pending().await;
    }
    let mut poll = tokio::time::interval(HARD_MEMORY_POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        poll.tick().await;
        if let Some(resident) = crate::process_metrics::resident_bytes_of(pid)
            && resident > limit
        {
            return resident;
        }
    }
}

/// The pause after a replacement's first failed start, doubled after each further one up to the maximum.
const REPLACEMENT_RETRY_FIRST: Duration = Duration::from_millis(500);
const REPLACEMENT_RETRY_MAX: Duration = Duration::from_secs(30);

/// A borrowed instance. Dropping it gives the instance back; if the instance died, if a conversion on it was
/// abandoned (the lease dropped mid-conversion), or if it was retired, it is killed and replaced instead.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one lease, each read on its own"
)]
pub struct Lease {
    pool: Arc<Inner>,
    /// The pool slot the instance holds.
    slot: usize,
    instance: Option<Instance>,
    /// Conversions the instance has answered, this lease's included.
    conversions: u32,
    /// The conversion timeout cut its last conversion short.
    timed_out: bool,
    /// The hard memory limit cut its last conversion short, at this RSS.
    over_memory: Option<u64>,
    /// A conversion reset the instance's high-water mark and its peak is not recorded yet.
    peak_pending: bool,
    in_flight: bool,
    dead: bool,
    retire: Option<StartReason>,
}

impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lease")
            .field("slot", &self.slot)
            .field("instance", &self.instance)
            .field("conversions", &self.conversions)
            .field("timed_out", &self.timed_out)
            .field("over_memory", &self.over_memory)
            .field("peak_pending", &self.peak_pending)
            .field("in_flight", &self.in_flight)
            .field("dead", &self.dead)
            .field("retire", &self.retire)
            .finish_non_exhaustive()
    }
}

impl Lease {
    /// The borrowed instance, read-only: its pid for the memory ceiling's `/proc/<pid>` read.
    pub fn instance(&self) -> &Instance {
        self.instance.as_ref().expect("a lease holds its instance")
    }

    /// The instance's process id (and process group id).
    pub fn pid(&self) -> u32 {
        self.instance().pid()
    }

    /// The pool slot the instance holds, `0..size`: the `instance` label of
    /// `docx_to_pdf.instance.memory.usage`, which a replacement inherits: one bounded series per slot, so a leak
    /// shows as a sawtooth that resets on recycle and an alert follows the slot across replacements.
    pub fn slot(&self) -> usize {
        self.slot
    }

    /// Converts on the borrowed instance. A dead instance ([`ConvertError::Dead`]) or garbage
    /// ([`ConvertError::BadAnswer`]) retires it with the reason `crash` when the lease is dropped; a call
    /// cancelled before its answer (a timeout around it, a dropped request) leaves it to be killed then.
    pub async fn convert(&mut self, input: &Path, output: &Path) -> Result<Answer, ConvertError> {
        let instance = self.instance.as_mut().expect("a lease holds its instance");
        let pid = instance.pid();
        crate::process_metrics::reset_peak_of(pid);
        self.peak_pending = true;
        self.in_flight = true;
        let answered = instance.convert(input, output).await;
        self.in_flight = false;
        self.record_peak();
        if answered.is_ok() {
            self.conversions = self.conversions.saturating_add(1);
        }
        if matches!(
            answered,
            Err(ConvertError::Dead(_) | ConvertError::BadAnswer(_))
        ) {
            self.dead = true;
        }
        answered
    }

    /// Records the instance's peak resident memory since its last conversion began, once per conversion and whatever
    /// ended it, so the heaviest conversions are not missing from it: an answer, the timeout, the hard memory limit,
    /// a dropped request. Read while the instance is still alive, before any kill; an instance that is already gone
    /// has no peak to read.
    fn record_peak(&mut self) {
        if !std::mem::take(&mut self.peak_pending) {
            return;
        }
        if let Some(instance) = &self.instance
            && let Some(peak) = crate::process_metrics::peak_resident_bytes_of(instance.pid())
        {
            self.pool.metrics.peak.record(peak, &[]);
        }
    }

    /// Kills the instance and replaces it in the background, counting the start with `reason`: `timeout`
    /// past the conversion timeout, `memory_limit` above the hard memory limit, `recycle` after enough
    /// conversions or above the memory ceiling.
    pub fn retire(mut self, reason: StartReason) {
        self.retire = Some(reason);
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // A conversion cut off before its answer (a dropped request) still has its peak.
        self.record_peak();
        let Some(instance) = self.instance.take() else {
            return;
        };
        let retire = if let Some(reason) = self.retire {
            Some(reason)
        } else if self.dead {
            Some(StartReason::Crash)
        } else if self.in_flight {
            Some(StartReason::Cancelled)
        } else if !instance.is_usable() {
            Some(StartReason::Crash)
        } else if self.pool.due_for_recycling(&instance, self.conversions) {
            Some(StartReason::Recycle)
        } else {
            None
        };
        self.pool
            .give_back(self.slot, instance, self.conversions, retire);
    }
}
