//! The pool against a stand-in instance: `sh -c <script> sh <profile dir>`, speaking the protocol. The real
//! instance under the pool is tested in `tests/pool_lok.rs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::instance::group::group_alive;
use crate::telemetry::{Telemetry, TelemetryBuilder};

/// Ready after a pause while the file `slow-start` exists in the profiles directory's parent marker dir;
/// never ready while `no-start` exists; exactly one start fails while `fail-once` exists (the start whose
/// `mkdir fail-once.taken` succeeds, as a first start that loses a race for a shared folder fails. Not
/// `rm fail-once`: two concurrent `rm`s of one file both exit 0 on macOS about once in 50, which failed two first
/// starts); answers `ok` and writes its pid to the output; sleeps first for an input whose name contains `slow`, and
/// for 30 s in a child process of its group for one whose name contains `hang`, and for 3 s for one whose name
/// contains `long`. Holds about 100 MB for a moment while starting if `big-start` exists, and while converting an
/// input whose name contains `big`; for one whose name contains `bomb` it becomes (`exec`, the same pid) a Perl
/// that takes 100 MB at once and keeps them, then sleeps 30 s (a content bomb). For one whose name
/// contains `hold` it becomes (`exec`, the same pid) a Perl that takes 100 MB, answers, and goes on answering
/// with them held, sleeping 30 s for an input whose name contains `hang`: an instance that is
/// already at 100 MB when its next conversion begins, so that conversion's peak is above 64 MiB however slowly
/// the machine runs.
fn script(markers: &Path) -> String {
    format!(
        r#"
        test -d "$1" || exit 9
        if [ -f '{markers}/slow-start' ]; then sleep 1; fi
        if [ -f '{markers}/no-start' ]; then exit 3; fi
        if [ -f '{markers}/fail-once' ] && mkdir '{markers}/fail-once.taken' 2>/dev/null; then exit 3; fi
        if [ -f '{markers}/big-start' ]; then big=$(head -c 100000000 /dev/zero | tr '\0' x); unset big; fi
        printf 'ready\t1\t%s\n' "$$"
        tab="$(printf '\t')"
        while IFS="$tab" read -r cmd input output; do
            case "$input" in
                *slow*) sleep 0.3 ;;
                *hang*) sleep 30 ;;
                *long*) sleep 3 ;;
                *bomb*) exec perl -e '$x = "x" x 100000000; sleep 30' ;;
                *big*) big=$(head -c 100000000 /dev/zero | tr '\0' x); unset big ;;
                *hold*) exec perl -e '
                    $x = "x" x 100000000;
                    $| = 1;
                    sub answer {{ open(my $f, ">", $_[0]) or die; print $f "$$\n"; close $f; print "ok\t1\t2\n"; }}
                    answer($ARGV[0]);
                    while (my $line = <STDIN>) {{
                        chomp $line;
                        my ($cmd, $input, $output) = split /\t/, $line;
                        sleep 30 if $input =~ /hang/;
                        answer($output);
                    }}' "$output" ;;
            esac
            printf '%s\n' "$$" > "$output"
            printf 'ok\t1\t2\n'
        done
        "#,
        markers = markers.display()
    )
}

struct Fixture {
    dir: tempfile::TempDir,
    telemetry: Telemetry,
    readiness: Readiness,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("profiles")).unwrap();
        std::fs::create_dir(dir.path().join("markers")).unwrap();
        std::fs::create_dir(dir.path().join("work")).unwrap();
        Self {
            dir,
            telemetry: TelemetryBuilder::new().build().unwrap(),
            readiness: Readiness::new(),
        }
    }

    fn markers(&self) -> PathBuf {
        self.dir.path().join("markers")
    }

    fn profiles(&self) -> PathBuf {
        self.dir.path().join("profiles")
    }

    fn launcher(&self) -> Launcher {
        Launcher::with_args(
            "/bin/sh",
            ["-c".to_string(), script(&self.markers()), "sh".to_string()],
            self.profiles(),
        )
    }

    fn mark(&self, name: &str) {
        std::fs::write(self.markers().join(name), "").unwrap();
    }

    fn unmark(&self, name: &str) {
        std::fs::remove_file(self.markers().join(name)).unwrap();
    }

    fn input(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join("work").join(name);
        std::fs::write(&path, "x").unwrap();
        path
    }

    fn output(&self, name: &str) -> PathBuf {
        self.dir.path().join("work").join(format!("{name}.pdf"))
    }

    async fn pool_with(&self, size: usize, limits: InstanceLimits) -> Pool {
        Pool::start_with_limits(
            self.launcher(),
            PoolSettings {
                size,
                wait_timeout: WAIT,
                max_waiting: 16,
                start_timeout: WAIT,
            },
            limits,
            self.readiness.clone(),
            &self.telemetry.meter(),
        )
        .await
        .unwrap()
    }

    /// Converts on `pool` and returns the pid of the instance that answered.
    async fn convert_on(&self, pool: &Pool, name: &str) -> u32 {
        let output = self.output(name);
        let conversion = pool.convert(&self.input(name), &output).await.unwrap();
        assert!(matches!(conversion.answer, Answer::Converted { .. }));
        std::fs::read_to_string(&output)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    async fn pool(&self, size: usize, wait: Duration, max_waiting: usize) -> Pool {
        Pool::start(
            self.launcher(),
            PoolSettings {
                size,
                wait_timeout: wait,
                max_waiting,
                start_timeout: WAIT,
            },
            self.readiness.clone(),
            &self.telemetry.meter(),
        )
        .await
        .unwrap()
    }

    fn metrics(&self) -> String {
        self.telemetry.prometheus_text()
    }

    fn profiles_left(&self) -> usize {
        std::fs::read_dir(self.profiles()).unwrap().count()
    }
}

