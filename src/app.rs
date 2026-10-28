//! The HTTP application: the router, the conversion paths upstream's clients call and the probe handlers.
//! Every error answer is a [`Problem`] (RFC 9457); CORS exists on the two conversion paths only.

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, post};
use opentelemetry::KeyValue;
use opentelemetry::metrics::Histogram;
use tower_http::cors::{AllowHeaders, Any, CorsLayer};
use tracing::Instrument as _;
use tracing::field::Empty;

use crate::allowlist::{self, Format};
use crate::config::Config;
use crate::conversion::{Conversions, PoolSlot};
use crate::http_telemetry::{HttpTelemetry, instrument};
use crate::pictures::Pictures;
use crate::pool::Pool;
use crate::problem::Problem;
use crate::readiness::Readiness;
use crate::telemetry::{Telemetry, render_prometheus};
use crate::upload::Upload;

/// What every handler can reach.
#[derive(Clone)]
pub struct AppState {
    /// The readiness seam `/readyz` reads.
    pub readiness: Readiness,
    /// The registry `/metrics` renders.
    pub metrics: prometheus::Registry,
    /// The request instrumentation.
    pub http: HttpTelemetry,
    /// How a conversion request's document is received: its size limit and where it is spooled.
    pub upload: Upload,
    /// `docx_to_pdf.document.size`: each received document's size, by its sniffed `format`.
    pub document_size: Histogram<u64>,
    /// The pool, once booted (`main` fills it while the router already serves).
    pub pool: PoolSlot,
    /// `docx_to_pdf.conversions`, `.conversion.duration` and `.pdf.size`.
    pub conversions: Conversions,
    /// Pictures LibreOffice would drop, or past `DOCX_TO_PDF_PICTURE_MAX_BITMAP`, changed before conversion.
    pub pictures: Pictures,
}

/// Document sizes from 1 KiB to 256 MiB, the default limit (10 MiB) among them; the largest documents with
/// images seen in use are 100-150 MB.
const SIZE_BUCKETS: [f64; 14] = [
    1024.0,
    4096.0,
    16384.0,
    65536.0,
    262_144.0,
    1_048_576.0,
    2_097_152.0,
    4_194_304.0,
    10_485_760.0,
    16_777_216.0,
    33_554_432.0,
    67_108_864.0,
    134_217_728.0,
    268_435_456.0,
];

impl AppState {
    /// The state for `readiness`, with metrics and request telemetry from `telemetry`, and documents up to
    /// the default limit (`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`'s default, 10 MiB).
    pub fn new(readiness: Readiness, telemetry: &Telemetry) -> Self {
        Self {
            readiness,
            metrics: telemetry.prometheus_registry(),
            http: HttpTelemetry::new(telemetry),
            upload: Upload::new(crate::config::DEFAULT_MAX_DOCUMENT_SIZE),
            document_size: telemetry
                .meter()
                .u64_histogram("docx_to_pdf.document.size")
                .with_unit("By")
                .with_description("Size of each received document, by its sniffed format.")
                .with_boundaries(SIZE_BUCKETS.to_vec())
                .build(),
            pool: PoolSlot::default(),
            conversions: Conversions::new(&telemetry.meter()),
            pictures: Pictures::new(
                &telemetry.meter(),
                crate::config::DEFAULT_PICTURE_MAX_BITMAP,
            ),
        }
    }

    /// The state the binary serves: `new`, with the limits `config` sets (`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`,
    /// `DOCX_TO_PDF_IDLE_TIMEOUT`, `DOCX_TO_PDF_MAX_INFLATED_SIZE`; the pictures step's threshold, its budget from
    /// `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` and its bound from the pool's size and wait, so a
    /// many-picture bomb is refused before any downscaling rather than after a minute of it).
    pub fn from_config(readiness: Readiness, telemetry: &Telemetry, config: &Config) -> Self {
        let mut upload = Upload::new(config.max_document_size);
        upload.idle_timeout = config.idle_timeout;
        upload.max_inflated_size = config.max_inflated_size;
        let pictures = Pictures::new(&telemetry.meter(), config.picture_max_bitmap)
            .with_hard_memory(config.instance_hard_memory)
            .with_concurrency(config.pool_size, config.pool_wait_timeout);
        Self {
            pictures,
            ..Self::new(readiness, telemetry).with_upload(upload)
        }
    }

