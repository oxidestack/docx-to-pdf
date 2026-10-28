//! Per-request telemetry: the caller's W3C trace context, one server span named `<METHOD> <route>`, and the
//! `http.server.request.duration` and `http.server.active_requests` instruments, labelled by route, never by raw
//! path. A request ends when its response body has been sent, failed or been dropped, not when its handler returns:
//! with the PDF streamed from its file, the body's transfer is most of a large conversion's answer.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::Router;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{Method, StatusCode, Version};
use axum::middleware::{self, Next};
use axum::response::Response;
use http_body::{Frame, SizeHint};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, UpDownCounter};
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry_http::HeaderExtractor;
use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, HTTP_REQUEST_METHOD, HTTP_RESPONSE_STATUS_CODE, HTTP_ROUTE,
};
use tokio::sync::watch;
use tracing::Instrument as _;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::telemetry::Telemetry;

/// What the instrumentation layer needs: the propagator and the HTTP instruments.
#[derive(Clone)]
pub struct HttpTelemetry {
    propagator: Arc<dyn TextMapPropagator + Send + Sync>,
    duration: Histogram<f64>,
    active: UpDownCounter<i64>,
}

impl HttpTelemetry {
    /// The instruments, from the service's meter.
    pub fn new(telemetry: &Telemetry) -> Self {
        let meter = telemetry.meter();
        Self {
            propagator: telemetry.propagator(),
            duration: meter
                .f64_histogram("http.server.request.duration")
                .with_unit("s")
                .with_description("Duration of HTTP server requests.")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
            active: meter
                .i64_up_down_counter("http.server.active_requests")
                .with_unit("{request}")
                .with_description("Number of active HTTP server requests.")
                .build(),
        }
    }
}

/// Wraps every route of `router` in the request instrumentation.
pub fn instrument<S>(router: Router<S>, http: HttpTelemetry) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(middleware::from_fn_with_state(http, track))
}

/// The semantic conventions' buckets, extended to five minutes: a conversion can take well over a minute
/// (the slowest measured, 63 s; the default timeout, 180 s).
const DURATION_BUCKETS: [f64; 18] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0, 30.0, 60.0,
    120.0, 300.0,
];

async fn track(State(http): State<HttpTelemetry>, request: Request, next: Next) -> Response {
    let method = method_attribute(request.method());
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned());
    let parent = http.propagator.extract(&HeaderExtractor(request.headers()));
    let grace = request.extensions().get::<GraceExpiry>().cloned();

    let name = match &route {
        Some(route) => format!("{method} {route}"),
        None => method.to_owned(),
    };
    let span = tracing::info_span!(
        "http.server.request",
        otel.name = %name,
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = method,
        http.route = route.as_deref(),
        http.response.status_code = Empty,
        error.type = Empty,
        url.path = request.uri().path(),
        url.scheme = "http",
        network.protocol.version = protocol_version(request.version()),
    );
    // A malformed or absent traceparent leaves a fresh root; nothing to report.
    let _ = span.set_parent(parent);

    let mut attributes = vec![KeyValue::new(HTTP_REQUEST_METHOD, method)];
    if let Some(route) = &route {
        attributes.push(KeyValue::new(HTTP_ROUTE, route.clone()));
    }
    let active = ActiveGuard::new(http.active.clone(), attributes.clone());
    let mut outcome = Outcome {
        duration: http.duration.clone(),
        span: span.clone(),
        attributes,
        started: Instant::now(),
        grace,
        status: None,
        recorded: false,
    };

    let response = next.run(request).instrument(span).await;

    outcome.status = Some(response.status());
    let cut = outcome
        .grace
        .as_ref()
        .map(|grace| Box::pin(grace.turned()) as GraceTurned);
    response.map(|body| {
        Body::new(TrackedBody {
            inner: body,
            outcome,
            cut,
            _active: active,
        })
    })
}

/// The response body, carrying the request's [`Outcome`] and its place in `http.server.active_requests` until
/// it has been sent to its end, has failed, or has been dropped. It enters the request's span while polled,
/// and forwards `size_hint` and `is_end_stream`, so `content-length` and hyper's handling of an empty body
/// are unchanged. When the shutdown grace period runs out mid-body, the body ends with an error at once, so
/// hyper closes the connection and the request is recorded as `shutdown_grace_exceeded` before the telemetry flush.
struct TrackedBody {
    inner: Body,
    outcome: Outcome,
    /// Resolves when the grace period runs out; `None` outside `server::serve`, or once the body was cut.
    cut: Option<GraceTurned>,
    _active: ActiveGuard,
}

type GraceTurned = Pin<Box<dyn Future<Output = ()> + Send>>;

