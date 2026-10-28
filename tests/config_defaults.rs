//! The Default column of the configuration reference, `docs/guide/configuration.md`, against the code: a row whose
//! default disagrees with what the service does when the variable is unset fails here, so the page never promises a default the service does not have.
//!
//! Every row has a check of its own in [`check`], and a row without one fails, so a new setting cannot land with an
//! unchecked default. The checks compare the cell with the code, never with a copy of the value:
//! - a setting of the service's configuration: setting the variable to the documented value gives the same
//!   configuration as leaving it unset; a default that follows another setting is checked by its rule;
//! - an alias ("—"): it has no default of its own, and a value in it acts as the same value in the main variable;
//! - an export switch: the signals exported with the documented value are those exported without it;
//! - the OTLP protocol: the protocol the service resolves for each signal with none set, which
//!   `tests/otlp_protocol.rs` shows is the content type every export arrives with;
//! - the OTLP timeout, the service name, `RUST_LOG`, `TMPDIR`: the library's constant, the service's constant, the
//!   filter `install_subscriber` falls back to, and the container image's `ENV`.
//!
//! `tests/config_reference.rs` holds the other half: every variable read has a row, and every row is read.
//!
//! No LibreOffice needed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use docx_to_pdf::config::{Config, parse_duration, parse_size};
use docx_to_pdf::telemetry::{DEFAULT_SERVICE_NAME, OtlpProtocol, OtlpSignals, TelemetryBuilder};

const PAGE: &str = "docs/guide/configuration.md";

const SIGNALS: [&str; 3] = ["TRACES", "LOGS", "METRICS"];

