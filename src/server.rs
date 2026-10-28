//! Serving with graceful shutdown: when the shutdown signal comes, readiness turns 503 at
//! once, no new connection is accepted, and in-flight requests may finish up to a grace period.
//!
//! And the idle limit, which cuts a client that stops sending (slow loris) rather than letting it hold a
//! connection and an upload slot forever: a connection on which no request is in flight and no byte
//! arrives for the limit is closed — with a 408 problem document when a request's head had begun, silently
//! when nothing of a new request had arrived (a fresh or a kept-alive connection). A body that stops is
//! answered 408 where it is read (`upload`); while a request is in flight (its body read, its conversion
//! running, its answer being sent) this limit does not count, however long the client stays silent.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use std::future::IntoFuture as _;

use axum::Router;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::Request;
use axum::extract::connect_info::{ConnectInfo, Connected};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::serve::{IncomingStream, Listener};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::{Instant, Sleep};

use crate::http_telemetry::GraceExpiry;
use crate::problem::{IDLE_TIMEOUT_DETAIL, Problem};
use crate::readiness::Readiness;

/// How serving ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown {
    /// Every in-flight request finished within the grace period.
    Drained,
    /// The grace period ran out; the requests still in flight were cut short with 503.
    GraceExceeded,
}

/// How long, after the grace period, the cut-short requests get to write their 503 before the connections
/// still open are cut.
const CUT_SHORT_WRITE: Duration = Duration::from_secs(1);

/// How long, after the connections are cut, their tasks get to end before serving is abandoned.
const CUT_CONNECTIONS: Duration = Duration::from_secs(1);

/// Serves `app` on `listener` until `signal` resolves, then drains for at most `grace`. A request still running when
/// the grace period ends has its handler dropped (so its work stops) and gets 503; a response body still being sent
/// ends with an error, so it is recorded before the telemetry flush. After [`CUT_SHORT_WRITE`] every connection still
/// open is cut, also one whose client stopped reading, so each request has been recorded and its body dropped (its
/// PDF directory removed) by the time this returns, before `main` flushes the telemetry.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    readiness: Readiness,
    signal: impl Future<Output = ()> + Send + 'static,
    grace: Duration,
) -> std::io::Result<Shutdown> {
    serve_with_idle_timeout(
        listener,
        app,
        readiness,
        signal,
        grace,
        crate::config::DEFAULT_IDLE_TIMEOUT,
    )
    .await
}

/// [`serve`] as the binary runs it: the grace (`DOCX_TO_PDF_SHUTDOWN_GRACE`) and the idle limit
/// (`DOCX_TO_PDF_IDLE_TIMEOUT`) from `config`.
pub fn serve_configured(
    listener: TcpListener,
    app: Router,
    readiness: Readiness,
    signal: impl Future<Output = ()> + Send + 'static,
    config: &crate::config::Config,
) -> impl Future<Output = std::io::Result<Shutdown>> + Send + 'static {
    serve_with_idle_timeout(
        listener,
        app,
        readiness,
        signal,
        config.shutdown_grace,
        config.idle_timeout,
    )
}

