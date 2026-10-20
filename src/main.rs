// The engine is not called until the main loop exists (plan step 6). Remove this then.
#![allow(dead_code)]

mod core;
mod logging;
mod storage;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;

use anyhow::{Context, Result, bail};

use crate::core::config::{Config, Secrets};

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

/// Paths of the files in the data folder.
struct DataDir(PathBuf);

impl DataDir {
    fn default_path() -> PathBuf {
        let base = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
        PathBuf::from(base).join("ScreenTime")
    }

    fn config(&self) -> PathBuf {
        self.0.join("config.json")
    }

    fn secrets(&self) -> PathBuf {
        self.0.join("secrets.json")
    }

    fn state(&self) -> PathBuf {
        self.0.join("state.json")
    }

    fn logs(&self) -> PathBuf {
        self.0.join("logs")
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

    let (cfg, _secrets) = load_checked(&data)?;
    tracing::info!(pc = %cfg.pc_name, children = cfg.children.len(), parents = cfg.parents.len(), "config loaded");
    let state = storage::load_state(&data.state());

    let (stop_tx, stop_rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = stop_tx.send(());
    })
    .context("cannot set the Ctrl+C handler")?;
    tracing::info!("running, press Ctrl+C to stop");

    // The main loop comes in plan step 6. Until then, wait for Ctrl+C.
    let _ = stop_rx.recv();

    storage::save_json(&data.state(), &state)?;
    tracing::info!("stopped, state saved");
    Ok(ExitCode::SUCCESS)
}

/// Loads `secrets.json` (if there is one) and `config.json`, and checks the config.
fn load_checked(data: &DataDir) -> Result<(Config, Option<Secrets>)> {
    let secrets = if data.secrets().exists() {
        Some(Secrets::load(&data.secrets())?)
    } else {
        tracing::warn!(path = %data.secrets().display(), "no secrets file, Telegram and config updates are off");
        None
    };
    let cfg = Config::load(&data.config())?;
    let errors = cfg.validate(secrets.as_ref().map(|s| s.pc_name.as_str()));
    if !errors.is_empty() {
        bail!("config has errors:\n  {}", errors.join("\n  "));
    }
    Ok((cfg, secrets))
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
