mod diff;
mod icons;
mod input;
mod markdown;
mod syntax;
mod tree;
mod ui;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use space::{agent::Worker, project::Project, store::Store};

fn main() {
    if let Err(error) = start() {
        eprintln!("space: {error:#}");
        std::process::exit(1);
    }
}

fn start() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = match args.next() {
        Some(arg) if arg == "--help" || arg == "-h" => {
            println!(
                "Usage: space [PROJECT_FOLDER]\n\nOpen the project in a GPUI window. The current folder is the default.\nSet GEMINI_API_KEY in the environment or .env to send messages to Google."
            );
            return Ok(());
        }
        Some(arg) if arg.to_string_lossy().starts_with('-') => {
            bail!("Usage: space [PROJECT_FOLDER]")
        }
        Some(arg) => PathBuf::from(arg),
        None => std::env::current_dir()?,
    };
    if args.next().is_some() {
        bail!("Usage: space [PROJECT_FOLDER]");
    }
    if let Err(error) = dotenvy::dotenv()
        && !error.not_found()
    {
        bail!("Cannot load .env; check file access and syntax");
    }
    let project = Project::open(&path)?;
    let store = Store::open(&project)?;
    let worker = Worker::spawn(project, store)?;
    let shutdown = worker.shutdown.clone();
    let commands = worker.commands;
    let events = worker.events;
    ui::run(commands, worker.sidebar_commands, events, shutdown.clone());
    shutdown.cancel();
    worker
        .thread
        .join()
        .map_err(|_| anyhow::anyhow!("Agent worker stopped unexpectedly"))?
        .context("Agent worker failed")
}