/// The value of the one sample of `name` whose labels contain every one of `labels`; 0 if none.
fn sample(text: &str, name: &str, labels: &[&str]) -> f64 {
    let found: Vec<f64> = text
        .lines()
        .filter(|l| {
            l.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
        })
        .filter(|l| labels.iter().all(|label| l.contains(label)))
        .map(|l| l.rsplit(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(
        found.len() <= 1,
        "one sample of {name} {labels:?} in\n{text}"
    );
    found.first().copied().unwrap_or(0.0)
}

/// The bound on a wait that is not the subject of a test: a stand-in's start, a replacement, a borrower
/// waiting for one, a group's end. Generous, so a CPU-starved runner (other projects' containers on the same
/// VM) only makes a test slower; a passing test never waits for it. A limit a test is about is set
/// where that test sets it, tight.
const WAIT: Duration = Duration::from_secs(60);

async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "never: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[allow(
    unsafe_code,
    reason = "SIGKILL to a process group, which std has no form of"
)]
fn kill_group(pgid: u32) {
    // SAFETY: SIGKILL to a process group this test's pool started.
    unsafe {
        libc::kill(-pgid.cast_signed(), libc::SIGKILL);
    }
}

#[tokio::test]
async fn the_pool_starts_its_instances_and_counts_them() {
    let f = Fixture::new();
    assert!(!f.readiness.is_ready());
    let pool = f.pool(3, Duration::from_secs(1), 4).await;
    assert!(f.readiness.is_ready());
    assert_eq!(
        pool.counts(),
        Counts {
            idle: 3,
            busy: 0,
            starting: 0
        }
    );
    let m = f.metrics();
    assert_eq!(
        sample(
            &m,
            "docx_to_pdf_instance_starts_total",
            &["reason=\"initial\""]
        ),
        3.0
    );
    assert_eq!(sample(&m, "docx_to_pdf_pool_size", &[]), 3.0);
    assert_eq!(
        sample(&m, "docx_to_pdf_pool_instances", &["state=\"idle\""]),
        3.0
    );
    assert_eq!(
        sample(&m, "docx_to_pdf_instance_start_duration_seconds_count", &[]),
        3.0
    );
    assert_eq!(f.profiles_left(), 3);
}

#[tokio::test]
async fn concurrent_conversions_never_share_an_instance() {
    let f = Arc::new(Fixture::new());
    let pool = f.pool(2, WAIT, 16).await;
    let in_use = Arc::new(Mutex::new(HashSet::new()));
    let seen = Arc::new(Mutex::new(HashSet::new()));
    let mut tasks = Vec::new();
    for n in 0..8 {
        let (pool, f, in_use, seen) = (pool.clone(), f.clone(), in_use.clone(), seen.clone());
        tasks.push(tokio::spawn(async move {
            let mut lease = pool.acquire().await.unwrap();
            let pid = lease.pid();
            assert!(in_use.lock().unwrap().insert(pid), "{pid} lent twice");
            seen.lock().unwrap().insert(pid);
            let (input, output) = (f.input(&format!("slow-{n}")), f.output(&format!("{n}")));
            let answer = lease.convert(&input, &output).await.unwrap();
            assert!(matches!(answer, Answer::Converted { .. }), "{answer:?}");
            assert_eq!(
                std::fs::read_to_string(&output).unwrap().trim(),
                pid.to_string()
            );
            in_use.lock().unwrap().remove(&pid);
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(seen.lock().unwrap().len(), 2, "both instances served");
    assert_eq!(pool.counts().idle, 2);
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_pool_wait_duration_seconds_count",
            &["outcome=\"acquired\""]
        ),
        8.0
    );
}

#[tokio::test]
async fn a_lease_shows_as_busy_and_comes_back_idle() {
    let f = Fixture::new();
    let pool = f.pool(2, Duration::from_secs(1), 4).await;
    let lease = pool.acquire().await.unwrap();
    assert_eq!(
        pool.counts(),
        Counts {
            idle: 1,
            busy: 1,
            starting: 0
        }
    );
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_pool_instances",
            &["state=\"busy\""]
        ),
        1.0
    );
    let pid = lease.pid();
    drop(lease);
    assert_eq!(pool.counts().idle, 2);
    assert!(group_alive(pid), "a usable instance is given back alive");
}

