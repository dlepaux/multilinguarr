//! multilinguarr binary entry point.
//!
//! Usage:
//!   `multilinguarr`          — start the server
//!   `multilinguarr openapi`  — dump `OpenAPI` spec to stdout and exit

use std::process::ExitCode;

use multilinguarr::app;
use multilinguarr::config::Bootstrap;

fn main() -> ExitCode {
    // Subcommands (no clap needed — one optional arg)
    if let Some(cmd) = std::env::args().nth(1) {
        return match cmd.as_str() {
            "openapi" => {
                print!("{}", multilinguarr::api::openapi::spec_json());
                ExitCode::SUCCESS
            }
            other => {
                eprintln!("unknown command: {other}");
                eprintln!("usage: multilinguarr [openapi]");
                ExitCode::from(1)
            }
        };
    }

    let bootstrap = match Bootstrap::from_env() {
        Ok(b) => b,
        Err(err) => {
            eprintln!("fatal: {err}");
            return ExitCode::from(1);
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(&bootstrap.log_level)
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    rt.block_on(async {
        let server = match app::build(bootstrap).await {
            Ok(s) => s,
            Err(err) => {
                tracing::error!(%err, "startup failed");
                return ExitCode::from(1);
            }
        };

        let cancel = server.cancel.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = sigterm.recv() => {},
            }
            tracing::info!("shutdown signal received");
            cancel.cancel();
        });

        if let Err(err) = app::run(server).await {
            tracing::error!(%err, "server error");
            return ExitCode::from(1);
        }

        ExitCode::SUCCESS
    })
}
