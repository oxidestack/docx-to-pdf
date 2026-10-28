//! What every instrument of the service's metrics does on each way a conversion ends, read both where
//! `/metrics` reads it (the Prometheus registry) and where OTLP does (a periodic reader, here over an
//! in-memory exporter); and one trace holding the server span and every span of a conversion.
//!
//! Each scenario takes a reading before and after, and asserts the change of every cumulative series (every
//! counter and every histogram's count, the service's own and HTTP's) exactly: what it names changed by that
//! much, nothing else changed. The gauges are asserted by their value afterwards. The pool is a stand-in
//! (`sh` speaking the instance protocol, as in `tests/conversion.rs`), so every path can be provoked anywhere.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::num::ToF64;
use docx_to_pdf::pool::{InstanceLimits, Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use docx_to_pdf::upload::Upload;
use http_body_util::BodyExt;
use opentelemetry::trace::{SpanId, SpanKind, Status};
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;

/// The stand-in instance. By the document's content: `DIE` kills the instance mid-conversion, `SLOW` waits
/// 30 s; anything else gets a small PDF and `ok` with a load of 3 ms and an export of 4 ms.
fn script(markers: &Path) -> String {
    format!(
        r#"
        printf 'ready\t1\t%s\n' "$$"
        tab="$(printf '\t')"
        while IFS="$tab" read -r cmd input output; do
            : > '{markers}/started'
            case "$(cat "$input")" in
                *DIE*) kill -9 $$ ;;
                *SLOW*) sleep 30; printf 'ok\t1\t1\n' ;;
                *)
                    sleep 0.05
                    printf '%%PDF-1.4\n%% pid=%s\n%%%%EOF\n' "$$" > "$output"
                    printf 'ok\t3\t4\n' ;;
            esac
        done
        "#,
        markers = markers.display()
    )
}

/// How an instrument's points are read: a cumulative series (a counter's value, a histogram's count) whose
/// change a scenario asserts, or a gauge-like value asserted as it stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Counter,
    Histogram,
    Gauge,
}

/// Every instrument the service exports: its OpenTelemetry name, its Prometheus series name, its kind.
const INSTRUMENTS: &[(&str, &str, Kind)] = &[
    (
        "http.server.request.duration",
        "http_server_request_duration_seconds",
        Kind::Histogram,
    ),
    (
        "http.server.active_requests",
        "http_server_active_requests",
        Kind::Gauge,
    ),
    (
        "docx_to_pdf.conversions",
        "docx_to_pdf_conversions_total",
        Kind::Counter,
    ),
    (
        "docx_to_pdf.conversion.duration",
        "docx_to_pdf_conversion_duration_seconds",
        Kind::Histogram,
    ),
    (
        "docx_to_pdf.pool.wait.duration",
        "docx_to_pdf_pool_wait_duration_seconds",
        Kind::Histogram,
    ),
    (
        "docx_to_pdf.document.size",
        "docx_to_pdf_document_size_bytes",
        Kind::Histogram,
    ),
    (
        "docx_to_pdf.pdf.size",
        "docx_to_pdf_pdf_size_bytes",
        Kind::Histogram,
    ),
    (
        "docx_to_pdf.pool.instances",
        "docx_to_pdf_pool_instances",
        Kind::Gauge,
    ),
    (
        "docx_to_pdf.pool.size",
        "docx_to_pdf_pool_size",
        Kind::Gauge,
    ),
    (
        "docx_to_pdf.instance.starts",
        "docx_to_pdf_instance_starts_total",
        Kind::Counter,
    ),
    (
        "docx_to_pdf.instance.start.duration",
        "docx_to_pdf_instance_start_duration_seconds",
        Kind::Histogram,
    ),
    (
        "docx_to_pdf.instance.memory.usage",
        "docx_to_pdf_instance_memory_usage_bytes",
        Kind::Gauge,
    ),
    (
        "docx_to_pdf.instance.memory.peak",
        "docx_to_pdf_instance_memory_peak_bytes",
        Kind::Histogram,
    ),
    (
        "process.cpu.time",
        "process_cpu_time_seconds_total",
        Kind::Gauge,
    ),
    (
        "process.memory.usage",
        "process_memory_usage_bytes",
        Kind::Gauge,
    ),
    (
        "process.unix.file_descriptor.count",
        "process_unix_file_descriptor_count",
        Kind::Gauge,
    ),
];