#[tokio::test]
async fn a_killed_instance_is_replaced_and_the_conversion_succeeds_by_the_retry() {
    let f = Fixture::new();
    let pool = f.pool(1, WAIT, 4).await;
    let first = pool.acquire().await.unwrap().pid();
    // Not waited for: an idle instance is not reaped until it is lent, so on Linux its zombie still counts
    // as a group member.
    kill_group(first);

    let output = f.output("retried");
    let conversion = pool.convert(&f.input("doc"), &output).await.unwrap();
    assert!(conversion.retried, "the answer came from the retry");
    assert!(matches!(conversion.answer, Answer::Converted { .. }));
    let second: u32 = std::fs::read_to_string(&output)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(second, first, "the retry ran on another instance");

    let m = f.metrics();
    assert_eq!(
        sample(
            &m,
            "docx_to_pdf_instance_starts_total",
            &["reason=\"crash\""]
        ),
        1.0
    );
    assert_eq!(
        sample(
            &m,
            "docx_to_pdf_instance_starts_total",
            &["reason=\"initial\""]
        ),
        1.0
    );
    eventually("one profile left, the replacement's", || {
        f.profiles_left() == 1
    })
    .await;
}

#[tokio::test]
async fn a_conversion_that_does_not_meet_a_dead_instance_is_not_retried() {
    let f = Fixture::new();
    let pool = f.pool(1, Duration::from_secs(1), 4).await;
    let conversion = pool
        .convert(&f.input("doc"), &f.output("doc"))
        .await
        .unwrap();
    assert!(!conversion.retried);
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"crash\""]
        ),
        0.0
    );
}

#[tokio::test]
async fn past_the_wait_limit_the_borrower_is_refused() {
    let f = Fixture::new();
    let pool = f.pool(1, Duration::from_millis(150), 4).await;
    let held = pool.acquire().await.unwrap();
    let began = Instant::now();
    assert_eq!(pool.acquire().await.unwrap_err(), AcquireError::Timeout);
    assert!(began.elapsed() >= Duration::from_millis(150));
    assert!(began.elapsed() < Duration::from_secs(2));
    let m = f.metrics();
    assert_eq!(
        sample(
            &m,
            "docx_to_pdf_pool_wait_duration_seconds_count",
            &["outcome=\"rejected\""]
        ),
        1.0
    );
    drop(held);
    assert!(pool.acquire().await.is_ok(), "free again once given back");
}

#[tokio::test]
async fn a_waiter_gets_the_instance_given_back_within_the_wait_limit() {
    let f = Fixture::new();
    let pool = f.pool(1, Duration::from_secs(5), 4).await;
    let held = pool.acquire().await.unwrap();
    let pid = held.pid();
    let waiter = tokio::spawn({
        let pool = pool.clone();
        async move { pool.acquire().await.map(|lease| lease.pid()) }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(held);
    assert_eq!(waiter.await.unwrap().unwrap(), pid);
}

#[tokio::test]
async fn past_the_queue_limit_the_borrower_is_refused_at_once() {
    let f = Fixture::new();
    let pool = f.pool(1, Duration::from_secs(5), 1).await;
    let held = pool.acquire().await.unwrap();
    let waiter = tokio::spawn({
        let pool = pool.clone();
        async move { pool.acquire().await.map(|lease| lease.pid()) }
    });
    eventually("one borrower waits", || {
        pool.inner.waiting.load(Ordering::SeqCst) == 1
    })
    .await;
    let began = Instant::now();
    assert_eq!(pool.acquire().await.unwrap_err(), AcquireError::QueueFull);
    assert!(began.elapsed() < Duration::from_secs(1), "refused at once");
    drop(held);
    assert!(
        waiter.await.unwrap().is_ok(),
        "the one in line still gets it"
    );
    assert_eq!(pool.inner.waiting.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn no_queue_at_all_refuses_a_borrower_when_every_instance_is_lent() {
    let f = Fixture::new();
    let pool = f.pool(1, Duration::from_secs(5), 0).await;
    let _held = pool.acquire().await.unwrap();
    assert_eq!(pool.acquire().await.unwrap_err(), AcquireError::QueueFull);
}

#[tokio::test]
async fn readiness_ends_while_no_instance_is_alive_and_returns_with_the_replacement() {
    let f = Fixture::new();
    let pool = f.pool(1, WAIT, 4).await;
    assert!(f.readiness.is_ready());
    f.mark("slow-start");
    let first = pool.acquire().await.unwrap().pid();
    kill_group(first);

    let converting = tokio::spawn({
        let pool = pool.clone();
        let (input, output) = (f.input("doc"), f.output("doc"));
        async move { pool.convert(&input, &output).await }
    });
    eventually("not ready while the replacement starts", || {
        !f.readiness.is_ready()
    })
    .await;
    assert_eq!(pool.counts().starting, 1);
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_pool_instances",
            &["state=\"starting\""]
        ),
        1.0
    );
    let conversion = converting.await.unwrap().unwrap();
    assert!(conversion.retried);
    assert!(f.readiness.is_ready(), "ready again with the replacement");
}

#[tokio::test]
async fn a_retired_instance_is_killed_and_replaced_with_its_reason() {
    let f = Fixture::new();
    let pool = f.pool(1, WAIT, 4).await;
    let lease = pool.acquire().await.unwrap();
    let old = lease.pid();
    lease.retire(StartReason::Recycle);
    let new = pool.acquire().await.unwrap().pid();
    assert_ne!(new, old);
    // Eventually: a member forked as the signal landed is reaped by whoever adopted it, a moment later.
    eventually("the retired instance's group is gone", || !group_alive(old)).await;
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"recycle\""]
        ),
        1.0
    );
}

