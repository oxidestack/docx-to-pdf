//! The service's configuration, from `DOCX_TO_PDF_*` environment variables and upstream's aliases, so a
//! deployment's pool size and port carry over with no change to its values. `OTEL_*` is read by the telemetry
//! module, not here.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// The listening port when nothing sets one: upstream's 8080, which clients already call.
pub const DEFAULT_PORT: u16 = 8080;

/// How long in-flight requests may finish after SIGTERM when nothing sets it or the conversion timeout:
/// the conversion timeout's, so an in-flight conversion may run to its own limit.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = DEFAULT_CONVERSION_TIMEOUT;

/// The longest one conversion may run when nothing sets it: 180 s, about 2.9 times the slowest corpus document
/// measured, `tables-1000` (0.6 MiB, 63 s alone on one amd64 vCPU; moalhaddar/docx-to-pdf 66 s), so a slower or
/// busier core still converts it. LibreOffice is slow and a real report can take well over a minute, so the limit
/// is set from the measured tail, not from the typical report's 0.9 s.
pub const DEFAULT_CONVERSION_TIMEOUT: Duration = Duration::from_secs(180);

/// The longest a request may go without a byte of its head or body when nothing sets it: 120 s, the
/// Tomcat connection timeout every upstream deployment we read sets. It cuts a client that stops sending (slow
/// loris) without cutting an upload that only pauses.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Conversions after which an instance is recycled when nothing sets it. 3000 conversions on one instance showed
/// no growth (0.8 MiB a thousand), so this is insurance against a leak in LibreOffice, at one 0.3 s restart in
/// 200; Gotenberg's 10 costs it 7-11 % of its throughput on one amd64 vCPU.
pub const DEFAULT_INSTANCE_MAX_CONVERSIONS: u32 = 200;

/// The instance RSS above which it is recycled when nothing sets it: above the 646 MiB the typical reports leave
/// at most, below the 1.04-1.26 GiB a 1000-table document leaves for good.
pub const DEFAULT_INSTANCE_MAX_MEMORY: u64 = 1024 * 1024 * 1024;

/// The instance RSS above which a conversion is cut short when nothing sets it: the instance is killed
/// mid-conversion and its request answered 413, so a content bomb costs one request, never the pod — a huge
/// document must not take the pod out of memory in production. 2 GiB passes every heavy document measured within
/// the 10 MiB default (the worst, 2000 tables, 1.86 GiB) and stops the picture and pages bombs within seconds; the
/// pod's memory is sized for `POOL_SIZE` instances at it at once.
pub const DEFAULT_INSTANCE_HARD_MEMORY: u64 = 2 * 1024 * 1024 * 1024;

/// The largest document accepted when nothing sets it: 10 MiB, moalhaddar/docx-to-pdf's 10 485 760 bytes, so a
/// document upstream accepted is still accepted.
pub const DEFAULT_MAX_DOCUMENT_SIZE: u64 = 10 * 1024 * 1024;

/// The floor of the inflated-size limit when nothing sets it: 256 MiB, the `/tmp` the sizing model
/// gives one conversion when documents are small (`TMP_LONG_LAYOUT_MIB`), since LibreOffice writes an inflated part
/// to `/tmp` while it loads it. Every heavy document measured under the 10 MiB default inflates to less
/// (the most, 10 000 tables, 122 MiB, which times out anyway); the 7.4 MiB pages bomb inflates to 1.5 GiB.
pub const DEFAULT_MAX_INFLATED_SIZE: u64 = 256 * 1024 * 1024;

/// The decoded bitmap past which a picture is downscaled or replaced before conversion: a quarter of LibreOffice's
/// 1 GiB bound, past which LibreOffice drops the picture silently, so a conversion's pictures stay well under the
/// per-conversion hard memory limit (a picture near the bound peaks at 1.1-2.4 GiB).
pub const DEFAULT_PICTURE_MAX_BITMAP: u64 = 256 * 1024 * 1024;

/// The inflated-size limit when nothing sets it, for documents up to `max_document_size`: the larger of
/// [`DEFAULT_MAX_INFLATED_SIZE`] and three times the document limit — the sizing model's `/tmp` for one conversion,
/// `max(3 × D, 256 MiB)` — so a document limit raised for large pictures (stored, they inflate to about their size)
/// raises it too, and a bomb never takes more `/tmp` than the sizing reserves.
pub fn default_max_inflated_size(max_document_size: u64) -> u64 {
    DEFAULT_MAX_INFLATED_SIZE.max(max_document_size.saturating_mul(3))
}

/// LibreOffice instances when nothing sets it: upstream's `pool.size:1`, its default.
pub const DEFAULT_POOL_SIZE: usize = 1;

/// The variables that set the pool size, in precedence order: the service's own first, then upstream's two
/// spellings, both live in the upstream deployments we read, so their pool size carries over unchanged. Between
/// those two `POOL_SIZE` comes first, as in moalhaddar/docx-to-pdf: started with `POOL_SIZE=2` and `pool.size=3` it
/// starts two instances.
pub const POOL_SIZE_VARIABLES: &[&str] = &["DOCX_TO_PDF_POOL_SIZE", "POOL_SIZE", "pool.size"];

