//! The docx-to-pdf service: configuration, telemetry, the server, graceful shutdown; and, with the argument
//! `instance`, one LibreOffice instance: a child process of the server's own binary, so a crash or a kill takes
//! one instance, never the server.

use std::process::ExitCode;

use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::config::Config;
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::instance::{self, INSTANCE_MODE};
use docx_to_pdf::pool::{InstanceLimits, Pool, PoolSettings};
use docx_to_pdf::probe::{self, ProbeState};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::server::{self, Shutdown};
use docx_to_pdf::telemetry::{self, Telemetry};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// At shutdown, how long each idle instance gets to end by itself after its stdin closes; then it is killed.
const INSTANCE_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn main() -> ExitCode {
    // Instance mode: one LibreOffice instance, started by the server. It hosts
    // LibreOfficeKit and needs neither the runtime nor the service's configuration.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(INSTANCE_MODE) {
        instance::child::run(&args[1..]);
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(service()),
        Err(e) => {
            eprintln!("docx-to-pdf: starting the runtime: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn service() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("docx-to-pdf: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Inside the runtime: a gRPC exporter needs one.
    let telemetry = match Telemetry::from_env() {
        Ok(telemetry) => telemetry,
        Err(e) => {
            eprintln!("docx-to-pdf: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = telemetry::install_subscriber(&telemetry) {
        eprintln!("docx-to-pdf: {e}");
        return ExitCode::FAILURE;
    }
    for note in config.notes.iter().chain(telemetry.notes()) {
        tracing::warn!("{note}");
    }

    let code = run(&config, &telemetry).await;

    let telemetry = std::sync::Arc::new(telemetry);
    let flushing = telemetry.clone();
    match tokio::task::spawn_blocking(move || flushing.shutdown()).await {
        Ok(errors) => {
            for e in errors {
                eprintln!("docx-to-pdf: flushing telemetry: {e}");
            }
        }
        Err(e) => eprintln!("docx-to-pdf: flushing telemetry: {e}"),
    }
    code
}

/// Ends the probe's listener with `run`, on every path.
struct AbortOnDrop<T>(Option<tokio::task::JoinHandle<T>>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

async fn run(config: &Config, telemetry: &Telemetry) -> ExitCode {
    let listener = match TcpListener::bind(("0.0.0.0", config.port)).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(port = config.port, error = %e, "cannot listen");
            return ExitCode::FAILURE;
        }
    };
    let readiness = Readiness::new();
    let state = AppState::from_config(readiness.clone(), telemetry, config);
    let app = router(state.clone());
    // The platform probe: only with its own port set, never on the conversion port, since it holds every
    // instance for minutes and would be a denial of service there.
    let probing = match config.probe_port {
        None => None,
        Some(port) => match TcpListener::bind(("0.0.0.0", port)).await {
            Ok(listener) => {
                tracing::info!(port, "the platform probe listens: POST /probe");
                let probe = probe::router(ProbeState::new(state.pool.clone(), &state.upload));
                Some(tokio::spawn(async move {
                    axum::serve(listener, probe)
                        .with_graceful_shutdown(server::termination())
                        .await
                }))
            }
            Err(e) => {
                tracing::error!(port, error = %e, "cannot listen for the probe");
                return ExitCode::FAILURE;
            }
        },
    };
    let _stop_probing = AbortOnDrop(probing);
    tracing::info!(
        port = config.port,
        otlp = %telemetry.otlp(),
        version = env!("CARGO_PKG_VERSION"),
        "listening"
    );
    let launcher = match Launcher::current_exe(&config.profiles_dir) {
        Ok(launcher) => launcher.with_max_image_dpi(config.pdf_max_image_dpi),
        Err(e) => {
            tracing::error!(error = %e, "cannot find this binary to start instances");
            return ExitCode::FAILURE;
        }
    };

    // Serve while the pool boots: `/livez` answers, `/readyz` is 503 until the pool marks it ready
    // and a Service sends it nothing. An instance that cannot start fails the boot, which ends serving.
    let (boot_failed, on_boot_failed) = oneshot::channel::<()>();
    let signal = async move {
        tokio::select! {
            () = server::termination() => {}
            _ = on_boot_failed => {}
        }
    };
    let mut serving = tokio::spawn(server::serve_configured(
        listener,
        app,
        readiness.clone(),
        signal,
        config,
    ));
    let meter = telemetry.meter();
    let booting = Pool::start_with_limits(
        launcher,
        PoolSettings::from_config(config),
        InstanceLimits::from_config(config),
        readiness,
        &meter,
    );
    let (pool, served) = tokio::select! {
        started = booting => match started {
            Ok(pool) => {
                state.set_pool(pool.clone());
                (Some(pool), (&mut serving).await)
            }
            Err(e) => {
                tracing::error!(error = %e, "the pool could not start");
                let _ = boot_failed.send(());
                let _ = (&mut serving).await;
                return ExitCode::FAILURE;
            }
        },
        // Stopped while booting: dropping the boot kills the instances it started.
        served = &mut serving => (None, served),
    };
    if let Some(pool) = &pool {
        pool.shutdown(INSTANCE_STOP_TIMEOUT).await;
    }

    match served.unwrap_or_else(|e| Err(std::io::Error::other(e))) {
        Ok(Shutdown::Drained) => {
            tracing::info!("stopped: every in-flight request finished");
            ExitCode::SUCCESS
        }
        Ok(Shutdown::GraceExceeded) => {
            tracing::warn!("stopped: the grace period ran out");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "serving failed");
            ExitCode::FAILURE
        }
    }
}
