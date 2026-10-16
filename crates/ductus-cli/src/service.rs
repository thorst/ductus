//! `ductus run --service`: the engine as a Windows service (the installer registers it).
//!
//! The service manager starts the process; this connects to it, runs the engine like `ductus
//! run`, and turns Stop, Shutdown and Preshutdown into the clean stop a signal asks for on Linux.
//! While starting or stopping it reports progress, so the manager waits instead of deciding the
//! service hung (migrations on a big database, or a stop finishing the messages in hand). Logs go
//! to files, since a service has no console.

use crate::RunArgs;
use anyhow::{Context, Result};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle};
use windows_service::{define_windows_service, service_dispatcher};

const NAME: &str = "Ductus";
/// How long a start or stop may go without a progress report before the manager gives up on us.
const PROGRESS: Duration = Duration::from_secs(30);

/// Handed to the service's entry point, which the manager calls with no arguments of ours.
static ARGS: Mutex<Option<RunArgs>> = Mutex::new(None);

const STARTING: u8 = 0;
const RUNNING: u8 = 1;
const STOPPING: u8 = 2;
const DONE: u8 = 3;

/// Connect to the service manager and serve until it stops us.
pub fn run(args: RunArgs) -> Result<()> {
    let _logs = log_to_files()?;
    *ARGS.lock().unwrap() = Some(args);
    service_dispatcher::start(NAME, ffi_service_main).context(
        "connecting to the service manager: `--service` is for the Windows service itself; \
         in a terminal use `ductus run` (or `ductus dev`)",
    )
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = serve() {
        tracing::error!(error = %format!("{e:#}"), "the service stopped on an error");
    }
}

fn serve() -> Result<()> {
    let args = ARGS.lock().unwrap().take().context("the service started without its settings")?;
    let stop_timeout = args.stop_timeout;
    let stop = Arc::new(Notify::new());
    let phase = Arc::new(AtomicU8::new(STARTING));
    let checkpoint = Arc::new(AtomicU32::new(0));

    // The handler runs on a thread of the manager's; it only reports and wakes the engine.
    let handle_cell: Arc<Mutex<Option<ServiceStatusHandle>>> = Arc::default();
    let handler = {
        let (stop, phase, checkpoint, handle) = (stop.clone(), phase.clone(), checkpoint.clone(), handle_cell.clone());
        move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                if phase.swap(STOPPING, Ordering::SeqCst) != STOPPING {
                    tracing::info!(?control, "the service manager asked the service to stop");
                    if let Some(h) = *handle.lock().unwrap() {
                        let wait = stop_timeout + Duration::from_secs(15);
                        report(&h, ServiceState::StopPending, &checkpoint, wait, ServiceExitCode::Win32(0));
                    }
                    stop.notify_one();
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    let status = service_control_handler::register(NAME, handler).context("registering with the service manager")?;
    *handle_cell.lock().unwrap() = Some(status);
    report(&status, ServiceState::StartPending, &checkpoint, PROGRESS, ServiceExitCode::Win32(0));

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    // While starting or stopping, show the manager that we're alive.
    let ticker = {
        let (phase, checkpoint) = (phase.clone(), checkpoint.clone());
        runtime.spawn(async move {
            loop {
                tokio::time::sleep(PROGRESS / 3).await;
                match phase.load(Ordering::SeqCst) {
                    STARTING => {
                        report(&status, ServiceState::StartPending, &checkpoint, PROGRESS, ServiceExitCode::Win32(0))
                    }
                    STOPPING => {
                        report(&status, ServiceState::StopPending, &checkpoint, PROGRESS, ServiceExitCode::Win32(0))
                    }
                    DONE => break,
                    _ => {}
                }
            }
        })
    };

    let result = runtime.block_on(async {
        let (root, database_url, mut options) = args.parts()?;
        options.stop_request = Some(stop.clone());
        let started_phase = phase.clone();
        options.on_started = Some(Box::new(move || {
            // A stop asked for during startup stays a stop.
            if started_phase.compare_exchange(STARTING, RUNNING, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                running(&status);
            }
        }));
        ductus_runtime::run(&root, &database_url, options).await
    });

    phase.store(DONE, Ordering::SeqCst);
    ticker.abort();
    let exit = match &result {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "the engine ended on an error");
            ServiceExitCode::ServiceSpecific(1)
        }
    };
    report(&status, ServiceState::Stopped, &checkpoint, Duration::ZERO, exit);
    // The runtime is dropped after the final report, so a slow teardown can't make the manager wait.
    Ok(())
}

fn running(status: &ServiceStatusHandle) {
    set(
        status,
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN | ServiceControlAccept::PRESHUTDOWN,
        0,
        Duration::ZERO,
        ServiceExitCode::Win32(0),
    );
}

/// Tell the manager where we are. Pending states carry a growing checkpoint and how long to wait.
fn report(
    status: &ServiceStatusHandle,
    state: ServiceState,
    checkpoint: &AtomicU32,
    wait: Duration,
    exit: ServiceExitCode,
) {
    let pending = matches!(state, ServiceState::StartPending | ServiceState::StopPending);
    let n = if pending { checkpoint.fetch_add(1, Ordering::SeqCst) + 1 } else { 0 };
    set(status, state, ServiceControlAccept::empty(), n, wait, exit);
}

fn set(
    status: &ServiceStatusHandle,
    state: ServiceState,
    accept: ServiceControlAccept,
    checkpoint: u32,
    wait: Duration,
    exit: ServiceExitCode,
) {
    let result = status.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: exit,
        checkpoint,
        wait_hint: wait,
        process_id: None,
    });
    if let Err(e) = result {
        tracing::warn!(error = %e, ?state, "couldn't report the service's state to the service manager");
    }
}

/// Daily log files in `DUCTUS_LOG_DIR` (default `%ProgramData%\Ductus\logs`), the last 14 kept.
fn log_to_files() -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let dir = std::env::var_os("DUCTUS_LOG_DIR").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into()))
            .join("Ductus")
            .join("logs")
    });
    std::fs::create_dir_all(&dir).with_context(|| format!("creating the log folder {}", dir.display()))?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("ductus")
        .filename_suffix("log")
        .max_log_files(14)
        .build(&dir)
        .with_context(|| format!("opening the log in {}", dir.display()))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(writer)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn,rustls_platform_verifier=off".into()),
        )
        .init();
    // A panic in a service has no console to print to.
    std::panic::set_hook(Box::new(|info| tracing::error!("{info}")));
    Ok(guard)
}
