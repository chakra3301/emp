//! `empd` — the EMP daemon.
//!
//! ```sh
//! empd serve                 # run the daemon (default)
//! empd import [pi hermes …]  # one-shot import without a daemon
//! empd doctor                # harness + source report as JSON
//! ```

mod attachments;
mod commands;
mod cron;
mod doctor;
mod drivers;
mod importing;
mod live;
mod mcp;
mod models;
mod phone;
mod rpc;
mod schedules;
mod server;
mod elsewhere;
mod locate;
mod servers;
mod state;
mod terminals;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use emp_protocol::ImportSourceKind;

#[derive(Parser)]
#[command(name = "empd", version, about = "EMP daemon")]
struct Cli {
    /// Data directory (default: ~/Library/Application Support/EMP, or $EMP_DATA_DIR).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon.
    Serve {
        /// WebSocket port on 127.0.0.1 (0 = pick a free port).
        #[arg(long, default_value_t = 0, env = "EMP_WS_PORT")]
        ws_port: u16,
        /// Skip the incremental import that runs at startup.
        #[arg(long)]
        no_initial_import: bool,
        /// The built web app (default: the app bundle's Resources/web, else apps/desktop/dist).
        #[arg(long, env = "EMP_WEB_DIR")]
        web_dir: Option<PathBuf>,
    },
    /// Import from the given sources (all when none) and exit.
    Import { sources: Vec<String> },
    /// Print the doctor report as JSON and exit.
    Doctor,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("EMP_LOG").unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(emp_core::paths::data_dir);
    let state = state::State::open(data_dir)?;

    match cli.command.unwrap_or(Command::Serve { ws_port: 0, no_initial_import: false, web_dir: None }) {
        Command::Serve { ws_port, no_initial_import, web_dir } => match server::serve(state, ws_port, !no_initial_import, phone::resolve_web_dir(web_dir)).await {
            Err(e) if e.downcast_ref::<server::AlreadyRunning>().is_some() => {
                tracing::info!("{e}; nothing to do");
                Ok(())
            }
            other => other,
        },
        Command::Import { sources } => {
            let kinds = parse_sources(&sources)?;
            let report = importing::run(&state, &kinds).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Doctor => {
            println!("{}", serde_json::to_string_pretty(&doctor::report(&state).await?)?);
            Ok(())
        }
    }
}

fn parse_sources(names: &[String]) -> Result<Vec<ImportSourceKind>> {
    names
        .iter()
        .map(|n| {
            ImportSourceKind::ALL
                .into_iter()
                .find(|k| k.as_str() == n)
                .ok_or_else(|| anyhow::anyhow!("unknown source {n:?} (pi, hermes, claude, codex, orion)"))
        })
        .collect()
}