#[tokio::test]
async fn a_lease_dropped_mid_conversion_kills_and_replaces_its_instance_as_cancelled() {
    let f = Fixture::new();
    let pool = f.pool(1, WAIT, 4).await;
    let mut lease = pool.acquire().await.unwrap();
    let old = lease.pid();
    let (input, output) = (f.input("slow"), f.output("slow"));
    let abandoned = tokio::time::timeout(Duration::from_millis(50), lease.convert(&input, &output));
    assert!(abandoned.await.is_err(), "the conversion was cut off");
    drop(lease);
    let new = pool.acquire().await.unwrap().pid();
    assert_ne!(new, old);
    eventually("the abandoned instance's group is gone", || {
        !group_alive(old)
    })
    .await;
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"cancelled\""]
        ),
        1.0
    );
}

#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn a_failed_replacement_is_retried_and_counted_as_failed() {
    let f = Fixture::new();
    let pool = f.pool(1, WAIT, 4).await;
    f.mark("no-start");
    pool.acquire().await.unwrap().retire(StartReason::Recycle);
    eventually("a replacement start failed", || {
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"recycle\""],
        ) == 1.0
    })
    .await;
    assert!(!f.readiness.is_ready());
    f.unmark("no-start");
    let lease = pool.acquire().await.unwrap();
    drop(lease);
    assert!(f.readiness.is_ready());
    assert!(
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"failed\""]
        ) >= 1.0
    );
}

#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn a_failed_first_start_is_retried_and_the_boot_waits_for_a_full_pool() {
    let f = Fixture::new();
    f.mark("fail-once");
    let settings = PoolSettings {
        size: 2,
        wait_timeout: Duration::from_secs(1),
        max_waiting: 1,
        // A wait bound, not this test's subject: a start that ran past it would be one more `failed` start and
        // fail the count below for a reason the test does not set.
        start_timeout: WAIT,
    };
    let meter = f.telemetry.meter();
    let began = Instant::now();
    let booting = Pool::start(f.launcher(), settings, f.readiness.clone(), &meter);
    let watching = async {
        eventually("both first starts have ended", || {
            sample(
                &f.metrics(),
                "docx_to_pdf_instance_starts_total",
                &["reason=\"initial\""],
            ) == 2.0
        })
        .await;
        assert!(
            !f.readiness.is_ready(),
            "not ready while the pool is not full"
        );
    };
    let (started, ()) = tokio::join!(booting, watching);
    let booted_in = began.elapsed();
    let pool = started.expect("the boot retries the failed start");
    assert!(f.readiness.is_ready());
    assert_eq!(
        pool.counts(),
        Counts {
            idle: 2,
            busy: 0,
            starting: 0
        }
    );
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"failed\""]
        ),
        1.0,
        "exactly the start that took the marker failed; booted in {booted_in:?} (the successful starts are \
         docx_to_pdf_instance_start_duration_seconds_count):\n{}",
        f.metrics()
    );
    let slots: HashSet<usize> = lock(&pool.inner.state)
        .slots()
        .iter()
        .map(|&(slot, _)| slot)
        .collect();
    assert_eq!(slots, HashSet::from([0, 1]), "the retry keeps its slot");
}

#[tokio::test]
async fn the_boot_fails_when_an_instance_cannot_start_within_its_attempts() {
    let f = Fixture::new();
    f.mark("no-start");
    let started = Pool::start(
        f.launcher(),
        PoolSettings {
            size: 2,
            wait_timeout: Duration::from_secs(1),
            max_waiting: 1,
            start_timeout: WAIT,
        },
        f.readiness.clone(),
        &f.telemetry.meter(),
    )
    .await;
    assert!(matches!(started, Err(StartFailure::Start(_))));
    assert!(!f.readiness.is_ready());
    assert_eq!(f.profiles_left(), 0);
    let text = f.metrics();
    let starts = |reason: &str| {
        sample(
            &text,
            "docx_to_pdf_instance_starts_total",
            &[&format!("reason=\"{reason}\"")],
        )
    };
    assert_eq!(starts("initial"), 2.0);
    let retries = f64::from(INITIAL_START_ATTEMPTS - 1);
    assert!(
        (retries..=2.0 * retries).contains(&starts("failed")),
        "the slot that ran out tried {INITIAL_START_ATTEMPTS} times; the other stopped at most there"
    );
}

#[tokio::test]
async fn the_boot_fails_when_an_instance_is_not_ready_within_the_start_timeout() {
    let f = Fixture::new();
    f.mark("slow-start");
    let began = Instant::now();
    let started = Pool::start(
        f.launcher(),
        PoolSettings {
            size: 1,
            wait_timeout: Duration::from_secs(1),
            max_waiting: 1,
            start_timeout: Duration::from_millis(200),
        },
        f.readiness.clone(),
        &f.telemetry.meter(),
    )
    .await;
    assert!(matches!(started, Err(StartFailure::TimedOut(_))));
    // Three starts of 200 ms each, with the pauses of 0.5 s and 1 s between them.
    let took = began.elapsed();
    assert!(
        took >= Duration::from_millis(2100) && took < Duration::from_millis(3000),
        "{took:?}"
    );
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"failed\""]
        ),
        f64::from(INITIAL_START_ATTEMPTS - 1)
    );
    eventually("the timed-out instance's profile is deleted", || {
        f.profiles_left() == 0
    })
    .await;
}