    /// The same state, receiving documents as `upload` says (`Config::max_document_size`).
    #[must_use]
    pub fn with_upload(self, upload: Upload) -> Self {
        Self { upload, ..self }
    }

    /// Hands the booted pool to the conversion paths; until then they answer 503. A second pool is ignored.
    pub fn set_pool(&self, pool: Pool) {
        if self.pool.set(pool).is_err() {
            tracing::warn!("a pool was already set; the second one is ignored");
        }
    }
}

/// The service's router: an unknown path is a 404 problem, a known path with a wrong method a 405 problem
/// with `Allow` (axum fills `Allow` in from the route's methods; `get` also answers HEAD).
pub fn router(state: AppState) -> Router {
    let http = state.http.clone();
    let routes = Router::new()
        .route("/pdf", conversion())
        .route("/docx-to-pdf", conversion())
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(not_found)
        .with_state(state);
    instrument(routes, http)
}

/// A conversion path: `POST` only, behind CORS as moalhaddar/docx-to-pdf answers it, so a browser client keeps
/// working. The 405 fallback is set before the CORS layer so that a preflight `OPTIONS` reaches CORS rather than the
/// 405.
fn conversion() -> MethodRouter<AppState> {
    post(convert).fallback(method_not_allowed).layer(cors())
}

/// Any origin; the preflight names `POST`, echoes whatever headers were requested (none requested, none
/// named) and may be cached for 30 minutes. A preflight for another method still gets `POST`: the browser
/// refuses the request, not the server (upstream's 403 there is its framework's, and the browser enforces CORS
/// anyway).
fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::POST])
        .allow_headers(AllowHeaders::mirror_request())
        .expose_headers([crate::pictures::HEADER])
        .max_age(Duration::from_secs(1800))
}

/// How long a client waits before asking again while the pool is still booting.
const NO_CONVERSION_RETRY: Duration = Duration::from_secs(30);

/// `POST /pdf` and `POST /docx-to-pdf`: the document is received (400 for a bad form, 413 past the size limit),
/// its format sniffed from its bytes (415 outside the allowlist, before any pool instance is touched),
/// then converted on the pool (`crate::conversion`): 200 with the PDF, 422, 503 or 500. Before the pool has
/// booted, an allowed document gets "not ready", 503 with `Retry-After`. The temporary
/// directory goes with the [`crate::upload::ReceivedDocument`] on every path; on 200 the PDF's body owns it
/// and streams the PDF from it, a bounded buffer at a time, so the PDF is never held in memory.
async fn convert(State(state): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    let document = match state
        .upload
        .receive(&headers, body.into_data_stream())
        .await
    {
        Err(error) => return Problem::from(error).into_response(),
        Ok(document) => document,
    };
    let format = match sniff(&state, &document).await {
        Err(problem) => {
            if matches!(problem, Problem::UnsupportedFormat(_)) {
                state.conversions.unsupported();
            }
            return problem.into_response();
        }
        Ok(format) => format,
    };
    let Some(pool) = state.pool.get() else {
        return Problem::Unavailable {
            detail: "the service is not ready to convert documents yet",
            retry_after: NO_CONVERSION_RETRY,
        }
        .into_response();
    };
    let pictures = if format.is_zip() {
        match state.pictures.prepare(&document.path()).await {
            Ok(header) => header,
            Err(problem) => return problem.into_response(),
        }
    } else {
        None
    };
    match state.conversions.convert(pool, document, format).await {
        Ok(pdf) => {
            let mut response = pdf.into_response();
            if let Some(value) = pictures {
                response
                    .headers_mut()
                    .insert(crate::pictures::HEADER, value);
            }
            response
        }
        Err(problem) => problem.into_response(),
    }
}