/// [`serve`], with `idle` as the idle limit (`DOCX_TO_PDF_IDLE_TIMEOUT`).
pub async fn serve_with_idle_timeout(
    listener: TcpListener,
    app: Router,
    readiness: Readiness,
    signal: impl Future<Output = ()> + Send + 'static,
    grace: Duration,
    idle: Duration,
) -> std::io::Result<Shutdown> {
    let (signalled, mut on_signal) = watch::channel(false);
    let (expire, expired) = watch::channel(false);
    let (cut, on_cut) = watch::channel(false);
    let listener = CuttableListener {
        inner: listener,
        cut: on_cut,
        idle,
    };
    let app = app.layer(middleware::from_fn(
        move |mut request: Request, next: Next| {
            let mut expired = expired.clone();
            request
                .extensions_mut()
                .insert(GraceExpiry::new(expired.clone()));
            // In flight from its head until its answer's body is gone: the idle limit does not count.
            let in_flight = request
                .extensions()
                .get::<ConnectInfo<Activity>>()
                .map(|ConnectInfo(activity)| activity.begin());
            async move {
                let response = tokio::select! {
                    response = next.run(request) => response,
                    _ = expired.wait_for(|e| *e) => cut_short(),
                };
                match in_flight {
                    Some(in_flight) => response.map(|inner| {
                        Body::new(InFlightBody {
                            inner,
                            _in_flight: in_flight,
                        })
                    }),
                    None => response,
                }
            }
        },
    ));
    let app = app.into_make_service_with_connect_info::<Activity>();

    let graceful = axum::serve(listener, app).with_graceful_shutdown(async move {
        signal.await;
        readiness.begin_shutdown();
        tracing::info!(
            grace_seconds = grace.as_secs_f64(),
            "shutting down: draining in-flight requests"
        );
        let _ = signalled.send(true);
    });
    let mut server = tokio::spawn(graceful.into_future());

    let grace_ran_out = async {
        // The sender is gone without a signal only when serving ended by itself; then never time out.
        if on_signal.wait_for(|s| *s).await.is_err() {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(grace).await;
    };

    let joined = tokio::select! {
        joined = &mut server => return finished(joined).map(|()| Shutdown::Drained),
        () = grace_ran_out => {
            tracing::warn!(
                grace_seconds = grace.as_secs_f64(),
                "the grace period ran out; cutting the requests still in flight short"
            );
            let _ = expire.send(true);
            if let Ok(joined) = tokio::time::timeout(CUT_SHORT_WRITE, &mut server).await { Ok(joined) } else {
                tracing::warn!("cutting the connections still open");
                let _ = cut.send(true);
                tokio::time::timeout(CUT_CONNECTIONS, &mut server).await
            }
        }
    };
    match joined {
        Ok(joined) => finished(joined)?,
        Err(_) => server.abort(),
    }
    Ok(Shutdown::GraceExceeded)
}

/// The listener `serve` serves on: its connections fail every read and write once `cut` turns `true`, so
/// hyper ends them even when it waits on a client that stopped reading, and each keeps the idle limit.
struct CuttableListener {
    inner: TcpListener,
    cut: watch::Receiver<bool>,
    idle: Duration,
}

/// What a connection's requests tell its stream: how many are in flight, when it last saw a byte or the end
/// of an answer, and whether a new request's head has begun since.
#[derive(Clone, Debug)]
struct Activity(Arc<ActivityState>);

#[derive(Debug)]
struct ActivityState {
    in_flight: AtomicUsize,
    last: Mutex<Instant>,
    head_begun: AtomicBool,
}

impl Activity {
    fn new() -> Self {
        Self(Arc::new(ActivityState {
            in_flight: AtomicUsize::new(0),
            last: Mutex::new(Instant::now()),
            head_begun: AtomicBool::new(false),
        }))
    }

    /// A request's head was read: it is in flight until the returned guard is dropped.
    fn begin(&self) -> InFlight {
        self.0.in_flight.fetch_add(1, Ordering::SeqCst);
        self.0.head_begun.store(false, Ordering::SeqCst);
        InFlight(self.clone())
    }

    /// Bytes arrived; with no request in flight they begin a new request's head.
    fn received(&self) {
        self.touch();
        if self.0.in_flight.load(Ordering::SeqCst) == 0 {
            self.0.head_begun.store(true, Ordering::SeqCst);
        }
    }

    fn touch(&self) {
        *self
            .0
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    /// When the idle limit `idle` runs out, or `None` while a request is in flight.
    fn deadline(&self, idle: Duration) -> Option<Instant> {
        if self.0.in_flight.load(Ordering::SeqCst) > 0 {
            return None;
        }
        Some(
            *self
                .0
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                + idle,
        )
    }
}

impl Connected<IncomingStream<'_, CuttableListener>> for Activity {
    fn connect_info(stream: IncomingStream<'_, CuttableListener>) -> Self {
        stream.io().activity.clone()
    }
}

/// A request in flight on its connection; dropped, the connection's idle clock restarts.
struct InFlight(Activity);

impl Drop for InFlight {
    fn drop(&mut self) {
        let state = &self.0.0;
        self.0.touch();
        state.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// An answer's body, holding its request [`InFlight`] until it has been sent or dropped. It forwards
/// `size_hint` and `is_end_stream`, so hyper frames the answer as before.
struct InFlightBody {
    inner: Body,
    _in_flight: InFlight,
}

impl HttpBody for InFlightBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, axum::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Listener for CuttableListener {
    type Io = CuttableStream;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, addr) = Listener::accept(&mut self.inner).await;
        let mut cut = self.cut.clone();
        let cut: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            // The sender is gone only when serving has ended; then the connection is never cut.
            if cut.wait_for(|c| *c).await.is_err() {
                std::future::pending::<()>().await;
            }
        });
        let idle = IdleClock {
            limit: self.idle,
            timer: Box::pin(tokio::time::sleep(self.idle)),
        };
        (
            CuttableStream {
                inner: stream,
                cut,
                activity: Activity::new(),
                idle,
            },
            addr,
        )
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// A connection that fails with `ConnectionAborted` once it is cut. Every poll also waits on the cut, so
/// a connection pending on its socket is woken by it. A read waiting with no request in flight also waits
/// on the idle limit, and fails with `TimedOut` when it runs out.
struct CuttableStream {
    inner: TcpStream,
    cut: Pin<Box<dyn Future<Output = ()> + Send>>,
    activity: Activity,
    idle: IdleClock,
}

/// The idle limit and its timer, re-armed to the connection's current deadline at each wait.
struct IdleClock {
    limit: Duration,
    timer: Pin<Box<Sleep>>,
}

impl CuttableStream {
    fn is_cut(&mut self, cx: &mut Context<'_>) -> bool {
        self.cut.as_mut().poll(cx).is_ready()
    }

    /// Whether the idle limit has run out while a read waits; registers the wake-up when it has not.
    fn is_idle_out(&mut self, cx: &mut Context<'_>) -> bool {
        let Some(deadline) = self.activity.deadline(self.idle.limit) else {
            return false;
        };
        if self.idle.timer.deadline() != deadline {
            self.idle.timer.as_mut().reset(deadline);
        }
        self.idle.timer.as_mut().poll(cx).is_ready()
    }

    /// Ends a connection the idle limit ran out on: a begun head gets a 408 problem document, written
    /// as far as the socket takes it at once; a connection with nothing of a new request gets nothing.
    fn idle_out(&self) -> io::Error {
        let head_begun = self.activity.0.head_begun.load(Ordering::SeqCst);
        tracing::info!(
            idle_seconds = self.idle.limit.as_secs_f64(),
            head_begun,
            "closing a connection idle past the limit"
        );
        if head_begun {
            let answer = request_timeout_answer();
            let mut sent = 0;
            while sent < answer.len() {
                match self.inner.try_write(&answer[sent..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sent += n,
                }
            }
        }
        io::Error::new(
            io::ErrorKind::TimedOut,
            "no byte of a request arrived within the idle limit",
        )
    }
}

/// The 408 for a head that stopped, written to the socket by hand: hyper has no request to answer yet.
fn request_timeout_answer() -> Vec<u8> {
    let problem = Problem::RequestTimeout(IDLE_TIMEOUT_DETAIL);
    let body = problem.body();
    format!(
        "HTTP/1.1 408 Request Timeout\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        crate::problem::PROBLEM_JSON,
        body.len()
    )
    .into_bytes()
}

fn aborted() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "the connection was cut at the end of the shutdown grace period",
    )
}