#[tokio::test]
async fn shutdown_refuses_borrowers_and_stops_every_instance() {
    let f = Fixture::new();
    let pool = f.pool(2, Duration::from_secs(5), 4).await;
    let lent = pool.acquire().await.unwrap();
    let lent_pid = lent.pid();
    let waiter = tokio::spawn({
        let pool = pool.clone();
        async move {
            let _first = pool.acquire().await;
            pool.acquire().await.map(|_| ())
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    pool.shutdown(Duration::from_secs(5)).await;
    assert!(!f.readiness.is_ready());
    assert_eq!(waiter.await.unwrap().unwrap_err(), AcquireError::Closed);
    assert_eq!(pool.acquire().await.unwrap_err(), AcquireError::Closed);
    drop(lent);
    eventually("the lent instance is killed, not replaced", || {
        !group_alive(lent_pid)
    })
    .await;
    eventually("every profile is deleted", || f.profiles_left() == 0).await;
    assert_eq!(pool.counts(), Counts::default());
}

/// Taken by every test that makes an instance hold about 100 MB, so they run one at a time: on a shared CI runner
/// several at once starve each other, and a 100 MB start then passes the start timeout.
static HEAVY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// No recycling, so a test sees only the limit it sets.
fn no_limits() -> InstanceLimits {
    InstanceLimits {
        conversion_timeout: Duration::from_secs(60),
        max_conversions: 0,
        max_memory: 0,
        hard_memory: 0,
    }
}

/// A hard limit the stand-in's `bomb` (about 100 MB) passes and its idle shell (a few MB) never reaches.
const BOMB_LIMIT: u64 = 50 * 1024 * 1024;

/// When a read of `pid`'s RSS, every 2 ms on a thread of its own, first finds it above `limit`; `None` if the
/// process ends first or 20 s pass.
fn first_seen_above(pid: u32, limit: u64) -> std::thread::JoinHandle<Option<Instant>> {
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            match crate::process_metrics::resident_bytes_of(pid) {
                None => return None,
                Some(resident) if resident > limit => return Some(Instant::now()),
                Some(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        None
    })
}

/// An instance that passes the hard memory limit mid-conversion is killed within one poll, with
/// its whole group; the conversion's answer is `MemoryLimit`, not retried on another instance; the replacement
/// is counted with the reason `memory_limit` and the pool is full again.
#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn a_conversion_above_the_hard_memory_limit_is_killed_within_one_poll_and_replaced() {
    let _heavy = HEAVY.lock().await;
    let f = Fixture::new();
    let pool = f
        .pool_with(
            1,
            InstanceLimits {
                hard_memory: BOMB_LIMIT,
                ..no_limits()
            },
        )
        .await;
    let first = pool.acquire().await.unwrap().pid();
    let seen = first_seen_above(first, BOMB_LIMIT);

    let began = Instant::now();
    let error = pool
        .convert(&f.input("bomb"), &f.output("bomb"))
        .await
        .unwrap_err();
    let answered = Instant::now();
    assert!(
        matches!(error, PoolError::MemoryLimit { resident, limit }
            if resident > BOMB_LIMIT && limit == BOMB_LIMIT),
        "{error:?}"
    );
    assert!(
        answered - began < Duration::from_secs(10),
        "cut short, not run to its 30 s sleep: {:?}",
        answered - began
    );
    let seen = seen
        .join()
        .unwrap()
        .expect("the bomb's RSS was seen above the limit");
    let late = answered.saturating_duration_since(seen);
    assert!(
        late <= HARD_MEMORY_POLL + Duration::from_millis(200),
        "answered {late:?} after the RSS passed the limit; the poll is {HARD_MEMORY_POLL:?}"
    );

    eventually("the bomb's group is gone", || !group_alive(first)).await;
    eventually(
        "the replacement is counted with reason memory_limit",
        || {
            sample(
                &f.metrics(),
                "docx_to_pdf_instance_starts_total",
                &["reason=\"memory_limit\""],
            ) == 1.0
        },
    )
    .await;
    let m = f.metrics();
    for reason in ["crash", "timeout", "cancelled", "recycle"] {
        assert_eq!(
            sample(
                &m,
                "docx_to_pdf_instance_starts_total",
                &[&format!("reason=\"{reason}\"")]
            ),
            0.0,
            "{reason}: a memory kill is none of these, and it is not retried"
        );
    }
    eventually("the pool is full again", || {
        pool.counts()
            == Counts {
                idle: 1,
                busy: 0,
                starting: 0,
            }
    })
    .await;
    eventually("one profile left, the replacement's", || {
        f.profiles_left() == 1
    })
    .await;
    let next = f.convert_on(&pool, "next").await;
    assert_ne!(next, first, "the next conversion runs on the replacement");
}

/// A memory kill takes only its own instance; a conversion in flight on another finishes on its
/// instance, which stays in the pool.
#[tokio::test]
async fn a_memory_kill_leaves_the_other_conversions_in_flight_untouched() {
    let _heavy = HEAVY.lock().await;
    let f = Fixture::new();
    let pool = f
        .pool_with(
            2,
            InstanceLimits {
                hard_memory: BOMB_LIMIT,
                ..no_limits()
            },
        )
        .await;
    let long = tokio::spawn({
        let pool = pool.clone();
        let (input, output) = (f.input("long"), f.output("long"));
        async move {
            let conversion = pool.convert(&input, &output).await;
            (conversion, output)
        }
    });
    eventually("the long conversion has its instance", || {
        pool.counts().busy == 1
    })
    .await;

    let error = pool
        .convert(&f.input("bomb"), &f.output("bomb"))
        .await
        .unwrap_err();
    assert!(matches!(error, PoolError::MemoryLimit { .. }), "{error:?}");
    assert!(
        !long.is_finished(),
        "the long conversion is still in flight"
    );

    let (conversion, output) = long.await.unwrap();
    let conversion = conversion.unwrap();
    assert!(
        matches!(conversion.answer, Answer::Converted { .. }) && !conversion.retried,
        "{conversion:?}"
    );
    let long_pid: u32 = std::fs::read_to_string(output)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(group_alive(long_pid), "its instance was not touched");
    eventually("the pool is full again", || pool.counts().idle == 2).await;
    let pids: HashSet<u32> = {
        let mut lent = Vec::new();
        for _ in 0..2 {
            lent.push(pool.acquire().await.unwrap());
        }
        lent.iter().map(Lease::pid).collect()
    };
    assert!(
        pids.contains(&long_pid),
        "the long conversion's instance is still in the pool: {pids:?}"
    );
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"memory_limit\""]
        ),
        1.0
    );
}

