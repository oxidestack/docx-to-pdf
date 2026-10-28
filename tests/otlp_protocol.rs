//! The OTLP protocol the service sends, read off the wire: a collector configured by the OpenTelemetry defaults
//! must understand it. With no protocol variable set,
//! a span, a log record and a metric each reach an OTLP/HTTP listener as `application/x-protobuf` (the
//! OpenTelemetry specification's default, `http/protobuf`); `OTEL_EXPORTER_OTLP_PROTOCOL=http/json` sends JSON; a
//! per-signal `OTEL_EXPORTER_OTLP_<SIGNAL>_PROTOCOL` wins over the general one.
//!
//! The service is configured the way `main` configures it, [`Telemetry::from_env`], so each case sets the process
//! environment; one lock keeps the cases from overlapping. No LibreOffice needed.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use docx_to_pdf::telemetry::{OtlpProtocol, Telemetry};
use opentelemetry::trace::Tracer as _;
use tracing_subscriber::layer::SubscriberExt as _;

const PROTOBUF: &str = "application/x-protobuf";
const JSON: &str = "application/json";

/// Every variable a case may set: cleared before each case, so the test's own environment cannot leak in.
const VARIABLES: &[&str] = &[
    "OTEL_SDK_DISABLED",
    "OTEL_TRACES_EXPORTER",
    "OTEL_LOGS_EXPORTER",
    "OTEL_METRICS_EXPORTER",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
    "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
    "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
    "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
];

static ENVIRONMENT: Mutex<()> = Mutex::new(());

/// Answers every request with an empty `200` and reports each one's path and content type.
#[allow(
    clippy::needless_pass_by_value,
    reason = "a test helper that takes what its callers build in place"
)]
fn serve(stream: TcpStream, sent: mpsc::Sender<(String, String)>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
            return;
        }
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_owned();
        let (mut content_type, mut length) = (String::new(), 0usize);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if line.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                match name.trim().to_ascii_lowercase().as_str() {
                    "content-type" => value.trim().clone_into(&mut content_type),
                    "content-length" => length = value.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let _ = sent.send((path, content_type));
        if reader
            .get_mut()
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .is_err()
        {
            return;
        }
    }
}

/// With `vars` as the only OTLP variables besides an endpoint on a local listener: the content type each signal's
/// first export arrived with, keyed `traces`, `logs`, `metrics`.
#[allow(unsafe_code, reason = "edition 2024's environment calls are unsafe")]
fn content_types(vars: &[(&str, &str)]) -> BTreeMap<&'static str, String> {
    let _guard = ENVIRONMENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sent = sent.clone();
            std::thread::spawn(move || serve(stream, sent));
        }
    });

    // SAFETY: edition 2024 marks these unsafe because a concurrent C `getenv` could race them; the lock above
    // serialises every case of this binary, and nothing else in it touches the environment.
    unsafe {
        for variable in VARIABLES {
            std::env::remove_var(variable);
        }
        std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", format!("http://{address}"));
        for (name, value) in vars {
            std::env::set_var(name, value);
        }
    }

    let telemetry = Telemetry::from_env().unwrap();
    assert!(telemetry.notes().is_empty(), "{:?}", telemetry.notes());
    let subscriber = tracing_subscriber::registry().with(telemetry.layers());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("one log record");
    });
    drop(telemetry.tracer().start("one span"));
    telemetry
        .meter()
        .u64_counter("otlp_protocol.probe")
        .build()
        .add(1, &[]);
    let errors = telemetry.shutdown();
    assert!(errors.is_empty(), "{errors:?}");

    let mut seen = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while seen.len() < 3 {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok((path, content_type)) = received.recv_timeout(left) else {
            panic!("{vars:?}: exports seen {seen:?}, not all three signals");
        };
        let signal = match path.as_str() {
            "/v1/traces" => "traces",
            "/v1/logs" => "logs",
            "/v1/metrics" => "metrics",
            other => panic!("an export to {other}"),
        };
        seen.entry(signal).or_insert(content_type);
    }
    // What arrived is what the service resolves (the resolution `tests/config_defaults.rs` checks the
    // configuration page against).
    for (signal, content_type) in &seen {
        let resolved = OtlpProtocol::of_signal(|name| std::env::var(name).ok(), signal).unwrap();
        let wire = match resolved {
            OtlpProtocol::HttpProtobuf => PROTOBUF,
            OtlpProtocol::HttpJson => JSON,
            OtlpProtocol::Grpc => "grpc",
        };
        assert_eq!(content_type, wire, "{signal}: resolved {resolved:?}");
    }
    seen
}

fn expect(traces: &str, logs: &str, metrics: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("traces", traces.to_owned()),
        ("logs", logs.to_owned()),
        ("metrics", metrics.to_owned()),
    ])
}

#[test]
fn with_no_protocol_set_every_signal_goes_out_as_protobuf() {
    assert_eq!(content_types(&[]), expect(PROTOBUF, PROTOBUF, PROTOBUF));
}

#[test]
fn http_protobuf_set_explicitly_sends_protobuf() {
    assert_eq!(
        content_types(&[("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")]),
        expect(PROTOBUF, PROTOBUF, PROTOBUF)
    );
}

#[test]
fn http_json_sends_json() {
    assert_eq!(
        content_types(&[("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json")]),
        expect(JSON, JSON, JSON)
    );
}

#[test]
fn a_per_signal_protocol_wins_over_the_general_one() {
    assert_eq!(
        content_types(&[
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"),
            ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", "http/protobuf"),
        ]),
        expect(PROTOBUF, JSON, JSON)
    );
    assert_eq!(
        content_types(&[("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json")]),
        expect(PROTOBUF, JSON, PROTOBUF)
    );
}
