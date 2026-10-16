//! Argv, the harness client, and shared TUI composition.

#[path = "graphcoder/args.rs"]
mod args;
#[path = "graphcoder/view.rs"]
mod view;

use acyclic_harness::{
    OperationId, client,
    local::{LocalHarness, LocalHarnessConfig},
};
use acyclic_harness_providers::{Credentials, Registry};
use args::{Args, InputFormat};
use clap::Parser;
use std::{process::ExitCode, sync::Arc};

fn main() -> ExitCode {
    let args = Args::parse();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
        .and_then(|runtime| runtime.block_on(run(args)));
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("graphcoder: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<u8, Box<dyn std::error::Error>> {
    let credentials = args
        .credentials
        .as_deref()
        .map(Credentials::from_file)
        .transpose()?
        .unwrap_or_default();
    let mut registry = Registry::new(credentials)?;
    if let Some(path) = &args.catalog {
        registry.load_catalog(path)?;
    }
    let provider = Arc::new(registry.resolve(&args.model)?);
    let local = Arc::new(
        LocalHarness::open(LocalHarnessConfig::new(
            &args.workspace,
            (provider.model().clone(), provider),
        ))
        .await?,
    );
    let operation = args
        .operation
        .as_deref()
        .map(OperationId::parse)
        .transpose()?
        .unwrap_or_else(OperationId::new);
    let (mut connection, target) = client::connect_local(local.clone(), operation).await?;
    if let Some(prompt) = args.prompt {
        let command = client::local_prompt(&local, operation, &prompt).await?;
        return Ok(client::run_headless(
            &mut connection,
            command,
            target,
            &mut std::io::stdout().lock(),
        )
        .await?
        .exit_code());
    }
    if args.input_format == InputFormat::Jsonl {
        return Ok(client::run_jsonl(
            &mut connection,
            target,
            client::stdin_jsonl(),
            &mut std::io::stdout().lock(),
        )
        .await?
        .exit_code());
    }
    view::run(local, connection, target, args.inline).await?;
    Ok(0)
}

#[cfg(test)]
#[path = "graphcoder/tests.rs"]
mod tests;
