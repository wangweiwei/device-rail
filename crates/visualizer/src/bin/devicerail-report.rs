use std::{ffi::OsString, io::Write as _, path::PathBuf, process::ExitCode};

use devicerail_core::{CancellationReason, ExecutionController};
use devicerail_visualizer::report::{
    ReportError, ReportLimits, ReportSummary, export_static_report, validate_static_report,
};
use serde::Serialize;
use thiserror::Error;

const USAGE: &str = "usage: devicerail-report export --bundle DIR --output DIR\n       devicerail-report validate DIR";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Export { bundle: PathBuf, output: PathBuf },
    Validate { report: PathBuf },
}

#[derive(Debug, Error)]
enum CliError {
    #[error("{USAGE}")]
    Usage,
    #[error(transparent)]
    Report(#[from] ReportError),
    #[error("failed to install the interrupt handler")]
    Signal,
    #[error("failed to serialize the report summary")]
    Summary,
    #[error("failed to write the report summary")]
    SummaryWrite,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandSummary {
    ok: bool,
    operation: &'static str,
    #[serde(flatten)]
    report: ReportSummary,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await.and_then(write_summary) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let code = if matches!(error, CliError::Usage) {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            };
            let _ = writeln!(std::io::stderr().lock(), "devicerail-report: {error}");
            code
        }
    }
}

async fn run() -> Result<CommandSummary, CliError> {
    let command = parse_args(std::env::args_os().skip(1))?;
    let operation = match &command {
        Command::Export { .. } => "export",
        Command::Validate { .. } => "validate",
    };
    let (controller, control) = ExecutionController::new();
    let task = async {
        match command {
            Command::Export { bundle, output } => {
                export_static_report(bundle, output, ReportLimits::default(), &control).await
            }
            Command::Validate { report } => {
                validate_static_report(report, ReportLimits::default(), &control).await
            }
        }
    };
    tokio::pin!(task);
    let report = tokio::select! {
        result = &mut task => result?,
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|_| CliError::Signal)?;
            controller.cancel(CancellationReason::Requested);
            task.await?
        }
    };
    Ok(CommandSummary {
        ok: true,
        operation,
        report,
    })
}

fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, CliError> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [command, bundle_flag, bundle, output_flag, output]
            if command == "export" && bundle_flag == "--bundle" && output_flag == "--output" =>
        {
            if bundle.is_empty() || output.is_empty() {
                return Err(CliError::Usage);
            }
            Ok(Command::Export {
                bundle: PathBuf::from(bundle),
                output: PathBuf::from(output),
            })
        }
        [command, report] if command == "validate" && !report.is_empty() => Ok(Command::Validate {
            report: PathBuf::from(report),
        }),
        _ => Err(CliError::Usage),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_exact_export_and_validate_shapes() {
        assert_eq!(
            parse_args([
                "export".into(),
                "--bundle".into(),
                "bundle".into(),
                "--output".into(),
                "report".into(),
            ])
            .expect("export"),
            Command::Export {
                bundle: PathBuf::from("bundle"),
                output: PathBuf::from("report"),
            }
        );
        assert_eq!(
            parse_args(["validate".into(), "report".into()]).expect("validate"),
            Command::Validate {
                report: PathBuf::from("report")
            }
        );
        assert!(parse_args(["export".into(), "bundle".into()]).is_err());
        assert!(parse_args(["validate".into(), OsString::new()]).is_err());
    }
}