/// How long a request waits for a free instance when nothing sets it: about one slowest corpus document ahead,
/// or 60 typical reports at pool 1.
pub const DEFAULT_POOL_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

/// How many requests may wait for an instance at once when nothing sets it: 16 typical reports take about 15 s
/// at pool 1, so with the typical mix the queue refuses first, at once, and the wait limit binds only behind slow
/// documents.
pub const DEFAULT_POOL_MAX_WAITING: usize = 16;

/// How long an instance may take from spawn to its ready line when nothing sets it: 30 s, about 60 times the
/// slowest of 72 starts measured on amd64 (0.51 s) and 7 times moalhaddar/docx-to-pdf's 4.4 s on an amd64 test
/// host.
pub const DEFAULT_INSTANCE_START_TIMEOUT: Duration = Duration::from_secs(30);

/// The variables that set the largest document, in precedence order: the service's own first. An alias joins
/// this list after it; Spring's `SPRING_SERVLET_MULTIPART_MAX_FILE_SIZE` is not on it — Spring's multipart settings
/// are not read, and whether that one should become an alias is an open decision.
pub const MAX_DOCUMENT_SIZE_VARIABLES: &[&str] = &["DOCX_TO_PDF_MAX_DOCUMENT_SIZE"];

/// What the service reads at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `DOCX_TO_PDF_PORT`, else upstream's `SERVER_PORT`, else 8080.
    pub port: u16,
    /// `DOCX_TO_PDF_SHUTDOWN_GRACE`, else the conversion timeout: after SIGTERM, how long in-flight requests
    /// may finish.
    pub shutdown_grace: Duration,
    /// `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`, in bytes: the largest document accepted; past it, 413 while the
    /// body is still arriving.
    pub max_document_size: u64,
    /// `DOCX_TO_PDF_MAX_INFLATED_SIZE`, in bytes, else [`default_max_inflated_size`]: a DOCX or ODT whose parts
    /// inflate past it is refused with 413 before any instance sees it; 0 = no limit. Not below a non-zero
    /// `max_document_size`, which would refuse documents whose pictures are stored as they are.
    pub max_inflated_size: u64,
    /// `DOCX_TO_PDF_PICTURE_MAX_BITMAP`, in bytes, else [`DEFAULT_PICTURE_MAX_BITMAP`]: a PNG or JPEG in a DOCX or ODT
    /// whose decoded bitmap passes it is downscaled or replaced before conversion; capped where it is used at
    /// LibreOffice's own bound, past which LibreOffice drops the picture.
    pub picture_max_bitmap: u64,
    /// `DOCX_TO_PDF_POOL_SIZE`, else upstream's `POOL_SIZE`, else its `pool.size`, else 1: LibreOffice
    /// instances, i.e. conversions in parallel.
    pub pool_size: usize,
    /// `DOCX_TO_PDF_POOL_WAIT_TIMEOUT`: the longest a request waits for a free instance; past it, 503.
    pub pool_wait_timeout: Duration,
    /// `DOCX_TO_PDF_POOL_MAX_WAITING`: requests allowed to wait at once; the next one gets 503 at once.
    pub pool_max_waiting: usize,
    /// `DOCX_TO_PDF_INSTANCE_START_TIMEOUT`: the longest an instance may take from spawn to its ready line.
    pub instance_start_timeout: Duration,
    /// `DOCX_TO_PDF_CONVERSION_TIMEOUT`: the longest one conversion may run; past it the instance is killed
    /// and replaced and the request gets 504.
    pub conversion_timeout: Duration,
    /// `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS`: an instance is recycled after this many conversions; 0 = never.
    pub instance_max_conversions: u32,
    /// `DOCX_TO_PDF_INSTANCE_MAX_MEMORY`, in bytes: an instance whose RSS is above it after a conversion is
    /// recycled; 0 = no ceiling.
    pub instance_max_memory: u64,
    /// `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`, in bytes: an instance whose RSS passes it during a conversion is
    /// killed at once and replaced, and the request gets 413; 0 = no hard limit. Above a non-zero
    /// `instance_max_memory`, so an instance the ceiling keeps is never killed at its next conversion's start.
    pub instance_hard_memory: u64,
    /// `DOCX_TO_PDF_IDLE_TIMEOUT`: the longest a request may go without a byte of its head or body; past
    /// it, 408 and the connection is closed. A connection with no request in flight is closed after it too.
    pub idle_timeout: Duration,
    /// `DOCX_TO_PDF_PROBE_PORT`: the port of the platform probe, `POST /probe`; unset, no
    /// probe listens. Never the conversion port: the probe holds every instance for minutes.
    pub probe_port: Option<u16>,
    /// `DOCX_TO_PDF_PDF_MAX_IMAGE_DPI`: a picture above this resolution is reduced to it in the PDF; unset,
    /// pictures are embedded as they are (reducing costs the typical report two thirds of its CPU).
    pub pdf_max_image_dpi: Option<u32>,
    /// Where each instance's temporary profile directory is made: the system's temporary directory
    /// (`TMPDIR`).
    pub profiles_dir: PathBuf,
    /// What the service set aside while reading the variables, to log as warnings once logging is up: a value it
    /// ignored rather than refused, such as a Kubernetes service link.
    pub notes: Vec<String>,
}