const ALL: OtlpSignals = OtlpSignals {
    traces: true,
    logs: true,
    metrics: true,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Each row of the page's tables that names one variable: the name as written (`<SIGNAL>` kept) and its
/// Default cell, the third.
fn rows() -> BTreeMap<String, String> {
    let page = std::fs::read_to_string(root().join(PAGE))
        .unwrap_or_else(|e| panic!("{PAGE}: {e}; the configuration reference is missing"));
    let mut rows = BTreeMap::new();
    for line in page.lines() {
        let Some(row) = line.trim().strip_prefix('|') else {
            continue;
        };
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        let Some(name) = cells
            .first()
            .and_then(|c| c.strip_prefix('`'))
            .and_then(|s| s.strip_suffix('`'))
            .filter(|s| !s.contains('`') && !s.contains(' '))
        else {
            continue;
        };
        let default = cells
            .get(2)
            .unwrap_or_else(|| panic!("{PAGE}: the row of {name} has no Default cell"));
        let before = rows.insert(name.to_owned(), (*default).to_owned());
        assert!(before.is_none(), "{PAGE}: {name} has two rows");
    }
    rows
}

/// The text inside the first pair of backticks in `cell`.
fn first_code(cell: &str) -> Option<&str> {
    let start = cell.find('`')? + 1;
    let len = cell[start..].find('`')?;
    Some(&cell[start..start + len])
}

/// The cell is one value in backticks and nothing else; that value.
fn only_value(cell: &str) -> Result<&str, String> {
    cell.strip_prefix('`')
        .and_then(|s| s.strip_suffix('`'))
        .filter(|s| !s.contains('`'))
        .ok_or_else(|| format!("expected one value in backticks, found {cell:?}"))
}

fn try_config(vars: &[(&str, &str)]) -> Result<Config, String> {
    let vars: HashMap<&str, &str> = vars.iter().copied().collect();
    Config::from_lookup(|name| vars.get(name).map(|v| (*v).to_owned()))
        .map_err(|e| format!("{vars:?} is refused: {e}"))
}

fn config(vars: &[(&str, &str)]) -> Config {
    try_config(vars).unwrap_or_else(|e| panic!("{e}"))
}

/// Setting `variable` to the documented `value` gives the configuration the service has with nothing set.
fn same_as_unset(variable: &str, value: &str) -> Result<(), String> {
    let unset = config(&[]);
    let set = try_config(&[(variable, value)])?;
    (set == unset).then_some(()).ok_or_else(|| {
        format!("{variable}={value} gives {set:?}, but with nothing set the service has {unset:?}")
    })
}

/// The cell says "—", and a value in `alias` acts as the same value in `main`.
fn alias_of(alias: &str, main: &str, sample: &str, cell: &str) -> Result<(), String> {
    if cell != "—" {
        return Err(format!(
            "an alias has no default of its own; expected \"—\", found {cell:?}"
        ));
    }
    let via_alias = config(&[(alias, sample)]);
    let via_main = config(&[(main, sample)]);
    (via_alias == via_main)
        .then_some(())
        .ok_or_else(|| format!("{alias}={sample} does not act as {main}={sample}"))
}

/// The cell starts with "unset", and `off` holds for the configuration the service has with nothing set.
fn unset_means(cell: &str, off: impl Fn(&Config) -> bool, what: &str) -> Result<(), String> {
    if !cell.starts_with("unset") {
        return Err(format!("expected \"unset: …\", found {cell:?}"));
    }
    off(&config(&[]))
        .then_some(())
        .ok_or_else(|| format!("with nothing set, {what} is not off"))
}

/// The cell names the general variable as the fallback of a per-signal one; the library reads the per-signal
/// variable first and the general one after it.
fn the_general(cell: &str, what: &str) -> Result<(), String> {
    let expected = format!("the general {what}");
    (cell == expected)
        .then_some(())
        .ok_or_else(|| format!("expected {expected:?}, found {cell:?}"))
}

/// The cell says "—": the variable has no default.
fn none(cell: &str) -> Result<(), String> {
    (cell == "—")
        .then_some(())
        .ok_or_else(|| format!("expected \"—\", found {cell:?}"))
}

/// The signals the service exports, given these variables.
fn otlp(vars: &[(&str, &str)]) -> OtlpSignals {
    let vars: HashMap<&str, &str> = vars.iter().copied().collect();
    let telemetry = TelemetryBuilder::new()
        .with_otlp_from(|name| vars.get(name).map(|v| (*v).to_owned()))
        .build()
        .unwrap_or_else(|e| panic!("{vars:?}: {e}"));
    let signals = telemetry.otlp();
    telemetry.shutdown();
    signals
}

const ENDPOINT: (&str, &str) = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9");

/// With an endpoint set, the documented value of `variable` exports what leaving it unset exports: every signal.
fn exports_as_unset(variables: &[&str], value: &str) -> Result<(), String> {
    let unset = otlp(&[ENDPOINT]);
    if unset != ALL {
        return Err(format!(
            "with only an endpoint set the service exports {unset}"
        ));
    }
    let mut vars = vec![ENDPOINT];
    vars.extend(variables.iter().map(|v| (*v, value)));
    let set = otlp(&vars);
    (set == unset)
        .then_some(())
        .ok_or_else(|| format!("{variables:?}={value} exports {set}, unset exports {unset}"))
}

/// The filter `install_subscriber` uses when `RUST_LOG` is unset or unusable, from `src/telemetry.rs`.
fn rust_log_fallback() -> Result<String, String> {
    let source = std::fs::read_to_string(root().join("src/telemetry.rs")).unwrap();
    let call = "EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(\"";
    let start = source.find(call).ok_or(
        "src/telemetry.rs: the RUST_LOG fallback is no longer spelled the way this test reads it",
    )? + call.len();
    let len = source[start..].find('"').ok_or("unterminated literal")?;
    Ok(source[start..start + len].to_owned())
}

/// The value the container image gives `variable` in an `ENV` line of the Dockerfile.
fn image_env(variable: &str) -> Option<String> {
    let dockerfile = std::fs::read_to_string(root().join("Dockerfile")).unwrap();
    let prefix = format!("{variable}=");
    dockerfile
        .lines()
        .filter_map(|line| line.trim().strip_prefix("ENV "))
        .flat_map(str::split_whitespace)
        .filter_map(|pair| pair.strip_prefix(prefix.as_str()))
        .next_back()
        .map(str::to_owned)
}

/// Whether the Default cell of `variable`'s row agrees with the code; `Err` says how it does not.
fn check(variable: &str, cell: &str) -> Result<(), String> {
    match variable {
        "DOCX_TO_PDF_PORT"
        | "DOCX_TO_PDF_IDLE_TIMEOUT"
        | "DOCX_TO_PDF_POOL_SIZE"
        | "DOCX_TO_PDF_POOL_MAX_WAITING"
        | "DOCX_TO_PDF_POOL_WAIT_TIMEOUT"
        | "DOCX_TO_PDF_INSTANCE_START_TIMEOUT"
        | "DOCX_TO_PDF_CONVERSION_TIMEOUT"
        | "DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS"
        | "DOCX_TO_PDF_INSTANCE_MAX_MEMORY"
        | "DOCX_TO_PDF_INSTANCE_HARD_MEMORY"
        | "DOCX_TO_PDF_MAX_DOCUMENT_SIZE"
        | "DOCX_TO_PDF_PICTURE_MAX_BITMAP" => same_as_unset(variable, only_value(cell)?),

        "SERVER_PORT" => alias_of(variable, "DOCX_TO_PDF_PORT", "9090", cell),
        "POOL_SIZE" | "pool.size" => alias_of(variable, "DOCX_TO_PDF_POOL_SIZE", "3", cell),

        "DOCX_TO_PDF_SHUTDOWN_GRACE" => {
            // "the value of `DOCX_TO_PDF_CONVERSION_TIMEOUT` (`<its default>`)".
            let followed = "DOCX_TO_PDF_CONVERSION_TIMEOUT";
            if first_code(cell) != Some(followed) {
                return Err(format!("expected it to follow {followed}, found {cell:?}"));
            }
            let stated = cell
                .split_once('(')
                .and_then(|(_, rest)| first_code(rest))
                .ok_or_else(|| format!("expected the value in parentheses, found {cell:?}"))?;
            let follows = config(&[(followed, "77s")]).shutdown_grace == Duration::from_secs(77);
            if !follows {
                return Err(format!("unset, it does not follow {followed}"));
            }
            same_as_unset(followed, stated)?;
            same_as_unset(variable, stated)
        }

        "DOCX_TO_PDF_MAX_INFLATED_SIZE" => {
            // "`<floor>`, or 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` when that is more".
            let floor = first_code(cell).ok_or_else(|| format!("no value in {cell:?}"))?;
            same_as_unset(variable, floor)?;
            if !cell.contains("3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` when that is more") {
                return Err(format!(
                    "expected the rule on three times the document limit, found {cell:?}"
                ));
            }
            let floor = parse_size(floor)?;
            let small = config(&[("DOCX_TO_PDF_MAX_DOCUMENT_SIZE", "1MiB")]).max_inflated_size;
            let large = config(&[("DOCX_TO_PDF_MAX_DOCUMENT_SIZE", "1GiB")]).max_inflated_size;
            (small == floor && large == 3 << 30).then_some(()).ok_or_else(|| {
                format!("for 1 MiB documents the limit is {small}, for 1 GiB {large}; the cell says {cell:?}")
            })
        }

        "DOCX_TO_PDF_PROBE_PORT" => unset_means(cell, |c| c.probe_port.is_none(), "the probe"),
        "DOCX_TO_PDF_PDF_MAX_IMAGE_DPI" => unset_means(
            cell,
            |c| c.pdf_max_image_dpi.is_none(),
            "the picture resolution limit",
        ),

        "OTEL_EXPORTER_OTLP_ENDPOINT" | "OTEL_EXPORTER_OTLP_<SIGNAL>_ENDPOINT" => {
            if !cell.starts_with("unset") {
                return Err(format!("expected \"unset…\", found {cell:?}"));
            }
            let exported = otlp(&[]);
            (exported == OtlpSignals::default())
                .then_some(())
                .ok_or_else(|| format!("with nothing set the service exports {exported}"))
        }

        "OTEL_EXPORTER_OTLP_PROTOCOL" => {
            let stated = only_value(cell)?;
            for signal in SIGNALS {
                let resolved = OtlpProtocol::of_signal(|_| None, &signal.to_ascii_lowercase())?;
                if stated != resolved.as_str() {
                    return Err(format!(
                        "the page says {stated}, {signal} with no protocol set go out as {}",
                        resolved.as_str()
                    ));
                }
            }
            Ok(())
        }
        "OTEL_EXPORTER_OTLP_<SIGNAL>_PROTOCOL" => the_general(cell, "protocol"),
        "OTEL_EXPORTER_OTLP_<SIGNAL>_HEADERS" => the_general(cell, "headers"),
        "OTEL_EXPORTER_OTLP_<SIGNAL>_TIMEOUT" => the_general(cell, "timeout"),

        "OTEL_<SIGNAL>_EXPORTER" => {
            let names: Vec<String> = SIGNALS
                .iter()
                .map(|s| format!("OTEL_{s}_EXPORTER"))
                .collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            exports_as_unset(&names, only_value(cell)?)
        }
        "OTEL_SDK_DISABLED" => exports_as_unset(&[variable], only_value(cell)?),

        "OTEL_EXPORTER_OTLP_TIMEOUT" => {
            let stated: u128 = only_value(cell)?
                .parse()
                .map_err(|_| format!("expected milliseconds, found {cell:?}"))?;
            let library = opentelemetry_otlp::OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT.as_millis();
            (stated == library).then_some(()).ok_or_else(|| {
                format!("the page says {stated} ms, opentelemetry-otlp waits {library} ms")
            })
        }

        "OTEL_SERVICE_NAME" => {
            let stated = only_value(cell)?;
            (stated == DEFAULT_SERVICE_NAME)
                .then_some(())
                .ok_or_else(|| {
                    format!(
                        "the page says {stated}, the service names itself {DEFAULT_SERVICE_NAME}"
                    )
                })
        }
        "OTEL_RESOURCE_ATTRIBUTES" | "OTEL_EXPORTER_OTLP_HEADERS" => none(cell),

        "RUST_LOG" => {
            let stated = only_value(cell)?;
            let fallback = rust_log_fallback()?;
            (stated == fallback).then_some(()).ok_or_else(|| {
                format!("the page says {stated}, the service falls back to {fallback}")
            })
        }

        "TMPDIR" => {
            let stated = only_value(cell)?;
            let image = image_env(variable).ok_or("the Dockerfile sets no TMPDIR")?;
            (stated == image)
                .then_some(())
                .ok_or_else(|| format!("the page says {stated}, the image sets {image}"))
        }

        _ => Err("no check for its default in tests/config_defaults.rs; add one".into()),
    }
}

#[test]
fn every_default_on_the_page_is_the_codes() {
    let rows = rows();
    assert!(rows.len() > 30, "{PAGE}: only {} rows read", rows.len());
    let wrong: BTreeSet<String> = rows
        .iter()
        .filter_map(|(variable, cell)| {
            check(variable, cell)
                .err()
                .map(|why| format!("{variable}: {why}"))
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{PAGE}: the Default column disagrees with the code:\n  {}",
        wrong.into_iter().collect::<Vec<_>>().join("\n  ")
    );
}

#[test]
fn a_wrong_default_is_caught() {
    // A guard on the method: each kind of check refuses a value the code does not have.
    for (variable, cell) in [
        ("DOCX_TO_PDF_IDLE_TIMEOUT", "`121s`"),
        ("DOCX_TO_PDF_INSTANCE_MAX_MEMORY", "`512MiB`"),
        ("DOCX_TO_PDF_INSTANCE_HARD_MEMORY", "`1GiB`"),
        ("SERVER_PORT", "`8080`"),
        (
            "DOCX_TO_PDF_SHUTDOWN_GRACE",
            "the value of `DOCX_TO_PDF_CONVERSION_TIMEOUT` (`120s`)",
        ),
        (
            "DOCX_TO_PDF_MAX_INFLATED_SIZE",
            "`128MiB`, or 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` when that is more",
        ),
        ("OTEL_SDK_DISABLED", "`true`"),
        ("OTEL_<SIGNAL>_EXPORTER", "`none`"),
        ("OTEL_EXPORTER_OTLP_TIMEOUT", "`30000`"),
        ("RUST_LOG", "`warn`"),
        ("a row nobody checks", "`1`"),
    ] {
        assert!(check(variable, cell).is_err(), "{variable} = {cell} passed");
    }
    // And a valid duration or size is what a same-as-unset check compares, not its spelling.
    assert_eq!(parse_duration("2m"), parse_duration("120s"));
    assert!(same_as_unset("DOCX_TO_PDF_IDLE_TIMEOUT", "2m").is_ok());
}