/// One reading: `name{key=value,…}` (OpenTelemetry name, attribute keys with `_` for `.` as Prometheus has
/// them, sorted) to the point's value, or a histogram's count.
type Points = BTreeMap<String, f64>;

fn key(name: &str, mut labels: Vec<(String, String)>) -> String {
    labels.sort();
    let labels: Vec<String> = labels
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    format!("{name}{{{}}}", labels.join(","))
}

fn kind_of(name: &str) -> Kind {
    INSTRUMENTS
        .iter()
        .find(|(otel, _, _)| *otel == name)
        .map_or_else(
            || panic!("{name} is not an instrument the service exports"),
            |(_, _, kind)| *kind,
        )
}

/// The `/metrics` exposition read into points: each instrument's series (a histogram's `_count`), the
/// Prometheus exporter's own `otel_scope_*` labels left out.
fn from_prometheus(text: &str) -> Points {
    let mut points = Points::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let (series, value) = line.rsplit_once(' ').unwrap();
        let (name, labels) = match series.split_once('{') {
            Some((name, labels)) => (name, labels.trim_end_matches('}')),
            None => (series, ""),
        };
        let Some((otel, _, _)) = INSTRUMENTS.iter().find(|(_, prom, kind)| match kind {
            Kind::Histogram => name == format!("{prom}_count"),
            _ => name == *prom,
        }) else {
            continue;
        };
        let labels = labels
            .split("\",")
            .filter(|l| !l.is_empty())
            .map(|l| {
                let (k, v) = l.split_once('=').unwrap();
                (k.to_owned(), v.trim_matches('"').to_owned())
            })
            .filter(|(k, _)| !k.starts_with("otel_scope"))
            .collect();
        points.insert(key(otel, labels), value.parse().unwrap());
    }
    points
}

fn labels<'a>(
    attributes: impl Iterator<Item = &'a opentelemetry::KeyValue>,
) -> Vec<(String, String)> {
    attributes
        .map(|kv| (kv.key.as_str().replace('.', "_"), kv.value.to_string()))
        .collect()
}

/// The latest OTLP export read into points.
fn from_otlp(exported: &ResourceMetrics) -> Points {
    let mut points = Points::new();
    for scope in exported.scope_metrics() {
        for metric in scope.metrics() {
            let name = metric.name();
            kind_of(name);
            let values = match metric.data() {
                AggregatedMetrics::F64(data) => values(data),
                AggregatedMetrics::U64(data) => values(data),
                AggregatedMetrics::I64(data) => values(data),
            };
            for (labels, value) in values {
                points.insert(key(name, labels), value);
            }
        }
    }
    points
}

/// A point's value as `f64`, whatever the instrument's value type; the values here convert exactly.
trait Value: Copy {
    fn as_f64(self) -> f64;
}

impl Value for f64 {
    fn as_f64(self) -> f64 {
        self
    }
}

impl Value for u64 {
    fn as_f64(self) -> f64 {
        self.to_f64()
    }
}

impl Value for i64 {
    fn as_f64(self) -> f64 {
        self.to_f64()
    }
}

/// A metric's points: their labels and their value, a histogram's count.
fn values<T: Value>(data: &MetricData<T>) -> Vec<(Vec<(String, String)>, f64)> {
    match data {
        MetricData::Gauge(g) => g
            .data_points()
            .map(|p| (labels(p.attributes()), p.value().as_f64()))
            .collect(),
        MetricData::Sum(s) => s
            .data_points()
            .map(|p| (labels(p.attributes()), p.value().as_f64()))
            .collect(),
        MetricData::Histogram(h) => h
            .data_points()
            .map(|p| (labels(p.attributes()), p.count().to_f64()))
            .collect(),
        MetricData::ExponentialHistogram(_) => panic!("no exponential histograms here"),
    }
}

