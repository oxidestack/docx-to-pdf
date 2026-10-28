//! OpenTelemetry providers and the `tracing` layers that feed them: the code instruments with `tracing` only, and
//! export is configured only by the standard `OTEL_*` variables.
//!
//! OTLP export is configured only by the standard `OTEL_*` variables and is off for a signal with no
//! endpoint, so a missing collector never produces errors. Metrics are read twice: by the Prometheus reader
//! behind `/metrics`, always, and by a periodic OTLP reader when metrics export is on.
//!
//! Nothing here is global: the propagator, the providers and the Prometheus registry belong to a
//! [`Telemetry`], so tests build their own with in-memory exporters.

use std::fmt;
use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Meter, MeterProvider as _};
use opentelemetry::propagation::{TextMapCompositePropagator, TextMapPropagator};
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{LogExporter, LoggerProviderBuilder, SdkLoggerProvider};
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{MeterProviderBuilder, PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider, SpanExporter, TracerProviderBuilder};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_VERSION};
use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{FilterFn, LevelFilter};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::registry::LookupSpan;

/// The instrumentation scope of everything this service emits.
pub const SCOPE: &str = "docx-to-pdf";

/// The service name when `OTEL_SERVICE_NAME` (or `service.name` in `OTEL_RESOURCE_ATTRIBUTES`) is unset.
pub const DEFAULT_SERVICE_NAME: &str = "docx-to-pdf";

/// The upstream release whose interface this service keeps, a resource attribute.
pub const UPSTREAM_VERSION: &str = "2.1.0-13";

/// Crates whose own diagnostics must not be fed back into the OpenTelemetry pipeline: the exporters log
/// through `tracing`, and exporting those logs would loop.
const PIPELINE_TARGETS: [&str; 6] = ["opentelemetry", "hyper", "h2", "tonic", "tower", "reqwest"];

/// Which signals go out over OTLP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OtlpSignals {
    /// Spans.
    pub traces: bool,
    /// Log records.
    pub logs: bool,
    /// Metrics, read periodically.
    pub metrics: bool,
}

impl fmt::Display for OtlpSignals {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let on = |b: bool| if b { "on" } else { "off" };
        write!(
            f,
            "traces {}, logs {}, metrics {}",
            on(self.traces),
            on(self.logs),
            on(self.metrics)
        )
    }
}

/// How one signal goes out over OTLP: `OTEL_EXPORTER_OTLP_<SIGNAL>_PROTOCOL`, else
/// `OTEL_EXPORTER_OTLP_PROTOCOL`, else the OpenTelemetry specification's default, `http/protobuf`.
/// The service passes it to the exporter explicitly: opentelemetry-otlp's own fallback is `http/json` whenever its
/// `http-json` feature is on, which it is, so `http/json` stays selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpProtocol {
    /// `grpc`.
    Grpc,
    /// `http/protobuf`: `application/x-protobuf` bodies.
    HttpProtobuf,
    /// `http/json`: `application/json` bodies.
    HttpJson,
}

impl OtlpProtocol {
    /// The protocol when no variable names one.
    pub const DEFAULT: Self = Self::HttpProtobuf;