/// With no ceiling, the hard limit is the ceiling: an instance given back above it is recycled rather than
/// killed at the start of its next conversion.
#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn with_no_ceiling_an_instance_given_back_above_the_hard_limit_is_recycled() {
    let f = Fixture::new();
    // Any live process is above one byte; the limit is read only between conversions here, since a lease
    // converts nothing.
    let pool = f
        .pool_with(
            1,
            InstanceLimits {
                hard_memory: 1,
                ..no_limits()
            },
        )
        .await;
    let first = pool.acquire().await.unwrap().pid();
    eventually("the recycle is counted", || {
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"recycle\""],
        ) == 1.0
    })
    .await;
    eventually("a replacement is idle", || pool.counts().idle == 1).await;
    assert_ne!(pool.acquire().await.unwrap().pid(), first);
}

#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn a_conversion_past_the_timeout_kills_its_instance_group_and_is_replaced_as_timeout() {
    let f = Fixture::new();
    let timeout = Duration::from_millis(300);
    let pool = f
        .pool_with(
            1,
            InstanceLimits {
                conversion_timeout: timeout,
                ..no_limits()
            },
        )
        .await;
    let first = pool.acquire().await.unwrap().pid();

    let began = Instant::now();
    let error = pool
        .convert(&f.input("hang"), &f.output("hang"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, PoolError::Timeout(t) if t == timeout),
        "{error:?}"
    );
    assert!(began.elapsed() >= timeout);
    assert!(
        began.elapsed() < timeout + Duration::from_secs(1),
        "{:?}",
        began.elapsed()
    );

    // The whole group goes, the `sleep 30` child included, and the profile with it: a killed instance leaves its
    // profile behind, so the pool deletes it.
    eventually("the timed-out instance's group is gone", || {
        !group_alive(first)
    })
    .await;
    eventually("the replacement is counted with reason timeout", || {
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"timeout\""],
        ) == 1.0
    })
    .await;
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"cancelled\""]
        ),
        0.0,
        "a timeout is not a cancellation"
    );
    eventually("one profile left, the replacement's", || {
        f.profiles_left() == 1
    })
    .await;

    let next = f.convert_on(&pool, "next").await;
    assert_ne!(next, first, "the next conversion runs on a fresh instance");
}

#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn an_instance_is_recycled_after_its_maximum_conversions_and_no_conversion_fails() {
    let f = Fixture::new();
    let pool = f
        .pool_with(
            1,
            InstanceLimits {
                max_conversions: 2,
                ..no_limits()
            },
        )
        .await;
    let mut pids = Vec::new();
    for n in 0..5 {
        pids.push(f.convert_on(&pool, &format!("doc-{n}")).await);
    }
    assert_eq!(pids[0], pids[1]);
    assert_ne!(pids[1], pids[2], "recycled after two conversions");
    assert_eq!(pids[2], pids[3]);
    assert_ne!(pids[3], pids[4], "and again after two more");
    eventually("two recycles counted", || {
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"recycle\""],
        ) == 2.0
    })
    .await;
    eventually("the recycled instances' groups are gone", || {
        !group_alive(pids[0]) && !group_alive(pids[2])
    })
    .await;
    eventually("one profile left", || f.profiles_left() == 1).await;
}

#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn an_instance_above_the_memory_ceiling_is_recycled_after_its_conversion() {
    let f = Fixture::new();
    // Any live process is above one byte.
    let pool = f
        .pool_with(
            1,
            InstanceLimits {
                max_memory: 1,
                ..no_limits()
            },
        )
        .await;
    let first = f.convert_on(&pool, "a").await;
    let second = f.convert_on(&pool, "b").await;
    assert_ne!(first, second, "recycled between the two conversions");
    eventually("the recycles counted", || {
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"recycle\""],
        ) == 2.0
    })
    .await;
}

#[tokio::test]
async fn zero_limits_never_recycle() {
    let f = Fixture::new();
    let pool = f.pool_with(1, no_limits()).await;
    let first = f.convert_on(&pool, "a").await;
    for n in 0..3 {
        assert_eq!(f.convert_on(&pool, &format!("b-{n}")).await, first);
    }
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_instance_starts_total",
            &["reason=\"recycle\""]
        ),
        0.0
    );
}

