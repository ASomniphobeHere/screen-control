mod app;
mod core;
mod logging;
mod storage;
mod win;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;

use anyhow::{Context, Result, bail};

use crate::app::{DataDir, Event};
use crate::core::config::Config;

const USAGE: &str = "\
Usage:
  screen-control service                          run as a Windows service (started by Windows)
  screen-control console [--data-dir DIR]         run in this terminal, for testing
  screen-control check-config FILE [--pc-name NAME]  check a config file";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("service") => run_service(),
        Some("console") => run_console(&args[1..]),
        Some("check-config") => check_config(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "stopped with an error");
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Gets the value after `--name`, if given.
fn option<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => match args.get(i + 1) {
            Some(value) => Ok(Some(value)),
            None => bail!("{name} needs a value"),
        },
    }
}

fn run_service() -> Result<ExitCode> {
    let data = DataDir(DataDir::default_path());
    let _guard = logging::init_file(&data.logs())?;
    bail!("service mode is not built yet (plan step 7)")
}

fn run_console(args: &[String]) -> Result<ExitCode> {
    logging::init_console();
    let data = DataDir(match option(args, "--data-dir")? {
        Some(dir) => PathBuf::from(dir),
        None => DataDir::default_path(),
    });
    tracing::info!(data_dir = %data.0.display(), "starting in console mode");

    let (cfg, _secrets) = app::load_checked(&data)?;

    let (events, rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = events.send(Event::Stop);
    })
    .context("cannot set the Ctrl+C handler")?;
    tracing::info!("press Ctrl+C to stop");

    app::run(&data, cfg, rx)?;
    Ok(ExitCode::SUCCESS)
}

fn check_config(args: &[String]) -> Result<ExitCode> {
    let Some(file) = args.first().filter(|a| !a.starts_with("--")) else {
        bail!("give the config file to check\n\n{USAGE}");
    };
    let pc_name = option(args, "--pc-name")?;
    let cfg = Config::load(Path::new(file))?;
    let errors = cfg.validate(pc_name);
    if errors.is_empty() {
        println!(
            "OK: {} children, {} parents, PC \"{}\"",
            cfg.children.len(),
            cfg.parents.len(),
            cfg.pc_name
        );
        Ok(ExitCode::SUCCESS)
    } else {
        println!("{} problem(s) found:", errors.len());
        for e in &errors {
            println!("  - {e}");
        }
        Ok(ExitCode::FAILURE)
    }
}