/// The `document.sniff` span: the received document's format, recorded with its size in
/// `docx_to_pdf.document.size`. A refused document is 415 and `format` = `unsupported`; a file that cannot
/// be read back is 500 and sets the span's status to error.
async fn sniff(
    state: &AppState,
    document: &crate::upload::ReceivedDocument,
) -> Result<Format, Problem> {
    let span = tracing::info_span!(
        "document.sniff",
        otel.name = "document.sniff",
        otel.status_code = Empty,
        error.type = Empty,
        format = Empty,
    );
    let sniffed = allowlist::sniff_file(&document.path())
        .instrument(span.clone())
        .await;
    let format = match sniffed {
        Err(error) => {
            span.record("otel.status_code", "error");
            span.record("error.type", "io");
            tracing::error!(parent: &span, %error, "the spooled document could not be read back");
            return Err(Problem::Internal);
        }
        Ok(format) => format,
    };
    let name = format.map_or(allowlist::UNSUPPORTED, Format::name);
    span.record("format", name);
    state
        .document_size
        .record(document.size(), &[KeyValue::new("format", name)]);
    format.ok_or(Problem::UnsupportedFormat(allowlist::REFUSED))
}

async fn not_found() -> Problem {
    Problem::NotFound
}

async fn method_not_allowed() -> Problem {
    Problem::MethodNotAllowed
}

/// The Prometheus text exposition of the metrics that also go out over OTLP.
async fn metrics(State(state): State<AppState>) -> Response {
    (
        [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        render_prometheus(&state.metrics),
    )
        .into_response()
}

/// 200 while the process serves HTTP; never depends on LibreOffice, because restarting the container does not
/// fix a broken instance; replacing it does.
async fn livez() -> &'static str {
    "ok"
}