    /// The variable's spelling of this protocol.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grpc => "grpc",
            Self::HttpProtobuf => "http/protobuf",
            Self::HttpJson => "http/json",
        }
    }

    /// A variable's value, case-insensitively, as opentelemetry-otlp reads it; `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Grpc, Self::HttpProtobuf, Self::HttpJson]
            .into_iter()
            .find(|p| value.trim().eq_ignore_ascii_case(p.as_str()))
    }

    /// The protocol of `signal` (`traces`, `logs` or `metrics`) given the variables `lookup` reads. A value that
    /// names no protocol is returned as the error, and the caller falls back to [`OtlpProtocol::DEFAULT`].
    pub fn of_signal(
        lookup: impl Fn(&str) -> Option<String>,
        signal: &str,
    ) -> Result<Self, String> {
        let get = |name: &str| {
            lookup(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let upper = signal.to_ascii_uppercase();
        match get(&format!("OTEL_EXPORTER_OTLP_{upper}_PROTOCOL"))
            .or_else(|| get("OTEL_EXPORTER_OTLP_PROTOCOL"))
        {
            None => Ok(Self::DEFAULT),
            Some(value) => Self::parse(&value).ok_or(value),
        }
    }

    /// The HTTP builders' protocol; `None` for gRPC, which has a builder of its own.
    fn http(self) -> Option<opentelemetry_otlp::Protocol> {
        match self {
            Self::Grpc => None,
            Self::HttpProtobuf => Some(opentelemetry_otlp::Protocol::HttpBinary),
            Self::HttpJson => Some(opentelemetry_otlp::Protocol::HttpJson),
        }
    }
}

/// The OpenTelemetry providers, the propagator and the Prometheus registry of one service.
pub struct Telemetry {
    tracer_provider: SdkTracerProvider,
    logger_provider: SdkLoggerProvider,
    meter_provider: SdkMeterProvider,
    registry: prometheus::Registry,
    propagator: Arc<TextMapCompositePropagator>,
    otlp: OtlpSignals,
    notes: Vec<String>,
    _process: crate::process_metrics::ProcessMetrics,
}

/// Builds a [`Telemetry`]: the resource, the exporters, the readers.
pub struct TelemetryBuilder {
    tracer: TracerProviderBuilder,
    logger: LoggerProviderBuilder,
    meter: MeterProviderBuilder,
    otlp: OtlpSignals,
    notes: Vec<String>,
}

impl Default for TelemetryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryBuilder {
    /// No exporters; the resource from `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES`, with
    /// `service.name` defaulting to `docx-to-pdf`, plus `service.version` and `upstream.version`. Nothing in
    /// it names the backend or its version: telemetry does not reveal which backend runs.
    pub fn new() -> Self {
        let resource = service_resource();
        Self {
            tracer: SdkTracerProvider::builder().with_resource(resource.clone()),
            logger: SdkLoggerProvider::builder().with_resource(resource.clone()),
            meter: SdkMeterProvider::builder().with_resource(resource),
            otlp: OtlpSignals::default(),
            notes: Vec::new(),
        }
    }

    /// Exports every span as soon as it ends (tests: an in-memory exporter).
    #[must_use]
    pub fn with_simple_span_exporter(mut self, exporter: impl SpanExporter + 'static) -> Self {
        self.tracer = self.tracer.with_simple_exporter(exporter);
        self
    }

    /// Adds a metric reader beside the Prometheus one (tests: a periodic reader over an in-memory exporter,
    /// which sees what the OTLP periodic reader pushes; [`Telemetry::force_flush`] makes it export).
    #[must_use]
    pub fn with_metric_reader(mut self, reader: impl MetricReader) -> Self {
        self.meter = self.meter.with_reader(reader);
        self
    }

    /// Exports every log record as soon as it is emitted (tests: an in-memory exporter).
    #[must_use]
    pub fn with_simple_log_exporter(mut self, exporter: impl LogExporter + 'static) -> Self {
        self.logger = self.logger.with_simple_exporter(exporter);
        self
    }

    /// OTLP exporters for the signals `OTEL_*` (read through `lookup`) turns on. The exporters themselves
    /// read the process environment for the endpoint, protocol, headers, timeout and compression.
    ///
    /// Building a gRPC exporter needs a Tokio runtime: call this inside one.
    #[must_use]
    pub fn with_otlp_from(mut self, lookup: impl Fn(&str) -> Option<String>) -> Self {
        let get = |name: &str| {
            lookup(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        if get("OTEL_SDK_DISABLED").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return self;
        }
        let general_endpoint = get("OTEL_EXPORTER_OTLP_ENDPOINT");

        let mut wanted = |signal: &str| -> Option<OtlpProtocol> {
            let upper = signal.to_ascii_uppercase();
            match get(&format!("OTEL_{upper}_EXPORTER")).as_deref() {
                None | Some("otlp") => {}
                Some("none") => return None,
                Some(other) => {
                    self.notes.push(format!(
                        "OTEL_{upper}_EXPORTER={other} is not supported (otlp or none); {signal} are not exported"
                    ));
                    return None;
                }
            }
            let signal_variable = format!("OTEL_EXPORTER_OTLP_{upper}_ENDPOINT");
            let (variable, endpoint) = get(&signal_variable)
                .map(|v| (signal_variable.as_str(), v))
                .or_else(|| {
                    general_endpoint
                        .clone()
                        .map(|v| ("OTEL_EXPORTER_OTLP_ENDPOINT", v))
                })?;
            // An endpoint is a URI, ASCII only (RFC 3986): the exporters would refuse it with "invalid uri
            // character", and the URL parser below them holds IDNA to ASCII (`idna_adapter` 1.0, Cargo.toml).
            if !endpoint.is_ascii() {
                self.notes.push(format!(
                    "{variable}={endpoint} is not ASCII: write an internationalised hostname in punycode \
                     (xn--…, e.g. `idn2 <name>`) and percent-encode the path; {signal} are not exported"
                ));
                return None;
            }
            Some(
                OtlpProtocol::of_signal(&lookup, signal).unwrap_or_else(|value| {
                    self.notes.push(format!(
                        "the OTLP protocol {value:?} for {signal} is not grpc, http/protobuf or http/json; \
                         {signal} are sent as {}",
                        OtlpProtocol::DEFAULT.as_str()
                    ));
                    OtlpProtocol::DEFAULT
                }),
            )
        };
        let traces = wanted("traces");
        let logs = wanted("logs");
        let metrics = wanted("metrics");

        if let Some(protocol) = traces {
            let built = match protocol.http() {
                None => opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .build(),
                Some(http) => opentelemetry_otlp::SpanExporter::builder()
                    .with_http()
                    .with_protocol(http)
                    .build(),
            };
            match built {
                Ok(exporter) => {
                    self.tracer = self.tracer.with_batch_exporter(exporter);
                    self.otlp.traces = true;
                }
                Err(e) => self
                    .notes
                    .push(format!("the OTLP span exporter failed to build: {e}")),
            }
        }
        if let Some(protocol) = logs {
            let built = match protocol.http() {
                None => opentelemetry_otlp::LogExporter::builder()
                    .with_tonic()
                    .build(),
                Some(http) => opentelemetry_otlp::LogExporter::builder()
                    .with_http()
                    .with_protocol(http)
                    .build(),
            };
            match built {
                Ok(exporter) => {
                    self.logger = self.logger.with_batch_exporter(exporter);
                    self.otlp.logs = true;
                }
                Err(e) => self
                    .notes
                    .push(format!("the OTLP log exporter failed to build: {e}")),
            }
        }
        if let Some(protocol) = metrics {
            let built = match protocol.http() {
                None => opentelemetry_otlp::MetricExporter::builder()
                    .with_tonic()
                    .build(),
                Some(http) => opentelemetry_otlp::MetricExporter::builder()
                    .with_http()
                    .with_protocol(http)
                    .build(),
            };
            match built {
                Ok(exporter) => {
                    self.meter = self
                        .meter
                        .with_reader(PeriodicReader::builder(exporter).build());
                    self.otlp.metrics = true;
                }
                Err(e) => self
                    .notes
                    .push(format!("the OTLP metric exporter failed to build: {e}")),
            }
        }
        self
    }

    /// The providers, with the Prometheus reader always attached to the meter provider and the
    /// `process.*` instruments registered on it.
    pub fn build(self) -> Result<Telemetry, TelemetryError> {
        let registry = prometheus::Registry::new();
        let prometheus = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .build()
            .map_err(|e| TelemetryError(format!("the Prometheus reader: {e}")))?;
        let meter_provider = self.meter.with_reader(prometheus).build();
        let process = crate::process_metrics::register(&meter_provider.meter(SCOPE));
        Ok(Telemetry {
            tracer_provider: self.tracer.build(),
            logger_provider: self.logger.build(),
            meter_provider,
            registry,
            propagator: Arc::new(TextMapCompositePropagator::new(vec![
                Box::new(TraceContextPropagator::new()),
                Box::new(BaggagePropagator::new()),
            ])),
            otlp: self.otlp,
            notes: self.notes,
            _process: process,
        })
    }
}

/// Telemetry could not be set up.
#[derive(Debug)]
pub struct TelemetryError(String);

impl fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TelemetryError {}

fn service_resource() -> Resource {
    let extra = [
        KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
        KeyValue::new("upstream.version", UPSTREAM_VERSION),
    ];
    let detected = Resource::builder().build();
    let named = detected
        .get(&opentelemetry::Key::from_static_str(SERVICE_NAME))
        .is_some_and(|name| !name.as_str().starts_with("unknown_service"));
    let builder = Resource::builder().with_attributes(extra);
    if named {
        builder.build()
    } else {
        builder.with_service_name(DEFAULT_SERVICE_NAME).build()
    }
}

impl Telemetry {
    /// OTLP from the process environment. Call inside a Tokio runtime (gRPC needs one).
    pub fn from_env() -> Result<Self, TelemetryError> {
        TelemetryBuilder::new()
            .with_otlp_from(|name| std::env::var(name).ok())
            .build()
    }

    /// Which signals go out over OTLP.
    pub fn otlp(&self) -> OtlpSignals {
        self.otlp
    }

    /// Problems met while configuring, to be logged once a subscriber is installed.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The `tracing` layers that feed OpenTelemetry: spans to the tracer provider, events to the logger
    /// provider, at `info` and above, never the exporters' own diagnostics.
    pub fn layers<S>(&self) -> impl Layer<S> + use<S>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let not_pipeline = FilterFn::new(|metadata| {
            let target = metadata.target();
            // `opentelemetry`, `opentelemetry::…` and `opentelemetry_sdk` alike.
            !PIPELINE_TARGETS.iter().any(|t| {
                target.strip_prefix(t).is_some_and(|rest| {
                    rest.is_empty() || rest.starts_with("::") || rest.starts_with('_')
                })
            })
        });
        let traces = tracing_opentelemetry::layer()
            .with_tracer(self.tracer_provider.tracer(SCOPE))
            .with_filter(LevelFilter::INFO);
        let logs = OpenTelemetryTracingBridge::new(&self.logger_provider)
            .with_filter(not_pipeline)
            .with_filter(LevelFilter::INFO);
        traces.and_then(logs).and_then(TracerLayer(self.tracer()))
    }

    /// The tracer every span of this service comes from: the one behind [`Telemetry::layers`], for spans
    /// whose start and end are known only after the fact ([`current_tracer`]).
    pub fn tracer(&self) -> SdkTracer {
        self.tracer_provider.tracer(SCOPE)
    }

    /// The meter every instrument of this service comes from.
    pub fn meter(&self) -> Meter {
        self.meter_provider.meter(SCOPE)
    }

    /// The W3C trace-context and baggage propagator incoming requests are read with.
    pub fn propagator(&self) -> Arc<dyn TextMapPropagator + Send + Sync> {
        self.propagator.clone()
    }

    /// The Prometheus text exposition of every metric (`/metrics`).
    pub fn prometheus_text(&self) -> String {
        render_prometheus(&self.registry)
    }

    /// A handle `/metrics` renders from.
    pub fn prometheus_registry(&self) -> prometheus::Registry {
        self.registry.clone()
    }

    /// Flushes and shuts every provider down. Blocks: call it outside an async task
    /// (`spawn_blocking`). Errors are returned as text, for stderr, since logging may be gone by then.
    pub fn shutdown(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if let Err(e) = self.tracer_provider.shutdown() {
            errors.push(format!("traces: {e}"));
        }
        if let Err(e) = self.meter_provider.shutdown() {
            errors.push(format!("metrics: {e}"));
        }
        if let Err(e) = self.logger_provider.shutdown() {
            errors.push(format!("logs: {e}"));
        }
        errors
    }

    /// Flushes every provider without shutting it down.
    pub fn force_flush(&self) {
        let _ = self.tracer_provider.force_flush();
        let _ = self.logger_provider.force_flush();
        let _ = self.meter_provider.force_flush();
    }
}

/// Carries the [`Telemetry::tracer`] inside the subscriber [`Telemetry::layers`] builds, so code that runs
/// under that subscriber finds it ([`current_tracer`]) without a handle passed down or a global. It observes
/// nothing.
struct TracerLayer(SdkTracer);

impl<S: Subscriber> Layer<S> for TracerLayer {}

/// The tracer of the [`Telemetry`] whose [`Telemetry::layers`] the current subscriber holds; `None` under a
/// subscriber without them (tests that install their own). A span built from it with explicit timestamps
/// (`SpanBuilder::with_start_time`, `Span::end_with_timestamp`) goes to the same exporters as the `tracing`
/// spans, and takes its parent from `tracing::Span::current()`'s OpenTelemetry context.
pub fn current_tracer() -> Option<SdkTracer> {
    tracing::dispatcher::get_default(|dispatch| {
        dispatch
            .downcast_ref::<TracerLayer>()
            .map(|layer| layer.0.clone())
    })
}

/// Installs the process-wide subscriber: stdout (`RUST_LOG`, default `info`; JSON lines unless stdout is a
/// terminal) plus [`Telemetry::layers`].
pub fn install_subscriber(telemetry: &Telemetry) -> Result<(), TelemetryError> {
    use std::io::IsTerminal as _;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stdout = if std::io::stdout().is_terminal() {
        tracing_subscriber::fmt::layer().boxed()
    } else {
        json_log_layer(std::io::stdout).boxed()
    };
    tracing_subscriber::registry()
        .with(stdout.with_filter(filter))
        .with(telemetry.layers())
        .try_init()
        .map_err(|e| TelemetryError(format!("the tracing subscriber: {e}")))
}

/// The stdout layer when no terminal is attached: one JSON object per line, the event's fields flattened,
/// the current span's fields under `span`, and the OpenTelemetry `trace_id` and `span_id` of the span the
/// event was emitted in (none outside a span).
pub fn json_log_layer<S, W>(make_writer: W) -> impl Layer<S> + use<S, W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> tracing_subscriber::fmt::MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(make_writer)
        .map_event_format(WithOtelIds)
}