/// A setting whose value cannot be used; the service refuses to start on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// The variable that held the value.
    pub variable: &'static str,
    /// What was wrong with it.
    pub reason: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.variable, self.reason)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Reads the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the variables through `lookup`, so tests need not touch the process environment.
    /// An empty value counts as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &'static str| {
            lookup(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .map(|v| (name, v))
        };

        let mut notes = Vec::new();
        // A Kubernetes service link is not a setting: set aside with a note, as though unset, so a Service named
        // `docx-to-pdf` does not stop the pod from starting.
        let get_port = |name: &'static str, notes: &mut Vec<String>| {
            get(name).filter(|(variable, value)| {
                let link = is_service_link(value);
                if link {
                    notes.push(format!(
                        "{variable}={value} ignored: the form of a Kubernetes service link, not a port; \
                         set the port explicitly or turn service links off (enableServiceLinks: false)"
                    ));
                }
                !link
            })
        };

        // The service's own variable wins over upstream's alias when both are set.
        let port = match get_port("DOCX_TO_PDF_PORT", &mut notes).or_else(|| get("SERVER_PORT")) {
            Some((variable, value)) => parse_port(variable, &value)?,
            None => DEFAULT_PORT,
        };

        let conversion_timeout = match get("DOCX_TO_PDF_CONVERSION_TIMEOUT") {
            Some((variable, value)) => match parse_duration(&value) {
                Ok(timeout) if timeout.is_zero() => {
                    return Err(ConfigError {
                        variable,
                        reason: "the timeout must be above zero".into(),
                    });
                }
                Ok(timeout) => timeout,
                Err(reason) => return Err(ConfigError { variable, reason }),
            },
            None => DEFAULT_CONVERSION_TIMEOUT,
        };

        // Unset, the grace is the conversion timeout, so an in-flight conversion may run to its own limit.
        let shutdown_grace = match get("DOCX_TO_PDF_SHUTDOWN_GRACE") {
            Some((variable, value)) => {
                parse_duration(&value).map_err(|reason| ConfigError { variable, reason })?
            }
            None => conversion_timeout,
        };

        let instance_max_conversions = match get("DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS") {
            Some((variable, value)) => value.parse::<u32>().map_err(|_| ConfigError {
                variable,
                reason: format!("{value:?} is not a number of conversions (0 = never)"),
            })?,
            None => DEFAULT_INSTANCE_MAX_CONVERSIONS,
        };

        let instance_max_memory = match get("DOCX_TO_PDF_INSTANCE_MAX_MEMORY") {
            Some((variable, value)) => {
                parse_size(&value).map_err(|reason| ConfigError { variable, reason })?
            }
            None => DEFAULT_INSTANCE_MAX_MEMORY,
        };

        let instance_hard_memory = match get("DOCX_TO_PDF_INSTANCE_HARD_MEMORY") {
            Some((variable, value)) => {
                parse_size(&value).map_err(|reason| ConfigError { variable, reason })?
            }
            None => DEFAULT_INSTANCE_HARD_MEMORY,
        };
        // An instance the ceiling leaves running between conversions must not already be past the hard limit.
        if instance_hard_memory > 0
            && instance_max_memory > 0
            && instance_hard_memory <= instance_max_memory
        {
            return Err(ConfigError {
                variable: "DOCX_TO_PDF_INSTANCE_HARD_MEMORY",
                reason: format!(
                    "{instance_hard_memory} bytes is not above DOCX_TO_PDF_INSTANCE_MAX_MEMORY \
                     ({instance_max_memory} bytes); raise it, or set 0 for no hard limit"
                ),
            });
        }

        let max_document_size = match MAX_DOCUMENT_SIZE_VARIABLES
            .iter()
            .find_map(|variable| get(variable))
        {
            Some((variable, value)) => match parse_size(&value) {
                Ok(0) => {
                    return Err(ConfigError {
                        variable,
                        reason: "the size must be above zero".into(),
                    });
                }
                Ok(size) => size,
                Err(reason) => return Err(ConfigError { variable, reason }),
            },
            None => DEFAULT_MAX_DOCUMENT_SIZE,
        };

        let max_inflated_size = match get("DOCX_TO_PDF_MAX_INFLATED_SIZE") {
            Some((variable, value)) => match parse_size(&value) {
                Ok(size) if size > 0 && size < max_document_size => {
                    return Err(ConfigError {
                        variable,
                        reason: format!(
                            "{size} bytes is below DOCX_TO_PDF_MAX_DOCUMENT_SIZE ({max_document_size} bytes), \
                             so documents within the size limit would be refused; raise it, or set 0 for no limit"
                        ),
                    });
                }
                Ok(size) => size,
                Err(reason) => return Err(ConfigError { variable, reason }),
            },
            None => default_max_inflated_size(max_document_size),
        };

        let picture_max_bitmap = match get("DOCX_TO_PDF_PICTURE_MAX_BITMAP") {
            Some((variable, value)) => match parse_size(&value) {
                Ok(0) => {
                    return Err(ConfigError {
                        variable,
                        reason: "0 would replace every picture; set a size in bytes".into(),
                    });
                }
                Ok(size) => size,
                Err(reason) => return Err(ConfigError { variable, reason }),
            },
            None => DEFAULT_PICTURE_MAX_BITMAP,
        };

        let pool_size = match POOL_SIZE_VARIABLES
            .iter()
            .find_map(|variable| get(variable))
        {
            Some((variable, value)) => match value.parse::<usize>() {
                Ok(size) if size > 0 => size,
                _ => {
                    return Err(ConfigError {
                        variable,
                        reason: format!("{value:?} is not a number of instances (1 or more)"),
                    });
                }
            },
            None => DEFAULT_POOL_SIZE,
        };

        let duration = |name: &'static str, default: Duration| match get(name) {
            Some((variable, value)) => {
                parse_duration(&value).map_err(|reason| ConfigError { variable, reason })
            }
            None => Ok(default),
        };
        let pool_wait_timeout =
            duration("DOCX_TO_PDF_POOL_WAIT_TIMEOUT", DEFAULT_POOL_WAIT_TIMEOUT)?;
        let instance_start_timeout = duration(
            "DOCX_TO_PDF_INSTANCE_START_TIMEOUT",
            DEFAULT_INSTANCE_START_TIMEOUT,
        )?;
        if instance_start_timeout.is_zero() {
            return Err(ConfigError {
                variable: "DOCX_TO_PDF_INSTANCE_START_TIMEOUT",
                reason: "the timeout must be above zero".into(),
            });
        }

        let idle_timeout = duration("DOCX_TO_PDF_IDLE_TIMEOUT", DEFAULT_IDLE_TIMEOUT)?;
        if idle_timeout.is_zero() {
            return Err(ConfigError {
                variable: "DOCX_TO_PDF_IDLE_TIMEOUT",
                reason: "the timeout must be above zero".into(),
            });
        }

        let pool_max_waiting = match get("DOCX_TO_PDF_POOL_MAX_WAITING") {
            Some((variable, value)) => value.parse::<usize>().map_err(|_| ConfigError {
                variable,
                reason: format!("{value:?} is not a number of requests (0 or more)"),
            })?,
            None => DEFAULT_POOL_MAX_WAITING,
        };

        let probe_port = match get_port("DOCX_TO_PDF_PROBE_PORT", &mut notes) {
            Some((variable, value)) => {
                let probe = parse_port(variable, &value)?;
                if probe == port {
                    return Err(ConfigError {
                        variable,
                        reason: format!(
                            "{probe} is the conversion port; the probe needs a port of its own"
                        ),
                    });
                }
                Some(probe)
            }
            None => None,
        };

        let pdf_max_image_dpi = match get("DOCX_TO_PDF_PDF_MAX_IMAGE_DPI") {
            Some((variable, value)) => match value.parse::<u32>() {
                Ok(dpi) if dpi > 0 => Some(dpi),
                _ => {
                    return Err(ConfigError {
                        variable,
                        reason: format!("{value:?} is not a resolution in DPI (1 or more)"),
                    });
                }
            },
            None => None,
        };

        Ok(Self {
            port,
            probe_port,
            pdf_max_image_dpi,
            shutdown_grace,
            max_document_size,
            max_inflated_size,
            picture_max_bitmap,
            pool_size,
            pool_wait_timeout,
            pool_max_waiting,
            instance_start_timeout,
            conversion_timeout,
            instance_max_conversions,
            instance_max_memory,
            instance_hard_memory,
            idle_timeout,
            profiles_dir: std::env::temp_dir(),
            notes,
        })
    }
}

