mod app;
mod config_sync;
mod core;
mod helper;
mod helper_protocol;
mod logging;
mod service;
mod storage;
mod telegram;
mod win;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;

use anyhow::{Context, Result, bail};

use crate::app::{DataDir, Event};
use crate::core::config::Config;

const USAGE: &str = "\
Usage:
  screen-control service [--data-dir DIR]            run as a Windows service (started by Windows)
  screen-control console [--data-dir DIR]            run in this terminal, for testing
  screen-control check-config FILE [--pc-name NAME]  check a config file
  screen-control helper --print                      print the messages the service sends to
                                                     this session, for testing

The data folder is C:\\ProgramData\\ScreenTime unless --data-dir is given.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("service") => run_service(&args[1..]),
        Some("console") => run_console(&args[1..]),
        Some("check-config") => check_config(&args[1..]),
        Some("helper") => helper::run(&args[1..]),
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

fn data_dir(args: &[String]) -> Result<DataDir> {
    Ok(DataDir(match option(args, "--data-dir")? {
        Some(dir) => PathBuf::from(dir),
        None => DataDir::default_path(),
    }))
}

fn run_service(args: &[String]) -> Result<ExitCode> {
    let data = data_dir(args)?;
    let _guard = logging::init_file(&data.logs())?;
    tracing::info!(data_dir = %data.0.display(), version = env!("CARGO_PKG_VERSION"), "starting as a service");
    service::run(data)?;
    Ok(ExitCode::SUCCESS)
}

fn run_console(args: &[String]) -> Result<ExitCode> {
    logging::init_console();
    let data = data_dir(args)?;
    tracing::info!(data_dir = %data.0.display(), "starting in console mode");

    let (cfg, secrets) = app::load_checked(&data)?;

    let (events, rx) = mpsc::channel();
    let stop = events.clone();
    ctrlc::set_handler(move || {
        let _ = stop.send(Event::Stop);
    })
    .context("cannot set the Ctrl+C handler")?;
    tracing::info!("press Ctrl+C to stop");

    app::run(&data, cfg, secrets, events, rx)?;
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