impl AsyncRead for CuttableStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.is_cut(cx) {
            return Poll::Ready(Err(aborted()));
        }
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                if buf.filled().len() > before {
                    self.activity.received();
                }
                Poll::Ready(Ok(()))
            }
            Poll::Pending if self.is_idle_out(cx) => Poll::Ready(Err(self.idle_out())),
            polled => polled,
        }
    }
}

impl AsyncWrite for CuttableStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.is_cut(cx) {
            return Poll::Ready(Err(aborted()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.is_cut(cx) {
            return Poll::Ready(Err(aborted()));
        }
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.is_cut(cx) {
            return Poll::Ready(Err(aborted()));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.is_cut(cx) {
            return Poll::Ready(Err(aborted()));
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn finished(joined: Result<io::Result<()>, tokio::task::JoinError>) -> io::Result<()> {
    joined.map_err(io::Error::other)?
}

/// The answer to a request cut short by shutdown: 503 with `Retry-After`, since another pod can take it.
fn cut_short() -> Response {
    Problem::Unavailable {
        detail: "the service is shutting down",
        retry_after: Duration::from_secs(1),
    }
    .into_response()
}

/// Resolves on SIGTERM or Ctrl-C.
pub async fn termination() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::{Notify, oneshot};
    use tokio::time::{Instant, sleep, timeout};

    /// Answers `/slow` with `done` after `delay`; the `Notify` gets a permit each time a request reaches the
    /// handler, so a test stops the server once a request is in flight rather than after a guessed pause, which
    /// a CPU-starved runner overshoots.
    fn slow_app(delay: Duration) -> (Router, Arc<Notify>) {
        let began = Arc::new(Notify::new());
        let notify = began.clone();
        let app = Router::new().route(
            "/slow",
            get(move || {
                let notify = notify.clone();
                async move {
                    notify.notify_one();
                    sleep(delay).await;
                    "done"
                }
            }),
        );
        (app, began)
    }

    /// Until a request has reached `slow_app`'s handler. The bound only keeps a broken server from hanging the
    /// test; a passing one never waits for it.
    async fn in_flight(began: &Notify) {
        timeout(Duration::from_secs(60), began.notified())
            .await
            .expect("the request reached the handler");
    }

    /// One HTTP/1.1 request over a raw socket; the status line and body, or the I/O error.
    async fn request(addr: std::net::SocketAddr, path: &str) -> std::io::Result<String> {
        let mut stream = TcpStream::connect(addr).await?;
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await?;
        let mut out = String::new();
        stream.read_to_string(&mut out).await?;
        Ok(out)
    }

    struct Running {
        addr: std::net::SocketAddr,
        readiness: Readiness,
        stop: oneshot::Sender<()>,
        served: tokio::task::JoinHandle<std::io::Result<Shutdown>>,
    }

    async fn start(app: Router, grace: Duration) -> Running {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let readiness = Readiness::new();
        readiness.mark_ready();
        let (stop, stopped) = oneshot::channel::<()>();
        let served = tokio::spawn(serve(
            listener,
            app,
            readiness.clone(),
            async move {
                let _ = stopped.await;
            },
            grace,
        ));
        Running {
            addr,
            readiness,
            stop,
            served,
        }
    }

    #[tokio::test]
    async fn shutdown_ends_readiness_at_once_and_drains_in_flight_requests() {
        let (app, began) = slow_app(Duration::from_millis(400));
        let r = start(app, Duration::from_secs(10)).await;
        let answering = tokio::spawn(request(r.addr, "/slow"));
        in_flight(&began).await;

        r.stop.send(()).unwrap();
        sleep(Duration::from_millis(20)).await;
        assert!(!r.readiness.is_ready(), "readiness must end at once");

        let answer = answering.await.unwrap().unwrap();
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.ends_with("done"), "{answer}");
        let outcome = timeout(Duration::from_secs(5), r.served).await.unwrap();
        assert_eq!(outcome.unwrap().unwrap(), Shutdown::Drained);
    }

    #[tokio::test]
    async fn a_request_past_the_grace_period_is_dropped_and_serving_ends() {
        let (app, began) = slow_app(Duration::from_secs(30));
        let r = start(app, Duration::from_millis(200)).await;
        let answering = tokio::spawn(request(r.addr, "/slow"));
        in_flight(&began).await;

        let began = Instant::now();
        r.stop.send(()).unwrap();
        let outcome = timeout(Duration::from_secs(5), r.served).await.unwrap();
        assert_eq!(outcome.unwrap().unwrap(), Shutdown::GraceExceeded);
        assert!(began.elapsed() < Duration::from_secs(2));
        let answer = timeout(Duration::from_secs(5), answering)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
        assert!(
            answer.to_ascii_lowercase().contains("retry-after: 1"),
            "{answer}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_request_cut_short_by_the_grace_period_is_recorded_as_shutdown_grace_exceeded() {
        use crate::http_telemetry::tests::{harness, samples, value};
        use crate::http_telemetry::{HttpTelemetry, instrument};
        use opentelemetry::trace::{SpanKind, Status};

        let h = harness();
        let (slow, began) = slow_app(Duration::from_secs(30));
        let app = instrument(slow, HttpTelemetry::new(&h.telemetry));
        let r = start(app, Duration::from_millis(200)).await;
        let answering = tokio::spawn(request(r.addr, "/slow"));
        in_flight(&began).await;
        r.stop.send(()).unwrap();
        let outcome = timeout(Duration::from_secs(5), r.served).await.unwrap();
        assert_eq!(outcome.unwrap().unwrap(), Shutdown::GraceExceeded);
        let answer = answering.await.unwrap().unwrap();
        assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");

        let text = h.telemetry.prometheus_text();
        let counts = samples(
            &text,
            "http_server_request_duration_seconds_count",
            &[r#"http_route="/slow""#],
        );
        assert_eq!(counts.len(), 1, "{text}");
        assert!(
            counts[0].contains(r#"http_response_status_code="503""#),
            "{text}"
        );
        assert!(
            counts[0].contains(r#"error_type="shutdown_grace_exceeded""#),
            "{text}"
        );
        assert_eq!(value(&counts[0]), 1.0, "{text}");
        let active = samples(
            &text,
            "http_server_active_requests",
            &[r#"http_route="/slow""#],
        );
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
        let error = span
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == "error.type")
            .map(|kv| kv.value.clone());
        assert_eq!(error, Some("shutdown_grace_exceeded".into()));
    }

    #[tokio::test]
    async fn no_new_connection_is_served_after_shutdown() {
        let r = start(slow_app(Duration::ZERO).0, Duration::from_secs(10)).await;
        let addr = r.addr;
        r.stop.send(()).unwrap();
        timeout(Duration::from_secs(5), r.served)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            request(addr, "/slow").await.is_err()
                || !request(addr, "/slow").await.unwrap().contains("done")
        );
    }
}