/// The `instance` labels of `docx_to_pdf_instance_memory_usage_bytes`, sorted.
fn memory_series(text: &str) -> Vec<String> {
    let mut labels: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("docx_to_pdf_instance_memory_usage_bytes{"))
        .filter_map(|l| l.split("instance=\"").nth(1))
        .filter_map(|l| l.split('"').next())
        .map(str::to_owned)
        .collect();
    labels.sort();
    labels
}

#[tokio::test]
async fn each_instance_reports_its_resident_memory_by_its_pool_slot() {
    let f = Fixture::new();
    let pool = f.pool_with(2, no_limits()).await;
    let lease = pool.acquire().await.unwrap();
    let slot = lease.slot();
    assert!(slot < 2, "a slot is 0..size-1: {slot}");
    let m = f.metrics();
    let rss = sample(
        &m,
        "docx_to_pdf_instance_memory_usage_bytes",
        &[&format!("instance=\"{slot}\"")],
    );
    assert!(rss > 0.0, "a lent instance is reported too:\n{m}");
    assert_eq!(memory_series(&m), ["0", "1"], "one series per slot:\n{m}");
    drop(lease);
}

/// A replacement takes its predecessor's slot, so the gauge keeps one series per slot across recycles
/// and an alert on one slot's growth follows it: the pid changes, the label does not.
#[tokio::test]
async fn a_replacement_keeps_its_slot_in_the_memory_gauge() {
    let f = Fixture::new();
    let pool = f
        .pool_with(
            2,
            InstanceLimits {
                max_conversions: 1,
                ..no_limits()
            },
        )
        .await;
    assert_eq!(memory_series(&f.metrics()), ["0", "1"]);
    let lease = pool.acquire().await.unwrap();
    let (slot, pid) = (lease.slot(), lease.pid());
    lease.retire(StartReason::Recycle);
    eventually("the replacement is idle", || {
        let counts = pool.counts();
        counts.idle == 2 && counts.starting == 0
    })
    .await;
    let m = f.metrics();
    assert_eq!(memory_series(&m), ["0", "1"], "the same series after:\n{m}");
    let leases = [pool.acquire().await.unwrap(), pool.acquire().await.unwrap()];
    let replacement = leases.iter().find(|l| l.slot() == slot).unwrap();
    assert_ne!(replacement.pid(), pid, "a new instance in slot {slot}");
}

/// The probe's reservation drains the pool (clients refused, not ready), waits for the
/// conversion under way, has every instance to itself, and gives the pool back whole.
#[tokio::test]
async fn a_reservation_waits_for_the_conversions_under_way_and_refuses_everyone_else() {
    let f = Fixture::new();
    let pool = f.pool(2, Duration::from_secs(5), 4).await;
    assert!(f.readiness.is_ready());
    let under_way = tokio::spawn({
        let pool = pool.clone();
        let (input, output) = (f.input("slow"), f.output("slow"));
        async move { pool.convert(&input, &output).await.map(|c| c.retried) }
    });
    eventually("a conversion is under way", || pool.counts().busy == 1).await;

    let reserving = tokio::spawn({
        let pool = pool.clone();
        async move { pool.reserve().await }
    });
    eventually("the pool is drained", || !f.readiness.is_ready()).await;
    assert_eq!(pool.acquire().await.unwrap_err(), AcquireError::Reserved);
    let reservation = reserving.await.unwrap().unwrap();
    assert!(
        under_way.await.unwrap().is_ok(),
        "the conversion under way ends normally"
    );
    assert_eq!(
        pool.counts().idle,
        2,
        "reserved once every instance is idle"
    );
    assert!(matches!(pool.reserve().await, Err(ReserveError::Busy)));

    // The probe converts on every instance at once; clients are still refused.
    let (a, b) = futures_util::future::join(
        reservation.convert(&f.input("slow-a"), &f.output("slow-a")),
        reservation.convert(&f.input("slow-b"), &f.output("slow-b")),
    )
    .await;
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(pool.acquire().await.unwrap_err(), AcquireError::Reserved);
    assert!(!f.readiness.is_ready());

    drop(reservation);
    assert!(f.readiness.is_ready(), "ready again once given back");
    assert_eq!(pool.counts().idle, 2, "the pool is whole");
    assert!(pool.acquire().await.is_ok());
    assert_eq!(
        sample(
            &f.metrics(),
            "docx_to_pdf_pool_wait_duration_seconds_count",
            &["outcome=\"rejected\""]
        ),
        2.0
    );
}

/// The count of `docx_to_pdf.instance.memory.peak` observations up to `le` bytes, and in all.
#[cfg(target_os = "linux")]
fn peaks(metrics: &str, le: &str) -> (f64, f64) {
    (
        sample(
            metrics,
            "docx_to_pdf_instance_memory_peak_bytes_bucket",
            &[&format!("le=\"{le}\"")],
        ),
        sample(metrics, "docx_to_pdf_instance_memory_peak_bytes_count", &[]),
    )
}

