use std::{io, path::PathBuf};

use devicerail_core::DriverError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HarmonyHdcError {
    #[error("invalid HDC configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid HDC target: {0}")]
    InvalidTarget(String),
    #[error("invalid {field}: value does not satisfy the bounded HarmonyOS grammar")]
    InvalidValue { field: &'static str },
    #[error("HDC executable was not found: {program}")]
    ExecutableNotFound { program: PathBuf },
    #[error("HDC I/O failed during {operation}: {message}")]
    Io {
        operation: &'static str,
        message: String,
    },
    #[error("HDC operation {operation} was cancelled")]
    Cancelled { operation: &'static str },
    #[error("HDC operation {operation} timed out")]
    TimedOut { operation: &'static str },
    #[error("HDC operation {operation} exited unsuccessfully with status {status}")]
    NonZeroExit {
        operation: &'static str,
        status: String,
    },
    #[error("HDC reported a command failure during {operation}")]
    ReportedFailure { operation: &'static str },
    #[error("HDC {stream} for {operation} exceeded {limit} bytes")]
    OutputTooLarge {
        operation: &'static str,
        stream: &'static str,
        limit: usize,
    },
    #[error("HDC returned invalid output for {operation}")]
    InvalidOutput { operation: &'static str },
    #[error("HDC target is unavailable: {state}")]
    TargetUnavailable { state: String },
    #[error("duplicate HDC target in discovery output")]
    DuplicateTarget,
}

pub type HarmonyHdcResult<T> = Result<T, HarmonyHdcError>;

impl HarmonyHdcError {
    pub(crate) fn io(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Io {
            operation,
            message: error.to_string(),
        }
    }

    pub(crate) fn process_io(
        operation: &'static str,
        program: &std::path::Path,
        error: io::Error,
    ) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            Self::ExecutableNotFound {
                program: program.to_path_buf(),
            }
        } else {
            Self::io(operation, error)
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfiguration(_) => "invalid_hdc_configuration",
            Self::InvalidTarget(_) => "invalid_hdc_target",
            Self::InvalidValue { .. } => "invalid_harmony_value",
            Self::ExecutableNotFound { .. } => "hdc_executable_not_found",
            Self::Io { .. } => "hdc_io_failed",
            Self::Cancelled { .. } => "hdc_cancelled",
            Self::TimedOut { .. } => "hdc_timed_out",
            Self::NonZeroExit { .. } => "hdc_command_failed",
            Self::ReportedFailure { .. } => "hdc_command_failed",
            Self::OutputTooLarge { .. } => "hdc_output_too_large",
            Self::InvalidOutput { .. } => "hdc_invalid_output",
            Self::TargetUnavailable { state } => match state.as_str() {
                "offline" => "hdc_target_offline",
                "unauthorized" => "hdc_target_unauthorized",
                _ => "hdc_target_unknown",
            },
            Self::DuplicateTarget => "hdc_duplicate_target",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Io { .. }
                | Self::TimedOut { .. }
                | Self::NonZeroExit { .. }
                | Self::ReportedFailure { .. }
                | Self::TargetUnavailable { .. }
        )
    }

    pub(crate) fn into_driver_error(self) -> DriverError {
        match self {
            Self::Cancelled { .. } => DriverError::Cancelled,
            Self::TimedOut { .. } => DriverError::TimedOut,
            other => DriverError::Platform {
                code: other.code().to_owned(),
                retryable: other.retryable(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use devicerail_core::DriverError;

    use super::HarmonyHdcError;

    #[test]
    fn control_failures_preserve_driver_control_semantics() {
        assert!(matches!(
            HarmonyHdcError::Cancelled { operation: "probe" }.into_driver_error(),
            DriverError::Cancelled
        ));
        assert!(matches!(
            HarmonyHdcError::TimedOut { operation: "probe" }.into_driver_error(),
            DriverError::TimedOut
        ));
    }

    #[test]
    fn platform_failures_expose_only_stable_codes() {
        let error = HarmonyHdcError::InvalidOutput {
            operation: "dump_layout",
        };
        assert_eq!(error.code(), "hdc_invalid_output");
        assert!(!error.retryable());
        assert!(matches!(
            error.into_driver_error(),
            DriverError::Platform {
                code,
                retryable: false
            } if code == "hdc_invalid_output"
        ));
    }

    #[test]
    fn unavailable_targets_map_to_closed_driver_codes() {
        for (state, expected_code) in [
            ("offline", "hdc_target_offline"),
            ("unauthorized", "hdc_target_unauthorized"),
            ("RAW-HDC-STATE-SENTINEL", "hdc_target_unknown"),
        ] {
            let error = HarmonyHdcError::TargetUnavailable {
                state: state.to_owned(),
            }
            .into_driver_error();
            match &error {
                DriverError::Platform { code, retryable } => {
                    assert_eq!(code, expected_code);
                    assert!(retryable, "{expected_code} should be retryable");
                }
                other => panic!("unexpected Driver error: {other:?}"),
            }
            assert!(!format!("{error:?}").contains("RAW-HDC-STATE-SENTINEL"));
        }
    }
}