impl HttpBody for TrackedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = &mut *self;
        if let Some(cut) = &mut this.cut
            && cut.as_mut().poll(cx).is_ready()
        {
            this.cut = None;
            this.outcome.fail(SHUTDOWN_GRACE_EXCEEDED);
            return Poll::Ready(Some(Err(axum::Error::new(
                "the shutdown grace period ran out while the body was being sent",
            ))));
        }
        if this.outcome.recorded {
            // Cut short: nothing more is sent.
            return Poll::Ready(None);
        }
        let polled = {
            let _entered = this.outcome.span.enter();
            Pin::new(&mut this.inner).poll_frame(cx)
        };
        match &polled {
            Poll::Ready(None) => this.outcome.finish(),
            Poll::Ready(Some(Ok(_))) if this.inner.is_end_stream() => this.outcome.finish(),
            Poll::Ready(Some(Err(_))) => this.outcome.fail(BODY_FAILED),
            _ => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for TrackedBody {
    fn drop(&mut self) {
        // hyper drops a body that says it is at its end without polling it (an empty body); that is a
        // body sent in full. Any other body dropped before its end lets `Outcome`'s drop decide.
        if self.inner.is_end_stream() {
            self.outcome.finish();
        }
    }
}

/// Records the request's duration and its span's outcome exactly once: at the end of the
/// response body, when the body fails, or when the request future or the body is dropped first.
struct Outcome {
    duration: Histogram<f64>,
    span: tracing::Span,
    attributes: Vec<KeyValue>,
    started: Instant,
    grace: Option<GraceExpiry>,
    /// The answer's status, once the handler has produced one and hyper is sending it.
    status: Option<StatusCode>,
    recorded: bool,
}

impl Outcome {
    /// The answer was sent in full: an error only if its status is one for a server span.
    fn finish(&mut self) {
        let error = self
            .status
            .filter(StatusCode::is_server_error)
            .map(|status| status.as_u16().to_string());
        self.record(self.status, error);
    }

    /// The answer's status was sent, but the request ended with `error`.
    fn fail(&mut self, error: &str) {
        self.record(self.status, Some(error.to_owned()));
    }

    /// `status` only when one was sent; `error` whenever the request ended with an error.
    fn record(&mut self, status: Option<StatusCode>, error: Option<String>) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        let mut attributes = std::mem::take(&mut self.attributes);
        if let Some(status) = status {
            let code = i64::from(status.as_u16());
            self.span.record("http.response.status_code", code);
            attributes.push(KeyValue::new(HTTP_RESPONSE_STATUS_CODE, code));
        }
        if let Some(error) = error {
            self.span.record("error.type", error.as_str());
            self.span.record("otel.status_code", "error");
            attributes.push(KeyValue::new(ERROR_TYPE, error));
        }
        self.duration
            .record(self.started.elapsed().as_secs_f64(), &attributes);
    }
}

impl Drop for Outcome {
    fn drop(&mut self) {
        // The answer was not sent to its end. Either the shutdown grace ran out (before an answer, the
        // cut-short layer answers 503 in its place; mid-body, the status already sent stays), or the client
        // went away and hyper dropped the request future or the body.
        if self.grace.as_ref().is_some_and(GraceExpiry::expired) {
            let status = self.status.or(Some(StatusCode::SERVICE_UNAVAILABLE));
            self.record(status, Some(SHUTDOWN_GRACE_EXCEEDED.to_owned()));
        } else {
            self.record(self.status, Some(CLIENT_DISCONNECTED.to_owned()));
        }
    }
}

/// `error.type` for a request whose client went away before its answer was sent to its end:
/// before the status (no `http.response.status_code`) or mid-body (the status that was sent).
pub const CLIENT_DISCONNECTED: &str = "client_disconnected";

/// `error.type` for a request still running when the shutdown grace period ended, cut short with 503.
pub const SHUTDOWN_GRACE_EXCEEDED: &str = "shutdown_grace_exceeded";

/// `error.type` for a response body that failed mid-stream (the PDF could not be read back): the semantic
/// conventions' fallback, as the service defines no value of its own for it.
pub const BODY_FAILED: &str = "_OTHER";

/// Whether the shutdown grace period has run out. The layer that cuts requests short (`server::serve`)
/// puts it in each request's extensions, so a request dropped for that reason is not taken for a
/// disconnect.
#[derive(Clone, Debug)]
pub struct GraceExpiry(watch::Receiver<bool>);

impl GraceExpiry {
    /// `expired` turns `true` when the grace period runs out.
    pub fn new(expired: watch::Receiver<bool>) -> Self {
        Self(expired)
    }

