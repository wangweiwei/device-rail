use std::{io, path::PathBuf};

use devicerail_protocol::DeviceId;
use thiserror::Error;

use crate::AdbDeviceState;

pub type AndroidAdbResult<T> = Result<T, AndroidAdbError>;

#[derive(Debug, Error)]
pub enum AndroidAdbError {
    #[error("adb operation was cancelled")]
    Cancelled,
    #[error("adb operation `{operation}` exceeded its deadline")]
    TimedOut { operation: &'static str },
    #[error("adb executable was not found: {program}")]
    ExecutableNotFound { program: PathBuf },
    #[error("failed to spawn adb operation `{operation}`: {source}")]
    Spawn {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("adb operation `{operation}` failed with status {status:?}: {stderr_tail}")]
    ProcessFailed {
        operation: &'static str,
        status: Option<i32>,
        stderr_tail: String,
    },
    #[error("failed to read {stream} for adb operation `{operation}`: {source}")]
    Read {
        operation: &'static str,
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("failed to write {stream} for adb operation `{operation}`: {source}")]
    Write {
        operation: &'static str,
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("protected adb operation `{operation}` failed with status {status:?}")]
    ProtectedOperationFailed {
        operation: &'static str,
        status: Option<i32>,
    },
    #[error("{stream} for adb operation `{operation}` exceeded {limit} bytes")]
    OutputTooLarge {
        operation: &'static str,
        stream: &'static str,
        limit: usize,
    },
    #[error("{stream} for adb operation `{operation}` is not valid UTF-8")]
    InvalidUtf8 {
        operation: &'static str,
        stream: &'static str,
    },
    #[error("invalid adb serial: {0}")]
    InvalidSerial(String),
    #[error("malformed `adb devices -l` output: {0}")]
    MalformedDevicesOutput(String),
    #[error("malformed Android screenshot PNG: {0}")]
    MalformedPng(String),
    #[error("malformed Android observation {input}: {detail}")]
    MalformedObservation { input: &'static str, detail: String },
    #[error("duplicate adb serial in discovery output: {0}")]
    DuplicateSerial(String),
    #[error("Android device {device_id} is missing")]
    Missing { device_id: DeviceId },
    #[error("Android device {device_id} is unauthorized; unlock it and accept the adb RSA prompt")]
    Unauthorized { device_id: DeviceId },
    #[error("Android device {device_id} is offline after {attempts} reconnect attempt(s)")]
    OfflineExhausted {
        device_id: DeviceId,
        attempts: usize,
    },
    #[error("Android device {device_id} is still booting after {attempts} check(s)")]
    BootingExhausted {
        device_id: DeviceId,
        attempts: usize,
    },
    #[error("Android device {device_id} is unavailable because host permissions deny adb access")]
    PermissionDenied { device_id: DeviceId },
    #[error("Android device {device_id} has unsupported adb state {state:?}")]
    UnsupportedState {
        device_id: DeviceId,
        state: AdbDeviceState,
    },
    #[error("adb returned an invalid value for {field}: {value:?}")]
    InvalidValue { field: &'static str, value: String },
}

impl AndroidAdbError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Cancelled => "android_adb_cancelled",
            Self::TimedOut { .. } => "android_adb_timed_out",
            Self::ExecutableNotFound { .. } => "android_adb_not_found",
            Self::Spawn { .. } => "android_adb_spawn_failed",
            Self::ProcessFailed { .. } => "android_adb_process_failed",
            Self::Read { .. } => "android_adb_read_failed",
            Self::Write { .. } => "android_adb_write_failed",
            Self::ProtectedOperationFailed { .. } => "android_adb_protected_operation_failed",
            Self::OutputTooLarge { .. } => "android_adb_output_too_large",
            Self::InvalidUtf8 { .. } => "android_adb_invalid_utf8",
            Self::InvalidSerial(_) => "android_adb_invalid_serial",
            Self::MalformedDevicesOutput(_) => "android_adb_malformed_devices",
            Self::MalformedPng(_) => "android_adb_malformed_png",
            Self::MalformedObservation { .. } => "android_adb_malformed_observation",
            Self::DuplicateSerial(_) => "android_adb_duplicate_serial",
            Self::Missing { .. } => "android_device_missing",
            Self::Unauthorized { .. } => "android_device_unauthorized",
            Self::OfflineExhausted { .. } => "android_device_offline",
            Self::BootingExhausted { .. } => "android_device_booting",
            Self::PermissionDenied { .. } => "android_device_permission_denied",
            Self::UnsupportedState { .. } => "android_device_state_unsupported",
            Self::InvalidValue { .. } => "android_adb_invalid_value",
        }
    }

    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::TimedOut { .. }
                | Self::Spawn { .. }
                | Self::ProcessFailed { .. }
                | Self::Read { .. }
                | Self::Write { .. }
                | Self::ProtectedOperationFailed { .. }
                | Self::MalformedPng(_)
                | Self::Missing { .. }
                | Self::Unauthorized { .. }
                | Self::OfflineExhausted { .. }
                | Self::BootingExhausted { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{io, path::PathBuf};

    use devicerail_protocol::DeviceId;

    use super::AndroidAdbError;

    #[test]
    fn retryability_distinguishes_transient_transport_from_local_configuration() {
        let device_id = DeviceId::new("android:test");
        for error in [
            AndroidAdbError::TimedOut { operation: "test" },
            AndroidAdbError::Spawn {
                operation: "test",
                source: io::Error::new(io::ErrorKind::Interrupted, "fixture"),
            },
            AndroidAdbError::Read {
                operation: "test",
                stream: "stdout",
                source: io::Error::new(io::ErrorKind::UnexpectedEof, "fixture"),
            },
            AndroidAdbError::Write {
                operation: "input_secret",
                stream: "stdin",
                source: io::Error::new(io::ErrorKind::BrokenPipe, "fixture"),
            },
            AndroidAdbError::ProtectedOperationFailed {
                operation: "input_secret",
                status: Some(1),
            },
            AndroidAdbError::MalformedPng("truncated transport output".to_owned()),
            AndroidAdbError::Missing {
                device_id: device_id.clone(),
            },
        ] {
            assert!(error.retryable(), "{} should be retryable", error.code());
        }

        for error in [
            AndroidAdbError::ExecutableNotFound {
                program: PathBuf::from("adb"),
            },
            AndroidAdbError::OutputTooLarge {
                operation: "capture_screenshot",
                stream: "stdout",
                limit: 32,
            },
            AndroidAdbError::MalformedObservation {
                input: "wm size",
                detail: "fixture".to_owned(),
            },
            AndroidAdbError::PermissionDenied { device_id },
        ] {
            assert!(!error.retryable(), "{} must not be retryable", error.code());
        }
    }
}
