use std::{ffi::OsString, io::Write as _, process::ExitCode};

use devicerail_bundle_cli::{CliError, Command, CommandSummary, execute, parse_args};
use devicerail_core::{CancellationReason, ExecutionController};

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if matches!(arguments.as_slice(), [argument] if argument == "--version") {
        return match writeln!(
            std::io::stdout().lock(),
            "devicerail-bundle {}",
            env!("CARGO_PKG_VERSION")
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => report(CliError::SummaryWrite),
        };
    }
    match run(arguments).await {
        Ok(summary) => match write_summary(summary) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => report(error),
        },
        Err(error) => report(error),
    }
}

async fn run(arguments: Vec<OsString>) -> Result<CommandSummary, CliError> {
    let command = parse_args(arguments)?;
    run_interruptible(command).await
}

async fn run_interruptible(command: Command) -> Result<CommandSummary, CliError> {
    let (controller, control) = ExecutionController::new();
    let operation = execute(command, &control);
    tokio::pin!(operation);

    tokio::select! {
        result = &mut operation => result,
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|_| CliError::Signal)?;
            controller.cancel(CancellationReason::Requested);
            operation.await
        }
    }
}

fn write_summary(summary: CommandSummary) -> Result<(), CliError> {
    let mut bytes = serde_json::to_vec(&summary).map_err(|_| CliError::Summary)?;
    bytes.push(b'\n');
    std::io::stdout()
        .lock()
        .write_all(&bytes)
        .map_err(|_| CliError::SummaryWrite)
}

fn report(error: CliError) -> ExitCode {
    let code = if matches!(error, CliError::Usage) {
        ExitCode::from(2)
    } else {
        ExitCode::FAILURE
    };
    let _ = writeln!(std::io::stderr().lock(), "devicerail-bundle: {error}");
    code
}
