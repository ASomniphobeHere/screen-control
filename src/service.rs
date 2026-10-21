//! Run as a Windows service. Windows starts the program with the `service` command.

use std::ffi::OsString;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::app::{self, DataDir, Event};

pub const SERVICE_NAME: &str = "ScreenControl";

/// Set by `run` before Windows calls `service_main`, which cannot take arguments from us.
static DATA_DIR: OnceLock<DataDir> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

/// Hands control to Windows. Returns when the service has stopped.
pub fn run(data: DataDir) -> Result<()> {
    let _ = DATA_DIR.set(data);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .context("cannot start the service dispatcher (only Windows can start the service)")
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        tracing::error!(error = format!("{e:#}"), "service stopped with an error");
    }
}

fn run_service() -> Result<()> {
    let data = DATA_DIR.get().context("data folder not set")?;
    let (sender, rx) = mpsc::channel();
    let events = sender.clone();

    let status = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = events.send(Event::Stop);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(change) => {
            tracing::debug!(reason = ?change.reason, session = change.notification.session_id, "session event");
            let _ = events.send(Event::SessionChanged);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;

    let set = |state: ServiceState, accept: ServiceControlAccept, exit_code: ServiceExitCode| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code,
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        })
    };
    let accept = ServiceControlAccept::STOP
        | ServiceControlAccept::SHUTDOWN
        | ServiceControlAccept::SESSION_CHANGE;
    set(ServiceState::Running, accept, ServiceExitCode::Win32(0))?;
    tracing::info!("service started");

    let result =
        app::load_checked(data).and_then(|(cfg, secrets)| app::run(data, cfg, secrets, sender, rx));
    if let Err(e) = &result {
        tracing::error!(error = format!("{e:#}"), "stopped with an error");
    }
    // A non-zero exit code makes Windows run the recovery actions (restart the service),
    // when the failure actions flag is set (`sc failureflag ScreenControl 1`).
    let exit_code = if result.is_ok() {
        ServiceExitCode::Win32(0)
    } else {
        ServiceExitCode::ServiceSpecific(1)
    };
    set(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit_code,
    )?;
    tracing::info!("service stopped");
    Ok(())
}