/// The service on a stand-in pool, its metrics read by Prometheus and by a periodic (OTLP-like) reader, its
/// spans captured in memory, its subscriber this thread's.
struct Service {
    dir: tempfile::TempDir,
    app: Router,
    telemetry: Telemetry,
    pool: Pool,
    exported: InMemoryMetricExporter,
    spans: InMemorySpanExporter,
    _guard: tracing::subscriber::DefaultGuard,
}

impl Service {
    async fn start(size: usize, limits: InstanceLimits) -> Self {
        Self::start_with(size, limits, Duration::from_secs(10)).await
    }

    async fn start_with(size: usize, limits: InstanceLimits, wait_timeout: Duration) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["profiles", "markers", "spool"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let exported = InMemoryMetricExporter::default();
        let spans = InMemorySpanExporter::default();
        let telemetry = TelemetryBuilder::new()
            .with_otlp_from(|_| None)
            .with_metric_reader(PeriodicReader::builder(exported.clone()).build())
            .with_simple_span_exporter(spans.clone())
            .build()
            .unwrap();
        let guard = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(telemetry.layers()),
        );
        let readiness = Readiness::new();
        let launcher = Launcher::with_args(
            "/bin/sh",
            [
                "-c".to_string(),
                script(&dir.path().join("markers")),
                "sh".to_string(),
            ],
            dir.path().join("profiles"),
        );
        let pool = Pool::start_with_limits(
            launcher,
            PoolSettings {
                size,
                wait_timeout,
                max_waiting: 8,
                start_timeout: Duration::from_secs(10),
            },
            limits,
            readiness.clone(),
            &telemetry.meter(),
        )
        .await
        .unwrap();
        let mut upload = Upload::new(1024 * 1024);
        upload.spool_root = dir.path().join("spool");
        let state = AppState::new(readiness, &telemetry).with_upload(upload);
        state.set_pool(pool.clone());
        Self {
            app: router(state),
            dir,
            telemetry,
            pool,
            exported,
            spans,
            _guard: guard,
        }
    }

    fn request(document: &[u8]) -> Request<Body> {
        let mut body = b"--B0017\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a.docx\"\r\n\r\n".to_vec();
        body.extend_from_slice(document);
        body.extend_from_slice(b"\r\n--B0017--\r\n");
        Request::builder()
            .method(Method::POST)
            .uri("/pdf")
            .header("content-type", "multipart/form-data; boundary=B0017")
            .body(Body::from(body))
            .unwrap()
    }

    async fn post(&self, document: &[u8]) -> StatusCode {
        let response = self
            .app
            .clone()
            .oneshot(Self::request(document))
            .await
            .unwrap();
        let status = response.status();
        response.into_body().collect().await.unwrap();
        status
    }

    /// Both readings at once; they must hold the same series.
    ///
    /// An observable instrument's callback runs once per collection but records into every reader's
    /// aggregation, and each reader reports what was recorded since its own last collection, so a reader's
    /// collection can carry a point the other reader's callback observed before it (the OpenTelemetry SDK's
    /// behaviour). While the memory gauge was labelled by pid, that showed a killed instance's
    /// series once more after its replacement, and this read took a first round of both collections. Labelled
    /// by pool slot, the replacement's fresh point overwrites its slot's old one, so a settled
    /// read needs no first round; only a collection taken while a slot's replacement is still starting can
    /// show that slot's last reading from before the kill, once.
    fn read(&self) -> Points {
        let prometheus = from_prometheus(&self.telemetry.prometheus_text());
        self.telemetry.force_flush();
        let exports = self.exported.get_finished_metrics().unwrap();
        self.exported.reset();
        let otlp = from_otlp(exports.last().expect("an OTLP export"));
        let series = |p: &Points| p.keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            series(&prometheus),
            series(&otlp),
            "/metrics and OTLP hold the same series"
        );
        for (key, value) in &prometheus {
            let name = key.split('{').next().unwrap();
            if kind_of(name) != Kind::Gauge {
                assert_eq!(
                    otlp[key], *value,
                    "{key}: /metrics and OTLP agree\n{prometheus:#?}\n{otlp:#?}"
                );
            }
        }
        otlp
    }

    /// Reads until every instance is idle again (a replacement may still be starting).
    async fn read_settled(&self) -> Points {
        let size = self.pool.settings().size;
        for _ in 0..400 {
            let counts = self.pool.counts();
            if counts.idle == size && counts.busy == 0 && counts.starting == 0 {
                return self.read();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the pool never settled: {:?}", self.pool.counts());
    }

    fn started(&self) -> PathBuf {
        self.dir.path().join("markers/started")
    }
}

/// How every cumulative series changed from `before` to `after`; a series that did not change is left out.
fn changes(before: &Points, after: &Points) -> BTreeMap<String, f64> {
    after
        .iter()
        .filter(|(key, _)| kind_of(key.split('{').next().unwrap()) != Kind::Gauge)
        .map(|(key, value)| (key.clone(), value - before.get(key).copied().unwrap_or(0.0)))
        .filter(|(_, change)| *change != 0.0)
        .collect()
}

fn expect(changes: &[(&str, f64)]) -> BTreeMap<String, f64> {
    changes.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

/// `changes` plus `conversions` observations of the instance peak-memory histogram, one per conversion
/// whatever ended it, a timeout or a kill included, since those are the heaviest; only Linux records it
/// (`/proc/<pid>/status` `VmHWM`).
fn with_peaks(mut changes: BTreeMap<String, f64>, conversions: f64) -> BTreeMap<String, f64> {
    if cfg!(target_os = "linux") {
        changes.insert(
            "docx_to_pdf.instance.memory.peak{}".to_string(),
            conversions,
        );
    }
    changes
}

/// Every gauge of the pool and the service process after a scenario, on a pool of `size` all idle.
fn assert_gauges(after: &Points, size: usize) {
    let gauge = |key: &str| after.get(key).copied();
    assert_eq!(
        gauge("docx_to_pdf.pool.size{}"),
        Some(size.to_f64()),
        "{after:#?}"
    );
    assert_eq!(
        gauge("docx_to_pdf.pool.instances{state=idle}"),
        Some(size.to_f64()),
        "{after:#?}"
    );
    assert_eq!(gauge("docx_to_pdf.pool.instances{state=busy}"), Some(0.0));
    assert_eq!(
        gauge("docx_to_pdf.pool.instances{state=starting}"),
        Some(0.0)
    );
    assert_eq!(
        gauge("http.server.active_requests{http_request_method=POST,http_route=/pdf}")
            .unwrap_or(0.0),
        0.0,
        "no request in flight (no series before the first): {after:#?}"
    );
    let memory: Vec<_> = after
        .iter()
        .filter(|(k, _)| k.starts_with("docx_to_pdf.instance.memory.usage{instance="))
        .collect();
    assert_eq!(memory.len(), size, "an RSS per instance: {after:#?}");
    assert!(memory.iter().all(|(_, v)| **v > 0.0), "{memory:?}");
    let expected: Vec<String> = (0..size).map(|slot| slot.to_string()).collect();
    assert_eq!(
        slots(after),
        expected,
        "labelled by pool slot, not by pid, so a replacement takes its slot's series"
    );
    for key in [
        "process.memory.usage{}",
        "process.unix.file_descriptor.count{}",
    ] {
        assert!(gauge(key).is_some_and(|v| v > 0.0), "{key}: {after:#?}");
    }
    for mode in ["user", "system"] {
        let key = format!("process.cpu.time{{cpu_mode={mode}}}");
        assert!(gauge(&key).is_some(), "{key}: {after:#?}");
    }
}

/// The pool slots the memory gauge names, sorted: `0..size` whatever the instances' pids.
fn slots(points: &Points) -> Vec<String> {
    let mut slots: Vec<String> = points
        .keys()
        .filter_map(|k| k.strip_prefix("docx_to_pdf.instance.memory.usage{instance="))
        .map(|k| k.trim_end_matches('}').to_owned())
        .collect();
    slots.sort();
    slots
}

const HTTP_200: &str = "http.server.request.duration{http_request_method=POST,http_response_status_code=200,http_route=/pdf}";

#[tokio::test]
async fn at_boot_each_instance_start_is_counted_initial_and_timed() {
    let service = Service::start(2, InstanceLimits::default()).await;
    let after = service.read_settled().await;
    assert_eq!(
        changes(&Points::new(), &after),
        expect(&[
            ("docx_to_pdf.instance.starts{reason=initial}", 2.0),
            ("docx_to_pdf.instance.start.duration{}", 2.0),
        ]),
        "{after:#?}"
    );
    assert_gauges(&after, 2);
}

#[tokio::test]
async fn a_conversion_changes_every_conversion_instrument_once() {
    let service = Service::start(1, InstanceLimits::default()).await;
    let before = service.read_settled().await;
    assert_eq!(service.post(b"{\\rtf1 hello}").await, StatusCode::OK);
    let after = service.read_settled().await;
    assert_eq!(
        changes(&before, &after),
        with_peaks(
            expect(&[
                (HTTP_200, 1.0),
                (
                    "docx_to_pdf.conversions{outcome=success,retried=false}",
                    1.0
                ),
                ("docx_to_pdf.conversion.duration{outcome=success}", 1.0),
                ("docx_to_pdf.pool.wait.duration{outcome=acquired}", 1.0),
                ("docx_to_pdf.document.size{format=rtf}", 1.0),
                ("docx_to_pdf.pdf.size{}", 1.0),
            ]),
            1.0,
        ),
    );
    assert_gauges(&after, 1);
    assert_eq!(slots(&before), slots(&after), "the same series");
}

#[tokio::test]
async fn a_refused_document_counts_unsupported_and_touches_no_instance() {
    let service = Service::start(1, InstanceLimits::default()).await;
    let before = service.read_settled().await;
    assert_eq!(
        service.post(b"plain text").await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    let after = service.read_settled().await;
    assert_eq!(
        changes(&before, &after),
        expect(&[
            (
                "http.server.request.duration{http_request_method=POST,http_response_status_code=415,http_route=/pdf}",
                1.0
            ),
            (
                "docx_to_pdf.conversions{outcome=unsupported,retried=false}",
                1.0
            ),
            ("docx_to_pdf.document.size{format=unsupported}", 1.0),
        ]),
    );
    assert_gauges(&after, 1);
}

#[tokio::test]
async fn a_timeout_counts_timeout_and_replaces_its_instance_as_timeout() {
    let service = Service::start(
        1,
        InstanceLimits {
            conversion_timeout: Duration::from_millis(500),
            ..InstanceLimits::default()
        },
    )
    .await;
    let before = service.read_settled().await;
    assert_eq!(
        service.post(b"{\\rtf1 SLOW}").await,
        StatusCode::GATEWAY_TIMEOUT
    );
    let after = service.read_settled().await;
    assert_eq!(
        changes(&before, &after),
        with_peaks(
            expect(&[
                (
                    "http.server.request.duration{error_type=504,http_request_method=POST,http_response_status_code=504,http_route=/pdf}",
                    1.0
                ),
                (
                    "docx_to_pdf.conversions{outcome=timeout,retried=false}",
                    1.0
                ),
                ("docx_to_pdf.conversion.duration{outcome=timeout}", 1.0),
                ("docx_to_pdf.pool.wait.duration{outcome=acquired}", 1.0),
                ("docx_to_pdf.document.size{format=rtf}", 1.0),
                ("docx_to_pdf.instance.starts{reason=timeout}", 1.0),
                ("docx_to_pdf.instance.start.duration{}", 1.0),
            ]),
            1.0,
        ),
    );
    assert_gauges(&after, 1);
    assert_eq!(
        slots(&before),
        slots(&after),
        "the replacement keeps its predecessor's series"
    );
}

/// The instance dies and so does the retry's: two acquisitions, two crashes, two replacements.
#[tokio::test]
async fn a_dead_instance_counts_dead_instance_retried_and_replaces_both_as_crash() {
    let service = Service::start(2, InstanceLimits::default()).await;
    let before = service.read_settled().await;
    assert_eq!(
        service.post(b"{\\rtf1 DIE}").await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let after = service.read_settled().await;
    assert_eq!(
        changes(&before, &after),
        expect(&[
            (
                "http.server.request.duration{error_type=500,http_request_method=POST,http_response_status_code=500,http_route=/pdf}",
                1.0
            ),
            (
                "docx_to_pdf.conversions{outcome=dead_instance,retried=true}",
                1.0
            ),
            (
                "docx_to_pdf.conversion.duration{outcome=dead_instance}",
                1.0
            ),
            ("docx_to_pdf.pool.wait.duration{outcome=acquired}", 2.0),
            ("docx_to_pdf.document.size{format=rtf}", 1.0),
            ("docx_to_pdf.instance.starts{reason=crash}", 2.0),
            ("docx_to_pdf.instance.start.duration{}", 2.0),
        ]),
    );
    assert_gauges(&after, 2);
    assert_eq!(
        slots(&before),
        slots(&after),
        "the replacement keeps its predecessor's series"
    );
}

/// The client goes away mid-conversion; the conversion is `cancelled`, its instance
/// killed and replaced as `cancelled`, the request `client_disconnected` with no status code.
#[tokio::test]
async fn a_disconnected_client_counts_cancelled_and_replaces_its_instance_as_cancelled() {
    let service = Service::start(1, InstanceLimits::default()).await;
    let before = service.read_settled().await;
    let request = tokio::spawn(
        service
            .app
            .clone()
            .oneshot(Service::request(b"{\\rtf1 SLOW}")),
    );
    for _ in 0..200 {
        if service.started().exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(service.started().exists(), "the instance never got it");
    request.abort();
    let _ = request.await;
    let after = service.read_settled().await;
    assert_eq!(
        changes(&before, &after),
        with_peaks(
            expect(&[
                (
                    "http.server.request.duration{error_type=client_disconnected,http_request_method=POST,http_route=/pdf}",
                    1.0
                ),
                (
                    "docx_to_pdf.conversions{outcome=cancelled,retried=false}",
                    1.0
                ),
                ("docx_to_pdf.conversion.duration{outcome=cancelled}", 1.0),
                ("docx_to_pdf.pool.wait.duration{outcome=acquired}", 1.0),
                ("docx_to_pdf.document.size{format=rtf}", 1.0),
                ("docx_to_pdf.instance.starts{reason=cancelled}", 1.0),
                ("docx_to_pdf.instance.start.duration{}", 1.0),
            ]),
            1.0,
        ),
    );
    assert_gauges(&after, 1);
    assert_eq!(
        slots(&before),
        slots(&after),
        "the replacement keeps its predecessor's series"
    );
}

/// A replacement between conversions: the instance is recycled after its one allowed conversion; the
/// conversion itself succeeded, the start counts `recycle`, and the memory gauge reports the new instance under the old one's slot.
#[tokio::test]
async fn a_recycled_instance_is_replaced_and_counted_recycle() {
    let service = Service::start(
        1,
        InstanceLimits {
            max_conversions: 1,
            ..InstanceLimits::default()
        },
    )
    .await;
    let before = service.read_settled().await;
    assert_eq!(service.post(b"{\\rtf1 hello}").await, StatusCode::OK);
    let after = service.read_settled().await;
    assert_eq!(
        changes(&before, &after),
        with_peaks(
            expect(&[
                (HTTP_200, 1.0),
                (
                    "docx_to_pdf.conversions{outcome=success,retried=false}",
                    1.0
                ),
                ("docx_to_pdf.conversion.duration{outcome=success}", 1.0),
                ("docx_to_pdf.pool.wait.duration{outcome=acquired}", 1.0),
                ("docx_to_pdf.document.size{format=rtf}", 1.0),
                ("docx_to_pdf.pdf.size{}", 1.0),
                ("docx_to_pdf.instance.starts{reason=recycle}", 1.0),
                ("docx_to_pdf.instance.start.duration{}", 1.0),
            ]),
            1.0,
        ),
    );
    assert_gauges(&after, 1);
    assert_eq!(
        slots(&before),
        slots(&after),
        "the replacement keeps its predecessor's series"
    );
}

/// No instance free within the wait: the wait counts `rejected`, the answer is 503 and no conversion is
/// counted (none took place).
#[tokio::test]
async fn a_rejected_wait_counts_rejected_and_no_conversion() {
    let service =
        Service::start_with(1, InstanceLimits::default(), Duration::from_millis(200)).await;
    let before = service.read_settled().await;
    let lease = service.pool.acquire().await.unwrap();
    let busy = service.read();
    assert_eq!(
        busy.get("docx_to_pdf.pool.instances{state=busy}"),
        Some(&1.0)
    );
    assert_eq!(
        busy.get("docx_to_pdf.pool.instances{state=idle}"),
        Some(&0.0)
    );
    assert_eq!(
        service.post(b"{\\rtf1 hello}").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(lease);
    let after = service.read_settled().await;
    assert_eq!(
        changes(&before, &after),
        expect(&[
            (
                "http.server.request.duration{error_type=503,http_request_method=POST,http_response_status_code=503,http_route=/pdf}",
                1.0
            ),
            // The test's own lease, then the request's wait.
            ("docx_to_pdf.pool.wait.duration{outcome=acquired}", 1.0),
            ("docx_to_pdf.pool.wait.duration{outcome=rejected}", 1.0),
            ("docx_to_pdf.document.size{format=rtf}", 1.0),
        ]),
    );
    assert_gauges(&after, 1);
}

/// One trace holds the server span and, under it, every span of the conversion.
#[tokio::test]
async fn one_trace_holds_the_server_span_and_every_conversion_span() {
    let service = Service::start(1, InstanceLimits::default()).await;
    service.telemetry.force_flush();
    service.spans.reset();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/pdf")
        .header("content-type", "multipart/form-data; boundary=B0017")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Service::request(b"{\\rtf1 hello}").into_body())
        .unwrap();
    let response = service.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().collect().await.unwrap();
    service.telemetry.force_flush();
    let spans: Vec<SpanData> = service.spans.get_finished_spans().unwrap();

    let servers: Vec<_> = spans
        .iter()
        .filter(|s| s.span_kind == SpanKind::Server)
        .collect();
    assert_eq!(servers.len(), 1, "{spans:#?}");
    let server = servers[0];
    assert_eq!(server.name, "POST /pdf");
    assert_eq!(
        server.span_context.trace_id().to_string(),
        "4bf92f3577b34da6a3ce929d0e0e4736",
        "the caller's trace"
    );
    assert_eq!(
        server.parent_span_id,
        SpanId::from_hex("00f067aa0ba902b7").unwrap()
    );
    for name in [
        "document.sniff",
        "pool.acquire",
        "backend.load",
        "backend.export",
    ] {
        let found: Vec<_> = spans.iter().filter(|s| s.name == name).collect();
        assert_eq!(found.len(), 1, "one {name}: {spans:#?}");
        let span = found[0];
        assert_eq!(
            span.span_context.trace_id(),
            server.span_context.trace_id(),
            "{name} in the request's trace"
        );
        assert_eq!(
            span.parent_span_id,
            server.span_context.span_id(),
            "{name} under the server span"
        );
        assert_eq!(span.status, Status::Unset, "{name}");
        assert!(span.start_time >= server.start_time, "{name}");
        assert!(span.end_time <= server.end_time, "{name}");
    }
    // Every span of the request's trace is one of those, and the trace has no other root.
    let in_trace: Vec<_> = spans
        .iter()
        .filter(|s| s.span_context.trace_id() == server.span_context.trace_id())
        .map(|s| s.name.to_string())
        .collect();
    assert_eq!(in_trace.len(), 5, "{in_trace:?}");
}

/// The size histograms reach 256 MiB, past the largest documents with images seen (100-150 MB), so a large
/// document lands in a bucket, not in `+Inf`.
#[tokio::test]
async fn the_size_histograms_reach_256_mib() {
    let service = Service::start(1, InstanceLimits::default()).await;
    assert_eq!(service.post(b"{\\rtf1 hello}").await, StatusCode::OK);
    let text = service.telemetry.prometheus_text();
    for name in [
        "docx_to_pdf_document_size_bytes_bucket",
        "docx_to_pdf_pdf_size_bytes_bucket",
    ] {
        for le in ["134217728", "268435456"] {
            assert!(
                text.lines()
                    .any(|l| l.starts_with(name) && l.contains(&format!("le=\"{le}\""))),
                "{name} has a bucket le={le}:\n{text}"
            );
        }
    }
}