/// 200 once the pool is ready, 503 before that and from the start of shutdown.
async fn readyz(State(state): State<AppState>) -> Response {
    if state.readiness.is_ready() {
        "ok".into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{OtlpSignals, TelemetryBuilder};
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;
    use tracing_subscriber::layer::SubscriberExt;

    async fn call(app: &Router, method: Method, path: &str) -> (StatusCode, String) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn app() -> (Router, Readiness, Telemetry) {
        let readiness = Readiness::new();
        let telemetry = TelemetryBuilder::new().build().unwrap();
        let app = router(AppState::new(readiness.clone(), &telemetry));
        (app, readiness, telemetry)
    }

    #[tokio::test]
    async fn metrics_shows_the_request_just_made_by_route() {
        let (app, _, _t) = app();
        call(&app, Method::GET, "/livez").await;
        let (status, text) = call(&app, Method::GET, "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            text.lines().any(
                |l| l.starts_with("http_server_request_duration_seconds_count{")
                    && l.contains(r#"http_route="/livez""#)
            ),
            "{text}"
        );
    }

    /// Records every event at WARN or above, from any target.
    #[derive(Clone, Default)]
    struct Complaints(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Complaints {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() <= tracing::Level::WARN {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("{} {:?}", event.metadata().target(), event));
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn without_an_otlp_endpoint_the_service_serves_and_nothing_complains() {
        let complaints = Complaints::default();
        let telemetry = TelemetryBuilder::new()
            .with_otlp_from(|_| None)
            .build()
            .unwrap();
        let subscriber = tracing_subscriber::registry()
            .with(complaints.clone())
            .with(telemetry.layers());
        let _guard = tracing::subscriber::set_default(subscriber);

        assert_eq!(telemetry.otlp(), OtlpSignals::default());
        assert_eq!(telemetry.notes(), [] as [String; 0]);
        let app = router(AppState::new(Readiness::new(), &telemetry));
        assert_eq!(call(&app, Method::GET, "/livez").await.0, StatusCode::OK);
        tracing::info!("an event with nowhere to go");
        assert_eq!(call(&app, Method::GET, "/metrics").await.0, StatusCode::OK);
        telemetry.force_flush();
        assert_eq!(telemetry.shutdown(), [] as [String; 0]);

        let complaints = complaints.0.lock().unwrap();
        assert!(complaints.is_empty(), "{complaints:?}");
    }

    #[tokio::test]
    async fn livez_is_200_ok_even_before_ready() {
        let (app, _, _t) = app();
        assert_eq!(
            call(&app, Method::GET, "/livez").await,
            (StatusCode::OK, "ok".into())
        );
    }

    #[tokio::test]
    async fn readyz_is_503_until_marked_ready_then_200_then_503_once_shutdown_begins() {
        let (app, readiness, _t) = app();
        assert_eq!(
            call(&app, Method::GET, "/readyz").await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );

        readiness.mark_ready();
        assert_eq!(
            call(&app, Method::GET, "/readyz").await,
            (StatusCode::OK, "ok".into())
        );

        readiness.begin_shutdown();
        assert_eq!(
            call(&app, Method::GET, "/readyz").await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        readiness.mark_ready();
        assert_eq!(
            call(&app, Method::GET, "/readyz").await.0,
            StatusCode::SERVICE_UNAVAILABLE,
            "the pool cannot undo shutdown"
        );
    }

    #[tokio::test]
    async fn probes_answer_head_and_refuse_post() {
        let (app, _, _t) = app();
        assert_eq!(call(&app, Method::HEAD, "/livez").await.0, StatusCode::OK);
        assert_eq!(
            call(&app, Method::POST, "/livez").await.0,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn actuator_paths_do_not_exist() {
        let (app, _, _t) = app();
        assert_eq!(
            call(&app, Method::GET, "/actuator/health").await.0,
            StatusCode::NOT_FOUND
        );
    }

    /// An answer with its headers, for the tests of the interface.
    struct Answer {
        status: StatusCode,
        headers: axum::http::HeaderMap,
        body: Vec<u8>,
    }

    impl Answer {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers.get(name).map(|v| v.to_str().unwrap())
        }

        fn problem(&self) -> serde_json::Value {
            assert_eq!(
                self.header("content-type"),
                Some("application/problem+json"),
                "{:?}",
                self.headers
            );
            serde_json::from_slice(&self.body).unwrap()
        }
    }

    async fn send(app: &Router, method: Method, path: &str, headers: &[(&str, &str)]) -> Answer {
        let mut request = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Answer {
            status,
            headers,
            body: body.to_vec(),
        }
    }

    /// RFC 9457 §3.1: `type`, `title`, `status`, `detail`, and nothing that names the request.
    fn assert_problem(answer: &Answer, status: StatusCode) {
        assert_eq!(answer.status, status);
        let problem = answer.problem();
        assert_eq!(problem["type"], "about:blank", "{problem}");
        assert_eq!(
            problem["title"],
            status.canonical_reason().unwrap(),
            "{problem}"
        );
        assert_eq!(problem["status"], status.as_u16(), "{problem}");
        assert!(problem["detail"].is_string(), "{problem}");
        assert_eq!(problem.as_object().unwrap().len(), 4, "{problem}");
    }

    #[tokio::test]
    async fn an_unknown_path_is_a_404_problem_actuator_included() {
        let (app, _, _t) = app();
        for path in [
            "/nope",
            "/actuator",
            "/actuator/health",
            "/actuator/prometheus",
            "/",
        ] {
            for method in [Method::GET, Method::POST] {
                let answer = send(&app, method.clone(), path, &[]).await;
                assert_problem(&answer, StatusCode::NOT_FOUND);
                assert!(
                    !String::from_utf8_lossy(&answer.body).contains(path.trim_start_matches('/'))
                        || path == "/",
                    "{method} {path}: the body names the path"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_wrong_method_is_a_405_problem_with_allow() {
        let (app, _, _t) = app();
        for (method, path, allow) in [
            (Method::GET, "/pdf", "POST"),
            (Method::PUT, "/pdf", "POST"),
            (Method::DELETE, "/docx-to-pdf", "POST"),
            (Method::GET, "/docx-to-pdf", "POST"),
            (Method::POST, "/livez", "GET,HEAD"),
            (Method::POST, "/readyz", "GET,HEAD"),
            (Method::DELETE, "/metrics", "GET,HEAD"),
        ] {
            let answer = send(&app, method.clone(), path, &[]).await;
            assert_problem(&answer, StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(answer.header("allow"), Some(allow), "{method} {path}");
        }
    }

    #[tokio::test]
    async fn head_on_a_conversion_path_is_405_with_allow_and_no_body() {
        let (app, _, _t) = app();
        let answer = send(&app, Method::HEAD, "/pdf", &[]).await;
        assert_eq!(answer.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(answer.header("allow"), Some("POST"));
        assert_eq!(answer.body, [] as [u8; 0]);
    }

    const PREFLIGHT: [(&str, &str); 3] = [
        ("origin", "http://a.example"),
        ("access-control-request-method", "POST"),
        ("access-control-request-headers", "content-type"),
    ];

    #[tokio::test]
    async fn a_preflight_on_a_conversion_path_allows_post_from_any_origin() {
        let (app, _, _t) = app();
        for path in ["/pdf", "/docx-to-pdf"] {
            let answer = send(&app, Method::OPTIONS, path, &PREFLIGHT).await;
            assert_eq!(answer.status, StatusCode::OK, "{path}");
            assert_eq!(answer.header("access-control-allow-origin"), Some("*"));
            assert_eq!(answer.header("access-control-allow-methods"), Some("POST"));
            assert_eq!(
                answer.header("access-control-allow-headers"),
                Some("content-type")
            );
            assert_eq!(answer.header("access-control-max-age"), Some("1800"));
            assert_eq!(answer.header("content-type"), None);
            assert_eq!(answer.body, [] as [u8; 0]);
        }
    }

    #[tokio::test]
    async fn a_preflight_echoes_any_requested_header_and_none_when_none_is_requested() {
        let (app, _, _t) = app();
        let answer = send(
            &app,
            Method::OPTIONS,
            "/pdf",
            &[
                ("origin", "http://a.example"),
                ("access-control-request-method", "POST"),
                ("access-control-request-headers", "x-foo, traceparent"),
            ],
        )
        .await;
        assert_eq!(
            answer.header("access-control-allow-headers"),
            Some("x-foo, traceparent")
        );

        let answer = send(&app, Method::OPTIONS, "/pdf", &PREFLIGHT[..2]).await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.header("access-control-allow-headers"), None);
    }

    #[tokio::test]
    async fn a_preflight_for_another_method_still_names_only_post() {
        let (app, _, _t) = app();
        let answer = send(
            &app,
            Method::OPTIONS,
            "/pdf",
            &[
                ("origin", "http://a.example"),
                ("access-control-request-method", "GET"),
            ],
        )
        .await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.header("access-control-allow-methods"), Some("POST"));
    }

    #[tokio::test]
    async fn every_answer_of_a_conversion_path_to_a_cors_request_allows_any_origin() {
        let (app, _, _t) = app();
        let origin = [("origin", "http://a.example")];
        for (method, path) in [
            (Method::POST, "/pdf"),
            (Method::POST, "/docx-to-pdf"),
            (Method::GET, "/pdf"),
        ] {
            let answer = send(&app, method.clone(), path, &origin).await;
            assert_eq!(
                answer.header("access-control-allow-origin"),
                Some("*"),
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn other_paths_have_no_cors() {
        let (app, _, _t) = app();
        for (method, path) in [
            (Method::OPTIONS, "/livez"),
            (Method::GET, "/livez"),
            (Method::OPTIONS, "/actuator/health"),
            (Method::GET, "/nope"),
        ] {
            let answer = send(&app, method.clone(), path, &PREFLIGHT).await;
            assert_eq!(
                answer.header("access-control-allow-origin"),
                None,
                "{method} {path}"
            );
        }
        assert_problem(
            &send(&app, Method::OPTIONS, "/livez", &PREFLIGHT).await,
            StatusCode::METHOD_NOT_ALLOWED,
        );
    }

    /// A `multipart/form-data` body with one file part named `name`.
    fn upload_body(name: &str, content: &[u8]) -> Vec<u8> {
        let mut body = format!(
            "--B0011\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"a.docx\"\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n--B0011--\r\n");
        body
    }

    /// An RTF document of exactly `len` bytes: on the allowlist, so it gets as far as the pool.
    fn rtf(len: usize) -> Vec<u8> {
        let mut document = b"{\\rtf1 ".to_vec();
        document.resize(len.max(document.len()), b'a');
        document
    }

    async fn post(app: &Router, path: &str, headers: &[(&str, &str)], body: Vec<u8>) -> Answer {
        let mut request = Request::builder().method(Method::POST).uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Answer {
            status,
            headers,
            body: body.to_vec(),
        }
    }

    const FORM: (&str, &str) = ("content-type", "multipart/form-data; boundary=B0011");

    fn app_with_limit(max_document_size: u64) -> (Router, Telemetry) {
        let telemetry = TelemetryBuilder::new().build().unwrap();
        let state =
            AppState::new(Readiness::new(), &telemetry).with_upload(Upload::new(max_document_size));
        (router(state), telemetry)
    }

    #[tokio::test]
    async fn until_a_conversion_exists_a_received_document_gets_503_with_retry_after() {
        let (app, _, _t) = app();
        for path in ["/pdf", "/docx-to-pdf"] {
            let answer = post(&app, path, &[FORM], upload_body("document", &rtf(16))).await;
            assert_problem(&answer, StatusCode::SERVICE_UNAVAILABLE);
            assert!(answer.header("retry-after").is_some(), "{path}");
        }
    }

    #[tokio::test]
    async fn a_body_that_is_not_multipart_is_a_400_problem() {
        let (app, _, _t) = app();
        for path in ["/pdf", "/docx-to-pdf"] {
            assert_problem(
                &post(&app, path, &[], Vec::new()).await,
                StatusCode::BAD_REQUEST,
            );
            assert_problem(
                &post(
                    &app,
                    path,
                    &[("content-type", "text/plain")],
                    b"hello".to_vec(),
                )
                .await,
                StatusCode::BAD_REQUEST,
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_body_or_one_without_document_is_a_400_problem() {
        let (app, _, _t) = app();
        assert_problem(
            &post(&app, "/pdf", &[FORM], b"--B0011\r\ngarbage".to_vec()).await,
            StatusCode::BAD_REQUEST,
        );
        assert_problem(
            &post(&app, "/pdf", &[FORM], upload_body("file", b"PK")).await,
            StatusCode::BAD_REQUEST,
        );
    }

    #[tokio::test]
    async fn a_document_over_the_limit_is_a_413_problem_with_cors() {
        let (app, _t) = app_with_limit(1000);
        let origin = ("origin", "http://a.example");
        let answer = post(
            &app,
            "/pdf",
            &[FORM, origin],
            upload_body("document", &[b'a'; 1001]),
        )
        .await;
        assert_problem(&answer, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(answer.header("access-control-allow-origin"), Some("*"));
        let answer = post(
            &app,
            "/docx-to-pdf",
            &[FORM],
            upload_body("document", &rtf(1000)),
        )
        .await;
        assert_eq!(
            answer.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "at the limit"
        );
    }

    #[tokio::test]
    async fn the_state_main_builds_from_the_config_honours_the_max_document_size() {
        let telemetry = TelemetryBuilder::new().build().unwrap();
        let config = crate::config::Config::from_lookup(|name| {
            (name == "DOCX_TO_PDF_MAX_DOCUMENT_SIZE").then(|| "1000".to_owned())
        })
        .unwrap();
        let app = router(AppState::from_config(Readiness::new(), &telemetry, &config));
        let answer = post(
            &app,
            "/pdf",
            &[FORM],
            upload_body("document", &[b'a'; 1001]),
        )
        .await;
        assert_problem(&answer, StatusCode::PAYLOAD_TOO_LARGE);
        let answer = post(&app, "/pdf", &[FORM], upload_body("document", &rtf(1000))).await;
        assert_eq!(
            answer.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "at the limit"
        );
    }

    #[test]
    fn the_state_main_builds_from_the_config_takes_its_idle_limit() {
        let telemetry = TelemetryBuilder::new().build().unwrap();
        let config = crate::config::Config::from_lookup(|name| {
            (name == "DOCX_TO_PDF_IDLE_TIMEOUT").then(|| "300ms".to_owned())
        })
        .unwrap();
        let state = AppState::from_config(Readiness::new(), &telemetry, &config);
        assert_eq!(
            state.upload.idle_timeout,
            std::time::Duration::from_millis(300)
        );
    }

    #[tokio::test]
    async fn the_default_limit_is_10_mib() {
        let (app, _, _t) = app();
        let answer = post(
            &app,
            "/pdf",
            &[FORM],
            upload_body("document", &vec![0; 10 * 1024 * 1024 + 1]),
        )
        .await;
        assert_problem(&answer, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn the_conversion_paths_are_their_own_routes_in_telemetry() {
        let (app, _, _t) = app();
        post(&app, "/docx-to-pdf", &[], Vec::new()).await;
        let (_, text) = call(&app, Method::GET, "/metrics").await;
        assert!(
            text.lines().any(
                |l| l.starts_with("http_server_request_duration_seconds_count{")
                    && l.contains(r#"http_route="/docx-to-pdf""#)
            ),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_document_outside_the_allowlist_is_a_415_problem_with_cors() {
        let (app, readiness, _t) = app();
        readiness.mark_ready();
        let origin = ("origin", "http://a.example");
        for (content, what) in [
            (&b"hello"[..], "text"),
            (b"", "empty"),
            (b"PK\x03\x04 not an archive", "a broken zip"),
        ] {
            for path in ["/pdf", "/docx-to-pdf"] {
                let answer = post(
                    &app,
                    path,
                    &[FORM, origin],
                    upload_body("document", content),
                )
                .await;
                assert_problem(&answer, StatusCode::UNSUPPORTED_MEDIA_TYPE);
                assert_eq!(
                    answer.problem()["detail"],
                    "the document is not DOCX, DOC, ODT or RTF",
                    "{what}"
                );
                assert_eq!(answer.header("access-control-allow-origin"), Some("*"));
            }
        }
    }

    #[tokio::test]
    async fn the_document_size_is_recorded_by_format() {
        let (app, _, _t) = app();
        post(&app, "/pdf", &[FORM], upload_body("document", &rtf(100))).await;
        post(&app, "/pdf", &[FORM], upload_body("document", b"text")).await;
        let (_, text) = call(&app, Method::GET, "/metrics").await;
        for format in ["rtf", "unsupported"] {
            assert!(
                text.lines()
                    .any(|l| l.starts_with("docx_to_pdf_document_size_bytes_count{")
                        && l.contains(&format!(r#"format="{format}""#))
                        && l.ends_with(" 1")),
                "{format}: {text}"
            );
        }
        assert!(
            text.lines()
                .any(|l| l.starts_with("docx_to_pdf_document_size_bytes_sum{")
                    && l.contains(r#"format="rtf""#)
                    && l.ends_with(" 100")),
            "{text}"
        );
    }
}