/// Adds `trace_id` and `span_id` to the JSON object another formatter writes, from the OpenTelemetry
/// context active while the event is emitted: tracing-opentelemetry attaches a span's context on enter
/// (context activation), the same context the log bridge stamps on OTLP log records.
struct WithOtelIds<F>(F);

impl<S, N, F> FormatEvent<S, N> for WithOtelIds<F>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'w> FormatFields<'w> + 'static,
    F: FormatEvent<S, N>,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        let context = opentelemetry::Context::current();
        let span = context.span();
        let ids = span.span_context();
        if !ids.is_valid() {
            return self.0.format_event(ctx, writer, event);
        }
        let mut line = String::new();
        self.0.format_event(ctx, Writer::new(&mut line), event)?;
        // The JSON formatter writes one object and a newline; the ids go in before its closing brace.
        match line.rfind('}') {
            Some(end) => {
                let (head, tail) = line.split_at(end);
                let comma = if head.trim_end().ends_with('{') {
                    ""
                } else {
                    ","
                };
                write!(
                    writer,
                    "{head}{comma}\"trace_id\":\"{}\",\"span_id\":\"{}\"{tail}",
                    ids.trace_id(),
                    ids.span_id()
                )
            }
            None => writer.write_str(&line),
        }
    }
}

/// Encodes a Prometheus registry in the text format.
pub fn render_prometheus(registry: &prometheus::Registry) -> String {
    use prometheus::Encoder as _;
    let mut out = Vec::new();
    // Encoding into a Vec fails only on a malformed family, which the OpenTelemetry reader never builds.
    let _ = prometheus::TextEncoder::new().encode(&registry.gather(), &mut out);
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn otlp(vars: &[(&str, &str)]) -> OtlpSignals {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        TelemetryBuilder::new()
            .with_otlp_from(|name| vars.get(name).cloned())
            .build()
            .unwrap()
            .otlp()
    }

    const ALL: OtlpSignals = OtlpSignals {
        traces: true,
        logs: true,
        metrics: true,
    };

    #[test]
    fn nothing_is_exported_without_an_endpoint() {
        assert_eq!(otlp(&[]), OtlpSignals::default());
        assert_eq!(
            otlp(&[("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")]),
            OtlpSignals::default()
        );
    }

    #[test]
    fn the_general_endpoint_turns_every_signal_on() {
        assert_eq!(
            otlp(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9")]),
            ALL
        );
    }

    #[test]
    fn a_per_signal_endpoint_turns_on_that_signal_only() {
        assert_eq!(
            otlp(&[(
                "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
                "http://127.0.0.1:9/v1/logs"
            )]),
            OtlpSignals {
                logs: true,
                ..OtlpSignals::default()
            }
        );
    }

    #[test]
    fn exporter_none_turns_one_signal_off() {
        assert_eq!(
            otlp(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9"),
                ("OTEL_TRACES_EXPORTER", "none"),
            ]),
            OtlpSignals {
                traces: false,
                ..ALL
            }
        );
    }

    #[test]
    fn sdk_disabled_turns_every_signal_off() {
        assert_eq!(
            otlp(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9"),
                ("OTEL_SDK_DISABLED", "true"),
            ]),
            OtlpSignals::default()
        );
    }

    #[tokio::test]
    async fn grpc_builds_inside_a_runtime() {
        assert_eq!(
            otlp(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
            ]),
            ALL
        );
    }

    /// The specification's default, not opentelemetry-otlp's feature default (`http/json`).
    #[test]
    fn the_protocol_defaults_to_http_protobuf_and_a_signal_variable_wins() {
        let of = |vars: &[(&str, &str)], signal| {
            let vars: HashMap<String, String> = vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            OtlpProtocol::of_signal(|name| vars.get(name).cloned(), signal)
        };
        assert_eq!(of(&[], "traces"), Ok(OtlpProtocol::HttpProtobuf));
        assert_eq!(
            of(&[("OTEL_EXPORTER_OTLP_PROTOCOL", " HTTP/JSON ")], "logs"),
            Ok(OtlpProtocol::HttpJson)
        );
        assert_eq!(
            of(
                &[
                    ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"),
                    ("OTEL_EXPORTER_OTLP_METRICS_PROTOCOL", "grpc"),
                ],
                "metrics"
            ),
            Ok(OtlpProtocol::Grpc)
        );
        assert_eq!(
            of(&[("OTEL_EXPORTER_OTLP_PROTOCOL", "http")], "traces"),
            Err("http".to_owned())
        );
    }

    #[test]
    fn an_unknown_protocol_is_noted_and_sent_as_protobuf() {
        let vars: HashMap<&str, &str> = [
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "udp"),
        ]
        .into();
        let telemetry = TelemetryBuilder::new()
            .with_otlp_from(|name| vars.get(name).map(|v| (*v).to_owned()))
            .build()
            .unwrap();
        assert_eq!(telemetry.otlp(), ALL);
        assert_eq!(telemetry.notes().len(), 3, "{:?}", telemetry.notes());
        assert!(
            telemetry.notes()[0].contains("\"udp\"")
                && telemetry.notes()[0].contains("http/protobuf"),
            "{:?}",
            telemetry.notes()
        );
    }

    /// IDNA is held to ASCII (`idna_adapter` 1.0, Cargo.toml): a non-ASCII hostname is refused with a note that
    /// names the variable and the remedy, and that signal stays off rather than failing at every export.
    #[tokio::test]
    async fn a_non_ascii_endpoint_hostname_is_refused_with_a_note() {
        for protocol in ["http/protobuf", "http/json", "grpc"] {
            let vars: HashMap<&str, &str> = [
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9"),
                (
                    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                    "http://collector.bücher.example:4318",
                ),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", protocol),
            ]
            .into();
            let telemetry = TelemetryBuilder::new()
                .with_otlp_from(|name| vars.get(name).map(|v| (*v).to_owned()))
                .build()
                .unwrap();
            assert_eq!(
                telemetry.otlp(),
                OtlpSignals {
                    traces: false,
                    ..ALL
                },
                "{protocol}"
            );
            let notes = telemetry.notes().join("\n");
            assert!(
                notes.contains("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
                    && notes.contains("collector.bücher.example")
                    && notes.contains("punycode")
                    && notes.contains("xn--"),
                "{protocol}: {notes}"
            );
        }
    }

    #[tokio::test]
    async fn grpc_is_read_case_insensitively() {
        assert_eq!(
            otlp(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "GRPC"),
            ]),
            ALL
        );
    }

    #[test]
    fn the_resource_names_the_service_and_upstream() {
        let telemetry = TelemetryBuilder::new().build().unwrap();
        telemetry.meter().u64_counter("probe").build().add(1, &[]);
        let text = telemetry.prometheus_text();
        assert!(text.contains("target_info{"), "{text}");
        assert!(text.contains("upstream_version=\"2.1.0-13\""), "{text}");
        assert!(text.contains("service_name=\"docx-to-pdf\""), "{text}");
    }

    /// Telemetry does not reveal which backend runs, neither by name nor by
    /// a version that would identify it; `service.version` identifies the build.
    #[test]
    fn the_resource_does_not_reveal_the_backend() {
        let telemetry = TelemetryBuilder::new().build().unwrap();
        telemetry.meter().u64_counter("probe").build().add(1, &[]);
        let text = telemetry.prometheus_text();
        let target_info = text
            .lines()
            .find(|line| line.starts_with("target_info{"))
            .unwrap_or_else(|| panic!("a target_info sample: {text}"));
        for word in ["backend", "libreoffice", "lok", "soffice", "office"] {
            assert!(
                !target_info.to_lowercase().contains(word),
                "{word}: {target_info}"
            );
        }
        assert!(target_info.contains("service_version="), "{target_info}");
    }
}
