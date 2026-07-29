use std::{ffi::OsString, io::Write as _, process::ExitCode};

use devicerail_core::ExecutionControl;
use devicerail_visualizer::{
    OfflineVisualizer, ServerError, ServerLimits, ViewerServer, VisualizerError, VisualizerLimits,
};
use thiserror::Error;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let code = if matches!(error, CliError::Usage) {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            };
            let _ = writeln!(std::io::stderr().lock(), "devicerail-visualizer: {error}");
            code
        }
    }
}

async fn run() -> Result<(), CliError> {
    let bundle = parse_args(std::env::args_os().skip(1))?;
    let viewer = OfflineVisualizer::open(
        bundle,
        VisualizerLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await?;
    let mut server = ViewerServer::bind(viewer, ServerLimits::default()).await?;
    writeln!(std::io::stdout().lock(), "{}", server.url()).map_err(|_| CliError::Output)?;
    std::io::stdout()
        .lock()
        .flush()
        .map_err(|_| CliError::Output)?;
    tokio::signal::ctrl_c()
        .await
        .map_err(|_| CliError::Signal)?;
    server.shutdown().await?;
    Ok(())
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Result<OsString, CliError> {
    let bundle = args.next().ok_or(CliError::Usage)?;
    if bundle.is_empty() || args.next().is_some() {
        return Err(CliError::Usage);
    }
    Ok(bundle)
}

#[derive(Debug, Error)]
enum CliError {
    #[error("usage: devicerail-visualizer <bundle-directory>")]
    Usage,
    #[error(transparent)]
    Visualizer(#[from] VisualizerError),
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("could not write the local capability URL")]
    Output,
    #[error("could not install the interrupt handler")]
    Signal,
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{CliError, parse_args};

    #[test]
    fn accepts_exactly_one_nonempty_bundle_path() {
        assert_eq!(
            parse_args([OsString::from("bundle")].into_iter()).expect("path"),
            OsString::from("bundle")
        );
        assert!(matches!(
            parse_args(Vec::<OsString>::new().into_iter()),
            Err(CliError::Usage)
        ));
        assert!(matches!(
            parse_args([OsString::new()].into_iter()),
            Err(CliError::Usage)
        ));
        assert!(matches!(
            parse_args([OsString::from("a"), OsString::from("b")].into_iter()),
            Err(CliError::Usage)
        ));
    }
}