    fn expired(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once the grace period has run out; never, if serving ended without it running out.
    fn turned(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut expired = self.0.clone();
        async move {
            if expired.wait_for(|e| *e).await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Decrements `http.server.active_requests` when the request ends, also when it is cancelled.
struct ActiveGuard {
    counter: UpDownCounter<i64>,
    attributes: Vec<KeyValue>,
}

impl ActiveGuard {
    fn new(counter: UpDownCounter<i64>, attributes: Vec<KeyValue>) -> Self {
        counter.add(1, &attributes);
        Self {
            counter,
            attributes,
        }
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.counter.add(-1, &self.attributes);
    }
}

/// The method as the semantic conventions name it: a known method, or `_OTHER`.
fn method_attribute(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::CONNECT => "CONNECT",
        Method::OPTIONS => "OPTIONS",
        Method::TRACE => "TRACE",
        Method::PATCH => "PATCH",
        _ => "_OTHER",
    }
}

fn protocol_version(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "0.9",
        Version::HTTP_10 => "1.0",
        Version::HTTP_2 => "2",
        Version::HTTP_3 => "3",
        _ => "1.1",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::telemetry::TelemetryBuilder;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use http_body_util::BodyExt as _;
    use opentelemetry::Value;
    use opentelemetry::trace::{SpanKind, Status, TraceId};
    use opentelemetry_sdk::logs::InMemoryLogExporter;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
    use tower::ServiceExt;
    use tracing_subscriber::layer::SubscriberExt;

    pub struct Harness {
        pub telemetry: Telemetry,
        pub spans: InMemorySpanExporter,
        pub logs: InMemoryLogExporter,
        _guard: tracing::subscriber::DefaultGuard,
    }

    /// A telemetry with in-memory exporters, its layers installed as this thread's subscriber.
    pub fn harness() -> Harness {
        let spans = InMemorySpanExporter::default();
        let logs = InMemoryLogExporter::default();
        let telemetry = TelemetryBuilder::new()
            .with_simple_span_exporter(spans.clone())
            .with_simple_log_exporter(logs.clone())
            .build()
            .unwrap();
        let subscriber = tracing_subscriber::registry().with(telemetry.layers());
        let guard = tracing::subscriber::set_default(subscriber);
        Harness {
            telemetry,
            spans,
            logs,
            _guard: guard,
        }
    }

    fn app(h: &Harness) -> Router {
        let routes = Router::new()
            .route(
                "/documents/{id}",
                get(|| async {
                    tracing::info!("inside the request");
                    "ok"
                }),
            )
            .route(
                "/broken",
                get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            )
            .route("/hang/{id}", get(std::future::pending::<&'static str>));
        instrument(routes, HttpTelemetry::new(&h.telemetry))
    }

    /// Sends `request` and reads its answer to the end, as hyper does: the request ends with its body.
    async fn send(app: &Router, request: Request<Body>) -> StatusCode {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        response.into_body().collect().await.unwrap();
        status
    }

    fn get_req(path: &str) -> Request<Body> {
        Request::get(path).body(Body::empty()).unwrap()
    }

    fn attr(span: &SpanData, key: &str) -> Option<Value> {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.clone())
    }

    const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const PARENT: &str = "00f067aa0ba902b7";

    #[tokio::test(flavor = "current_thread")]
    async fn a_request_with_a_traceparent_gets_a_server_span_in_that_trace() {
        let h = harness();
        let app = app(&h);
        let request = Request::get("/documents/42")
            .header("traceparent", format!("00-{TRACE}-{PARENT}-01"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&app, request).await, StatusCode::OK);

        h.telemetry.force_flush();
        let spans = h.spans.get_finished_spans().unwrap();
        let span = spans
            .iter()
            .find(|s| s.span_kind == SpanKind::Server)
            .expect("a server span");
        assert_eq!(span.name, "GET /documents/{id}");
        assert_eq!(
            span.span_context.trace_id(),
            TraceId::from_hex(TRACE).unwrap()
        );
        assert_eq!(span.parent_span_id.to_string(), PARENT);
        assert_eq!(attr(span, "http.request.method"), Some("GET".into()));
        assert_eq!(attr(span, "http.route"), Some("/documents/{id}".into()));
        assert_eq!(
            attr(span, "http.response.status_code"),
            Some(200_i64.into())
        );
        assert_eq!(attr(span, "url.path"), Some("/documents/42".into()));
        assert_eq!(attr(span, "url.scheme"), Some("http".into()));
        assert_eq!(span.status, Status::Unset);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_server_error_marks_the_span_as_an_error() {
        let h = harness();
        let app = app(&h);
        assert_eq!(
            send(&app, get_req("/broken")).await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        h.telemetry.force_flush();
        let spans = h.spans.get_finished_spans().unwrap();
        let span = spans
            .iter()
            .find(|s| s.span_kind == SpanKind::Server)
            .unwrap();
        assert!(
            matches!(span.status, Status::Error { .. }),
            "{:?}",
            span.status
        );
        assert_eq!(attr(span, "error.type"), Some("500".into()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_event_inside_a_request_reaches_the_log_exporter_with_the_spans_trace_id() {
        let h = harness();
        let app = app(&h);
        let request = Request::get("/documents/7")
            .header("traceparent", format!("00-{TRACE}-{PARENT}-01"))
            .body(Body::empty())
            .unwrap();
        send(&app, request).await;

        h.telemetry.force_flush();
        let logs = h.logs.get_emitted_logs().unwrap();
        let log = logs
            .iter()
            .find(|l| {
                l.record
                    .body()
                    .is_some_and(|b| format!("{b:?}").contains("inside the request"))
            })
            .expect("the event as a log record");
        let context = log.record.trace_context().expect("a trace context");
        assert_eq!(context.trace_id, TraceId::from_hex(TRACE).unwrap());
        let spans = h.spans.get_finished_spans().unwrap();
        let server = spans
            .iter()
            .find(|s| s.span_kind == SpanKind::Server)
            .unwrap();
        assert_eq!(context.span_id, server.span_context.span_id());
    }

    /// The samples of `name` (a Prometheus series) whose labels contain every one of `labels`.
    pub fn samples(text: &str, name: &str, labels: &[&str]) -> Vec<String> {
        text.lines()
            .filter(|l| {
                l.strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
            })
            .filter(|l| labels.iter().all(|label| l.contains(label)))
            .map(str::to_owned)
            .collect()
    }

    pub fn value(line: &str) -> f64 {
        line.rsplit(' ').next().unwrap().parse().unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_request_whose_client_disconnects_is_recorded_as_client_disconnected() {
        let h = harness();
        let app = app(&h);
        // hyper drops the request future when the client goes away; dropping it here is the same.
        let dropped = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            app.clone().oneshot(get_req("/hang/1")),
        )
        .await;
        assert!(dropped.is_err(), "the handler never answers");

        let text = h.telemetry.prometheus_text();
        let counts = samples(
            &text,
            "http_server_request_duration_seconds_count",
            &[r#"http_route="/hang/{id}""#],
        );
        assert_eq!(counts.len(), 1, "{text}");
        assert!(
            counts[0].contains(r#"error_type="client_disconnected""#),
            "{text}"
        );
        assert!(
            !counts[0].contains("http_response_status_code"),
            "no status was sent: {text}"
        );
        assert_eq!(value(&counts[0]), 1.0, "{text}");
        let active = samples(
            &text,
            "http_server_active_requests",
            &[r#"http_route="/hang/{id}""#],
        );
        assert_eq!(active.len(), 1, "{text}");
        assert_eq!(value(&active[0]), 0.0, "{text}");

        h.telemetry.force_flush();
        let spans = h.spans.get_finished_spans().unwrap();
        let span = spans
            .iter()
            .find(|s| s.span_kind == SpanKind::Server)
            .expect("the server span ended");
        assert!(
            matches!(span.status, Status::Error { .. }),
            "{:?}",
            span.status
        );
        assert_eq!(attr(span, "error.type"), Some("client_disconnected".into()));
        assert_eq!(attr(span, "http.response.status_code"), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_normal_request_is_recorded_exactly_once() {
        let h = harness();
        let app = app(&h);
        assert_eq!(send(&app, get_req("/documents/1")).await, StatusCode::OK);

        let text = h.telemetry.prometheus_text();
        let counts = samples(
            &text,
            "http_server_request_duration_seconds_count",
            &[r#"http_route="/documents/{id}""#],
        );
        assert_eq!(counts.len(), 1, "{text}");
        assert_eq!(value(&counts[0]), 1.0, "{text}");
        assert!(!counts[0].contains("error_type"), "{text}");
        let active = samples(
            &text,
            "http_server_active_requests",
            &[r#"http_route="/documents/{id}""#],
        );
        assert_eq!(value(&active[0]), 0.0, "{text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_duration_histogram_is_labelled_by_route_not_by_path() {
        let h = harness();
        let app = app(&h);
        send(&app, get_req("/documents/12345")).await;
        send(&app, get_req("/nowhere/at/all")).await;

        let text = h.telemetry.prometheus_text();
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("http_server_request_duration_seconds_count"))
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l.contains(r#"http_route="/documents/{id}""#)
                    && l.contains(r#"http_request_method="GET""#)
                    && l.contains(r#"http_response_status_code="200""#)),
            "{text}"
        );
        assert!(!text.contains("12345"), "a raw path leaked: {text}");
        assert!(!text.contains("/nowhere"), "a raw path leaked: {text}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains(r#"http_response_status_code="404""#)
                    && !l.contains("http_route")),
            "an unmatched request is counted without a route: {text}"
        );
        assert!(text.contains("http_server_active_requests"), "{text}");
    }
}