/// The value Kubernetes injects as `<SERVICE>_PORT` for every Service in the namespace: `tcp://10.96.96.218:8080`
/// (the protocol in lower case: `tcp`, `udp` or `sctp`; an IPv6 address in brackets).
fn is_service_link(value: &str) -> bool {
    ["tcp://", "udp://", "sctp://"].iter().any(|scheme| {
        value
            .strip_prefix(scheme)
            .is_some_and(|address| address.parse::<std::net::SocketAddr>().is_ok())
    })
}

fn parse_port(variable: &'static str, value: &str) -> Result<u16, ConfigError> {
    match value.parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(ConfigError {
            variable,
            reason: format!("{value:?} is not a port number (1-65535)"),
        }),
    }
}

/// A duration with a unit: `500ms`, `30s`, `2m`, `1h` (the migration guide's form).
pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let value = value.trim();
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("{value:?} is not a duration such as 500ms, 30s or 2m"))?;
    let millis = match unit.trim() {
        "ms" => Some(number),
        "s" => number.checked_mul(1_000),
        "m" => number.checked_mul(60_000),
        "h" => number.checked_mul(3_600_000),
        _ => return Err(format!("{value:?} needs a unit: ms, s, m or h")),
    };
    millis
        .map(Duration::from_millis)
        .ok_or_else(|| format!("{value:?} is too long"))
}