/// Each conversion records its instance's peak resident memory during that conversion, so the memory model can
/// come from production, not only from the bench.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_conversion_records_its_instances_peak_memory() {
    let _heavy = HEAVY.lock().await;
    let f = Fixture::new();
    let pool = f.pool_with(1, no_limits()).await;
    f.convert_on(&pool, "small").await;
    assert_eq!(
        peaks(&f.metrics(), "67108864"),
        (1.0, 1.0),
        "{}",
        f.metrics()
    );
    f.convert_on(&pool, "big").await;
    assert_eq!(
        peaks(&f.metrics(), "67108864"),
        (1.0, 2.0),
        "{}",
        f.metrics()
    );
}

/// A conversion the hard memory limit kills still records its peak, read before the kill: the
/// heaviest documents are in the histogram, not missing from it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_conversion_killed_above_the_hard_memory_limit_records_its_peak() {
    let _heavy = HEAVY.lock().await;
    let f = Fixture::new();
    let pool = f
        .pool_with(
            1,
            InstanceLimits {
                hard_memory: BOMB_LIMIT,
                ..no_limits()
            },
        )
        .await;
    let error = pool
        .convert(&f.input("bomb"), &f.output("bomb"))
        .await
        .unwrap_err();
    let PoolError::MemoryLimit { resident, .. } = error else {
        panic!("{error:?}");
    };
    // The kill can come while the bomb is still filling its 100 MB, so the peak is bounded by the RSS that
    // tripped the limit, not by a bucket.
    let m = f.metrics();
    assert_eq!(peaks(&m, "67108864").1, 1.0, "one peak: {m}");
    let peak = sample(&m, "docx_to_pdf_instance_memory_peak_bytes_sum", &[]);
    assert!(
        peak >= crate::num::ToF64::to_f64(resident) && resident > BOMB_LIMIT,
        "the peak {peak} is at least the RSS {resident} that passed the limit"
    );
}

/// A conversion cut short by the timeout still records its peak, read before its instance is
/// killed. The instance already holds 100 MB when the conversion begins (`hold`, given no timeout), so the
/// timed-out conversion's peak is above 64 MiB however slowly the machine runs.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_conversion_past_the_timeout_records_its_peak() {
    let _heavy = HEAVY.lock().await;
    let f = Fixture::new();
    let pool = f
        .pool_with(
            1,
            InstanceLimits {
                conversion_timeout: Duration::from_millis(300),
                ..no_limits()
            },
        )
        .await;
    let holding = hold(&f, &pool).await;
    assert_eq!(
        peaks(&f.metrics(), "67108864"),
        (0.0, 1.0),
        "{}",
        f.metrics()
    );

    let error = pool
        .convert(&f.input("hang"), &f.output("hang"))
        .await
        .unwrap_err();
    assert!(matches!(error, PoolError::Timeout(_)), "{error:?}");
    assert_eq!(
        peaks(&f.metrics(), "67108864"),
        (0.0, 2.0),
        "the timed-out conversion on {holding} recorded one more peak, above 64 MiB: {}",
        f.metrics()
    );
}

/// A conversion whose request is dropped before the answer records its peak when its lease is
/// dropped, before the instance is killed; once, however the lease ends. The instance holds 100 MB first.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_conversion_dropped_before_its_answer_records_its_peak_once() {
    let _heavy = HEAVY.lock().await;
    let f = Fixture::new();
    let pool = f.pool_with(1, no_limits()).await;
    let holding = hold(&f, &pool).await;
    let mut lease = pool.acquire().await.unwrap();
    assert_eq!(lease.pid(), holding);
    let (input, output) = (f.input("hang"), f.output("hang"));
    let abandoned =
        tokio::time::timeout(Duration::from_millis(300), lease.convert(&input, &output));
    assert!(abandoned.await.is_err(), "the conversion was cut off");
    assert_eq!(
        peaks(&f.metrics(), "67108864"),
        (0.0, 1.0),
        "{}",
        f.metrics()
    );
    drop(lease);
    assert_eq!(
        peaks(&f.metrics(), "67108864"),
        (0.0, 2.0),
        "one more peak, above 64 MiB: {}",
        f.metrics()
    );
}

/// Makes `pool`'s only instance hold 100 MB (the stand-in's `hold`) through a lease, which has no conversion
/// timeout, so a slow machine only makes it slower; gives the instance back and returns its pid.
#[cfg(target_os = "linux")]
async fn hold(f: &Fixture, pool: &Pool) -> u32 {
    let mut lease = pool.acquire().await.unwrap();
    let pid = lease.pid();
    let answer = lease.convert(&f.input("hold"), &f.output("hold")).await;
    assert!(matches!(answer, Ok(Answer::Converted { .. })), "{answer:?}");
    drop(lease);
    eventually("the holding instance is idle again", || {
        pool.counts().idle == 1
    })
    .await;
    assert_eq!(
        pool.acquire().await.unwrap().pid(),
        pid,
        "the holding instance was given back, not replaced"
    );
    pid
}

/// The peak is the conversion's own: memory the instance held before it (here, while starting) is not
/// counted, because the high-water mark is reset when a conversion begins.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_peak_does_not_carry_memory_held_before_the_conversion() {
    let _heavy = HEAVY.lock().await;
    let f = Fixture::new();
    f.mark("big-start");
    let pool = f.pool_with(1, no_limits()).await;
    f.convert_on(&pool, "small").await;
    assert_eq!(
        peaks(&f.metrics(), "67108864"),
        (1.0, 1.0),
        "{}",
        f.metrics()
    );
}
