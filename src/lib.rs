//! docx-to-pdf: the function and interface of `moalhaddar/docx-to-pdf:2.1.0-13` in Rust, reliably and
//! without its bugs, with OpenTelemetry traces, logs and metrics.

pub mod allowlist;
pub mod app;
pub mod config;
pub mod conversion;
pub mod http_telemetry;
pub mod instance;
pub mod num;
pub mod pdf_equivalence;
pub mod pictures;
pub mod pool;
pub mod probe;
pub mod problem;
pub mod process_metrics;
pub mod readiness;
pub mod server;
pub mod sizing;
pub mod telemetry;
pub mod upload;