/// A size in bytes: plain bytes (`1048576`) or a number with a unit, case-insensitive: `B`, `KiB`/`KB`,
/// `MiB`/`MB`, `GiB`/`GB` (the migration guide's form). `KB`, `MB` and `GB` are binary, as in Spring's
/// `DataSize`, so a value copied from a Spring setting keeps its meaning (`10MB` is 10 485 760 bytes).
pub fn parse_size(value: &str) -> Result<u64, String> {
    let value = value.trim();
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("{value:?} is not a size such as 10MiB, 1000MB or 1048576"))?;
    let multiplier: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" | "kb" => 1 << 10,
        "mib" | "mb" => 1 << 20,
        "gib" | "gb" => 1 << 30,
        _ => {
            return Err(format!(
                "{value:?} has an unknown unit: B, KiB, MiB, GiB (KB, MB, GB alike)"
            ));
        }
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| format!("{value:?} is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn images_keep_their_resolution_unless_a_maximum_is_set() {
        assert_eq!(config(&[]).unwrap().pdf_max_image_dpi, None);
        assert_eq!(
            config(&[("DOCX_TO_PDF_PDF_MAX_IMAGE_DPI", "300")])
                .unwrap()
                .pdf_max_image_dpi,
            Some(300)
        );
        for bad in ["0", "-1", "high"] {
            let e = config(&[("DOCX_TO_PDF_PDF_MAX_IMAGE_DPI", bad)]).unwrap_err();
            assert_eq!(e.variable, "DOCX_TO_PDF_PDF_MAX_IMAGE_DPI");
        }
    }

    #[test]
    fn the_probe_is_off_unless_its_port_is_set_and_never_on_the_conversion_port() {
        assert_eq!(config(&[]).unwrap().probe_port, None);
        assert_eq!(
            config(&[("DOCX_TO_PDF_PROBE_PORT", "9090")])
                .unwrap()
                .probe_port,
            Some(9090)
        );
        let e = config(&[("DOCX_TO_PDF_PROBE_PORT", "8080")]).unwrap_err();
        assert_eq!(e.variable, "DOCX_TO_PDF_PROBE_PORT");
        let e = config(&[
            ("DOCX_TO_PDF_PORT", "9000"),
            ("DOCX_TO_PDF_PROBE_PORT", "9000"),
        ])
        .unwrap_err();
        assert_eq!(e.variable, "DOCX_TO_PDF_PROBE_PORT");
        assert!(config(&[("DOCX_TO_PDF_PROBE_PORT", "nope")]).is_err());
    }

    #[test]
    fn the_port_is_8080_when_nothing_sets_it() {
        assert_eq!(config(&[]).unwrap().port, 8080);
    }

    #[test]
    fn docx_to_pdf_port_sets_the_port() {
        assert_eq!(config(&[("DOCX_TO_PDF_PORT", "9000")]).unwrap().port, 9000);
    }

    #[test]
    fn upstreams_server_port_still_sets_the_port() {
        assert_eq!(config(&[("SERVER_PORT", "9001")]).unwrap().port, 9001);
    }

    #[test]
    fn ours_wins_when_both_ports_are_set() {
        let c = config(&[("SERVER_PORT", "9001"), ("DOCX_TO_PDF_PORT", "9000")]).unwrap();
        assert_eq!(c.port, 9000);
    }

    #[test]
    fn an_empty_value_counts_as_unset() {
        let c = config(&[("DOCX_TO_PDF_PORT", ""), ("SERVER_PORT", "9001")]).unwrap();
        assert_eq!(c.port, 9001);
    }

    #[test]
    fn a_port_that_is_not_a_number_names_its_variable() {
        let err = config(&[("SERVER_PORT", "http")]).unwrap_err();
        assert_eq!(err.variable, "SERVER_PORT");
        assert!(config(&[("DOCX_TO_PDF_PORT", "0")]).is_err());
        assert!(config(&[("DOCX_TO_PDF_PORT", "70000")]).is_err());
    }

    /// Kubernetes injects `<SERVICE>_PORT=tcp://<clusterIP>:<port>` for every Service in the namespace, so a Service
    /// named `docx-to-pdf` sets `DOCX_TO_PDF_PORT` that way.
    #[test]
    fn a_kubernetes_service_link_in_the_port_is_ignored_with_a_note() {
        let link = "tcp://10.96.96.218:8080";
        let c = config(&[("DOCX_TO_PDF_PORT", link)]).unwrap();
        assert_eq!(c.port, 8080);
        assert_eq!(c.notes.len(), 1, "{:?}", c.notes);
        assert!(c.notes[0].contains("DOCX_TO_PDF_PORT"), "{:?}", c.notes);
        assert!(c.notes[0].contains("service link"), "{:?}", c.notes);
        assert!(c.notes[0].contains(link), "{:?}", c.notes);

        let c = config(&[("DOCX_TO_PDF_PORT", link), ("SERVER_PORT", "9001")]).unwrap();
        assert_eq!(c.port, 9001);
        assert_eq!(c.notes.len(), 1, "{:?}", c.notes);

        let c = config(&[("DOCX_TO_PDF_PORT", "tcp://[fd00::1]:8080")]).unwrap();
        assert_eq!(c.port, 8080);
    }

    #[test]
    fn a_kubernetes_service_link_in_the_probe_port_leaves_the_probe_off() {
        let c = config(&[("DOCX_TO_PDF_PROBE_PORT", "tcp://10.96.0.5:9090")]).unwrap();
        assert_eq!(c.probe_port, None);
        assert_eq!(c.notes.len(), 1, "{:?}", c.notes);
        assert!(
            c.notes[0].contains("DOCX_TO_PDF_PROBE_PORT"),
            "{:?}",
            c.notes
        );
        assert!(c.notes[0].contains("service link"), "{:?}", c.notes);
    }

    #[test]
    fn nothing_else_passes_for_a_service_link() {
        for variable in ["DOCX_TO_PDF_PORT", "DOCX_TO_PDF_PROBE_PORT"] {
            for bad in [
                "http",
                "0",
                "70000",
                "tcp://x",
                "tcp://10.96.0.5",
                "tcp://10.96.0.5:http",
                "http://10.96.0.5:8080",
                "tcp://host.example:8080",
            ] {
                let e = config(&[(variable, bad)]).unwrap_err();
                assert_eq!(e.variable, variable, "{bad}");
            }
        }
        assert_eq!(config(&[]).unwrap().notes, [] as [String; 0]);
    }

    #[test]
    fn the_shutdown_grace_takes_a_unit() {
        assert_eq!(
            config(&[]).unwrap().shutdown_grace,
            Duration::from_secs(180)
        );
        let c = config(&[("DOCX_TO_PDF_SHUTDOWN_GRACE", "2m")]).unwrap();
        assert_eq!(c.shutdown_grace, Duration::from_secs(120));
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        assert!(config(&[("DOCX_TO_PDF_SHUTDOWN_GRACE", "30")]).is_err());
    }

    #[test]
    fn the_max_document_size_is_10_mib_when_nothing_sets_it() {
        assert_eq!(config(&[]).unwrap().max_document_size, 10 * 1024 * 1024);
    }

    #[test]
    fn the_max_document_size_takes_a_unit_or_plain_bytes() {
        let size =
            |v: &str| config(&[("DOCX_TO_PDF_MAX_DOCUMENT_SIZE", v)]).map(|c| c.max_document_size);
        assert_eq!(size("10MiB"), Ok(10 * 1024 * 1024));
        assert_eq!(size("2GiB"), Ok(2 * 1024 * 1024 * 1024));
        assert_eq!(size("512KiB"), Ok(512 * 1024));
        assert_eq!(size("12345"), Ok(12345));
        assert_eq!(size("12345B"), Ok(12345));
        assert_eq!(size(" 3 mib "), Ok(3 * 1024 * 1024));
    }

    #[test]
    fn a_spring_size_copied_over_means_what_it_meant_to_spring() {
        // Spring's DataSize: `MB` is 1024 * 1024 bytes, so moalhaddar/docx-to-pdf's `10MB` is 10 485 760 bytes
        // and an upstream deployment's `1000MB` keeps its value.
        let size = |v: &str| {
            config(&[("DOCX_TO_PDF_MAX_DOCUMENT_SIZE", v)])
                .unwrap()
                .max_document_size
        };
        assert_eq!(size("10MB"), 10_485_760);
        assert_eq!(size("1000MB"), 1000 * 1024 * 1024);
        assert_eq!(size("1KB"), 1024);
        assert_eq!(size("1GB"), 1024 * 1024 * 1024);
    }

    #[test]
    fn a_max_document_size_that_cannot_be_used_names_its_variable() {
        for value in ["0", "-1", "ten", "10XB", "MB", "99999999999999999999"] {
            let err = config(&[("DOCX_TO_PDF_MAX_DOCUMENT_SIZE", value)]).unwrap_err();
            assert_eq!(err.variable, "DOCX_TO_PDF_MAX_DOCUMENT_SIZE", "{value}");
        }
    }

    #[test]
    fn springs_multipart_settings_are_not_read() {
        // Spring's multipart settings are not read (whether to alias one is an open decision of the
        // user's).
        let c = config(&[("SPRING_SERVLET_MULTIPART_MAX_FILE_SIZE", "1000MB")]).unwrap();
        assert_eq!(c.max_document_size, DEFAULT_MAX_DOCUMENT_SIZE);
    }

    #[test]
    fn the_picture_bitmap_threshold_is_256_mib_unless_set() {
        let mib = 1024 * 1024;
        assert_eq!(config(&[]).unwrap().picture_max_bitmap, 256 * mib);
        let set = |v: &str| config(&[("DOCX_TO_PDF_PICTURE_MAX_BITMAP", v)]);
        assert_eq!(set("1GiB").unwrap().picture_max_bitmap, 1024 * mib);
        assert_eq!(
            set("4GiB").unwrap().picture_max_bitmap,
            4096 * mib,
            "kept as set; LibreOffice's bound caps it where it is used"
        );
        for bad in ["0", "lots", "-1"] {
            let e = set(bad).unwrap_err();
            assert_eq!(e.variable, "DOCX_TO_PDF_PICTURE_MAX_BITMAP", "{bad}");
        }
    }

    #[test]
    fn the_inflated_size_limit_follows_the_document_limit_unless_set() {
        let mib = 1024 * 1024;
        assert_eq!(config(&[]).unwrap().max_inflated_size, 256 * mib);
        assert_eq!(DEFAULT_MAX_INFLATED_SIZE, 256 * mib);
        let raised = config(&[("DOCX_TO_PDF_MAX_DOCUMENT_SIZE", "1200MiB")]).unwrap();
        assert_eq!(
            raised.max_inflated_size,
            3600 * mib,
            "3 × the document limit"
        );
        let set = |v: &str| config(&[("DOCX_TO_PDF_MAX_INFLATED_SIZE", v)]);
        assert_eq!(set("1GiB").unwrap().max_inflated_size, 1024 * mib);
        assert_eq!(set("0").unwrap().max_inflated_size, 0, "0 = no limit");
        assert_eq!(
            set("10MiB").unwrap().max_inflated_size,
            10 * mib,
            "at the document limit"
        );
        for bad in ["9MiB", "lots", "-1"] {
            let e = set(bad).unwrap_err();
            assert_eq!(e.variable, "DOCX_TO_PDF_MAX_INFLATED_SIZE", "{bad}");
        }
        let e = config(&[
            ("DOCX_TO_PDF_MAX_DOCUMENT_SIZE", "300MiB"),
            ("DOCX_TO_PDF_MAX_INFLATED_SIZE", "256MiB"),
        ])
        .unwrap_err();
        assert!(
            e.reason.contains("below DOCX_TO_PDF_MAX_DOCUMENT_SIZE"),
            "{e}"
        );
    }

    #[test]
    fn the_pool_size_is_1_when_nothing_sets_it() {
        assert_eq!(config(&[]).unwrap().pool_size, 1);
    }

    #[test]
    fn docx_to_pdf_pool_size_sets_the_pool_size() {
        assert_eq!(
            config(&[("DOCX_TO_PDF_POOL_SIZE", "4")]).unwrap().pool_size,
            4
        );
    }

    #[test]
    fn upstreams_pool_size_still_sets_the_pool_size() {
        assert_eq!(config(&[("POOL_SIZE", "3")]).unwrap().pool_size, 3);
    }

    #[test]
    fn upstreams_dotted_pool_size_still_sets_the_pool_size() {
        // the infrastructure channels and a client backend's Testcontainers set the literal name `pool.size`.
        assert_eq!(config(&[("pool.size", "2")]).unwrap().pool_size, 2);
    }

    #[test]
    fn ours_wins_over_both_upstream_pool_sizes() {
        let c = config(&[
            ("POOL_SIZE", "3"),
            ("pool.size", "2"),
            ("DOCX_TO_PDF_POOL_SIZE", "5"),
        ])
        .unwrap();
        assert_eq!(c.pool_size, 5);
        // Between upstream's two, `POOL_SIZE` wins, as in moalhaddar/docx-to-pdf (`POOL_SIZE=2`, `pool.size=3`: two
        // instances).
        let c = config(&[("POOL_SIZE", "2"), ("pool.size", "3")]).unwrap();
        assert_eq!(c.pool_size, 2);
    }

    #[test]
    fn a_pool_size_that_cannot_be_used_names_its_variable() {
        for value in ["0", "-1", "two", "1.5"] {
            assert_eq!(
                config(&[("pool.size", value)]).unwrap_err().variable,
                "pool.size",
                "{value}"
            );
        }
    }

    #[test]
    fn the_pool_waits_and_starts_have_their_measured_defaults() {
        let c = config(&[]).unwrap();
        assert_eq!(c.pool_wait_timeout, Duration::from_secs(60));
        assert_eq!(c.pool_max_waiting, 16);
        assert_eq!(c.instance_start_timeout, Duration::from_secs(30));
        assert_eq!(c.profiles_dir, std::env::temp_dir());
    }

    #[test]
    fn the_timeout_and_recycling_have_their_measured_defaults() {
        let c = config(&[]).unwrap();
        assert_eq!(c.conversion_timeout, Duration::from_secs(180));
        assert_eq!(c.instance_max_conversions, 200);
        assert_eq!(c.instance_max_memory, 1024 * 1024 * 1024);
        assert_eq!(c.instance_hard_memory, 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn the_hard_memory_limit_is_set_by_its_variable_and_stays_above_the_ceiling() {
        let c = config(&[("DOCX_TO_PDF_INSTANCE_HARD_MEMORY", "1536MiB")]).unwrap();
        assert_eq!(c.instance_hard_memory, 1536 * 1024 * 1024);
        // `0` turns it off, whatever the ceiling.
        let c = config(&[("DOCX_TO_PDF_INSTANCE_HARD_MEMORY", "0")]).unwrap();
        assert_eq!(c.instance_hard_memory, 0);
        // With no ceiling, any hard limit goes.
        let c = config(&[
            ("DOCX_TO_PDF_INSTANCE_MAX_MEMORY", "0"),
            ("DOCX_TO_PDF_INSTANCE_HARD_MEMORY", "256MiB"),
        ])
        .unwrap();
        assert_eq!(c.instance_hard_memory, 256 * 1024 * 1024);
        // At or below the ceiling, an instance kept between conversions could be killed at the next one's start.
        for vars in [
            &[("DOCX_TO_PDF_INSTANCE_HARD_MEMORY", "1GiB")][..],
            &[("DOCX_TO_PDF_INSTANCE_HARD_MEMORY", "512MiB")][..],
            &[("DOCX_TO_PDF_INSTANCE_MAX_MEMORY", "3GiB")][..],
            &[("DOCX_TO_PDF_INSTANCE_HARD_MEMORY", "2X")][..],
        ] {
            assert_eq!(
                config(vars).unwrap_err().variable,
                "DOCX_TO_PDF_INSTANCE_HARD_MEMORY",
                "{vars:?}"
            );
        }
    }

    #[test]
    fn the_timeout_and_recycling_are_set_by_our_variables() {
        let c = config(&[
            ("DOCX_TO_PDF_CONVERSION_TIMEOUT", "90s"),
            ("DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS", "10"),
            ("DOCX_TO_PDF_INSTANCE_MAX_MEMORY", "512MiB"),
        ])
        .unwrap();
        assert_eq!(c.conversion_timeout, Duration::from_secs(90));
        assert_eq!(c.instance_max_conversions, 10);
        assert_eq!(c.instance_max_memory, 512 * 1024 * 1024);
        // `0` turns recycling off: never by count, no memory ceiling.
        let c = config(&[
            ("DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS", "0"),
            ("DOCX_TO_PDF_INSTANCE_MAX_MEMORY", "0"),
        ])
        .unwrap();
        assert_eq!(c.instance_max_conversions, 0);
        assert_eq!(c.instance_max_memory, 0);
        for (variable, value) in [
            ("DOCX_TO_PDF_CONVERSION_TIMEOUT", "0s"),
            ("DOCX_TO_PDF_CONVERSION_TIMEOUT", "300"),
            ("DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS", "-1"),
            ("DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS", "ten"),
            ("DOCX_TO_PDF_INSTANCE_MAX_MEMORY", "1XB"),
        ] {
            assert_eq!(config(&[(variable, value)]).unwrap_err().variable, variable);
        }
    }

    #[test]
    fn the_idle_limit_is_120_s_unless_set_and_above_zero() {
        assert_eq!(config(&[]).unwrap().idle_timeout, Duration::from_secs(120));
        let c = config(&[("DOCX_TO_PDF_IDLE_TIMEOUT", "300ms")]).unwrap();
        assert_eq!(c.idle_timeout, Duration::from_millis(300));
        // Upstream's Tomcat setting is not read: only the pool size and the port carry over.
        let c = config(&[("SERVER_TOMCAT_CONNECTION_TIMEOUT", "30")]).unwrap();
        assert_eq!(c.idle_timeout, Duration::from_secs(120));
        for value in ["0s", "120", "soon"] {
            assert_eq!(
                config(&[("DOCX_TO_PDF_IDLE_TIMEOUT", value)])
                    .unwrap_err()
                    .variable,
                "DOCX_TO_PDF_IDLE_TIMEOUT"
            );
        }
    }

    #[test]
    fn the_shutdown_grace_follows_the_conversion_timeout_unless_set() {
        // The migration guide: the grace's default is the conversion timeout's, so an in-flight conversion
        // may run to its own limit.
        let c = config(&[("DOCX_TO_PDF_CONVERSION_TIMEOUT", "90s")]).unwrap();
        assert_eq!(c.shutdown_grace, Duration::from_secs(90));
        let c = config(&[
            ("DOCX_TO_PDF_CONVERSION_TIMEOUT", "90s"),
            ("DOCX_TO_PDF_SHUTDOWN_GRACE", "10s"),
        ])
        .unwrap();
        assert_eq!(c.shutdown_grace, Duration::from_secs(10));
    }

    #[test]
    fn the_pool_waits_and_starts_are_set_by_our_variables() {
        let c = config(&[
            ("DOCX_TO_PDF_POOL_WAIT_TIMEOUT", "500ms"),
            ("DOCX_TO_PDF_POOL_MAX_WAITING", "0"),
            ("DOCX_TO_PDF_INSTANCE_START_TIMEOUT", "2m"),
        ])
        .unwrap();
        assert_eq!(c.pool_wait_timeout, Duration::from_millis(500));
        assert_eq!(c.pool_max_waiting, 0);
        assert_eq!(c.instance_start_timeout, Duration::from_secs(120));
        for (variable, value) in [
            ("DOCX_TO_PDF_POOL_WAIT_TIMEOUT", "60"),
            ("DOCX_TO_PDF_POOL_MAX_WAITING", "-1"),
            ("DOCX_TO_PDF_INSTANCE_START_TIMEOUT", "0s"),
        ] {
            assert_eq!(config(&[(variable, value)]).unwrap_err().variable, variable);
        }
    }
}
