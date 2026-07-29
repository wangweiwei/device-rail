use std::{path::Path, time::Duration};

use devicerail_core::{DriverError, DriverResult, ExecutionControl};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    task::JoinHandle,
    time,
};

use crate::{
    PLUGIN_ABI_VERSION, PluginOperation, PluginRequest, PluginResponse, PluginResponseResult,
};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_STDOUT_BYTES: usize = 24 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 16 * 1024;
const MAX_COMMAND_TIMEOUT_MS: u64 = 120_000;
const TERMINATION_GRACE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginTransportConfig {
    command_timeout_ms: u64,
}

impl Default for PluginTransportConfig {
    fn default() -> Self {
        Self {
            command_timeout_ms: 30_000,
        }
    }
}

impl PluginTransportConfig {
    pub fn new(timeout: Duration) -> DriverResult<Self> {
        let command_timeout_ms = timeout
            .as_millis()
            .try_into()
            .map_err(|_| invalid_config())?;
        if command_timeout_ms == 0 || command_timeout_ms > MAX_COMMAND_TIMEOUT_MS {
            return Err(invalid_config());
        }
        Ok(Self { command_timeout_ms })
    }

    pub const fn command_timeout_ms(self) -> u64 {
        self.command_timeout_ms
    }
}

pub(crate) struct PluginTransport {
    executable: std::path::PathBuf,
    config: PluginTransportConfig,
    supervisor: Mutex<SupervisorState>,
}

enum SupervisorState {
    NotStarted,
    Running(Box<RunningPlugin>),
    Broken,
}

struct RunningPlugin {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: JoinHandle<DriverResult<()>>,
}

enum ExchangeError {
    /// A well-formed error selected by the plugin. The process remains usable.
    Remote(DriverError),
    /// Transport/framing/process ambiguity. The process must be killed and is
    /// never restarted, so a mutating operation cannot be replayed.
    Fatal(DriverError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExchangeTimeout {
    CallerDeadline(Duration),
    Transport(Duration),
}

impl ExchangeTimeout {
    const fn duration(self) -> Duration {
        match self {
            Self::CallerDeadline(duration) | Self::Transport(duration) => duration,
        }
    }
}

#[derive(Clone, Copy)]
enum ExpectedResult {
    Hello,
    Ack,
    Frame,
    Action,
}

impl ExpectedResult {
    fn for_operation(operation: &PluginOperation) -> Self {
        match operation {
            PluginOperation::Hello { .. } => Self::Hello,
            PluginOperation::Health | PluginOperation::Connect | PluginOperation::Disconnect => {
                Self::Ack
            }
            PluginOperation::Observe { .. } => Self::Frame,
            PluginOperation::Execute { .. } => Self::Action,
        }
    }

    fn accepts(self, result: &PluginResponseResult) -> bool {
        matches!(
            (self, result),
            (Self::Hello, PluginResponseResult::Hello { .. })
                | (Self::Ack, PluginResponseResult::Ack)
                | (Self::Frame, PluginResponseResult::Frame { .. })
                | (Self::Action, PluginResponseResult::Action { .. })
        )
    }
}

impl PluginTransport {
    pub(crate) fn new(executable: std::path::PathBuf, config: PluginTransportConfig) -> Self {
        Self {
            executable,
            config,
            supervisor: Mutex::new(SupervisorState::NotStarted),
        }
    }

    pub(crate) async fn request(
        &self,
        request: PluginRequest,
        control: &ExecutionControl,
    ) -> DriverResult<PluginResponseResult> {
        ensure_active(control)?;
        let request_id = request.request_id;
        let expected_result = ExpectedResult::for_operation(&request.operation);
        let mut bytes =
            serde_json::to_vec(&request).map_err(|_| platform("plugin_request_invalid", false))?;
        if bytes.is_empty() || bytes.len() >= MAX_REQUEST_BYTES {
            return Err(platform("plugin_request_limit", false));
        }
        bytes.push(b'\n');
        let mut supervisor = lock_supervisor(&self.supervisor, control).await?;
        // The caller may have spent most or all of its budget waiting for the
        // serialized process. Recheck before spawning or delivering anything;
        // a pre-delivery timeout/cancellation leaves the existing process safe
        // to reuse and therefore must not poison it.
        ensure_active(control)?;
        if matches!(*supervisor, SupervisorState::NotStarted) {
            *supervisor = SupervisorState::Running(Box::new(self.spawn()?));
        }
        // Spawning is synchronous and may consume caller budget too. Take the
        // exchange budget only at the final pre-delivery boundary so lock and
        // spawn time cannot extend an absolute Core deadline.
        ensure_active(control)?;
        let timeout = exchange_timeout(self.config, control);
        let result = {
            let SupervisorState::Running(running) = &mut *supervisor else {
                return Err(platform("plugin_process_unavailable", false));
            };
            tokio::select! {
                result = exchange(running, bytes, request_id) => Some(result),
                _ = control.cancelled() => None,
                _ = time::sleep(timeout.duration()) => None,
            }
        };
        match result {
            Some(Ok(result)) if expected_result.accepts(&result) => Ok(result),
            Some(Ok(_)) => {
                // A response kind that does not match the delivered operation
                // leaves lifecycle/mutation state ambiguous. Never reuse or
                // restart that process.
                poison(&mut supervisor).await;
                Err(platform("plugin_response_kind_invalid", false))
            }
            Some(Err(ExchangeError::Remote(error))) => Err(error),
            Some(Err(ExchangeError::Fatal(error))) => {
                poison(&mut supervisor).await;
                Err(error)
            }
            None => {
                // The in-flight exchange was dropped before taking ownership
                // of the process. No request is retried after this ambiguous
                // point.
                poison(&mut supervisor).await;
                Err(if control.is_cancelled() {
                    DriverError::Cancelled
                } else {
                    match timeout {
                        ExchangeTimeout::CallerDeadline(_) => DriverError::TimedOut,
                        ExchangeTimeout::Transport(_) => platform("plugin_timeout", true),
                    }
                })
            }
        }
    }

    fn spawn(&self) -> DriverResult<RunningPlugin> {
        revalidate_executable(&self.executable)?;
        let working_directory = self.executable.parent().unwrap_or_else(|| Path::new("."));
        let mut command = Command::new(&self.executable);
        command
            .arg("--devicerail-plugin-abi=1")
            .current_dir(working_directory)
            .env_clear()
            .env("LANG", "C")
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| platform("plugin_spawn_failed", true))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| platform("plugin_stdio_failed", true))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| platform("plugin_stdio_failed", true))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| platform("plugin_stdio_failed", true))?;
        let stderr =
            tokio::spawn(async move { read_stderr_bounded(stderr, MAX_STDERR_BYTES).await });
        Ok(RunningPlugin {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
        })
    }
}

fn exchange_timeout(config: PluginTransportConfig, control: &ExecutionControl) -> ExchangeTimeout {
    let configured = Duration::from_millis(config.command_timeout_ms);
    match control.remaining() {
        Some(remaining) if remaining <= configured => ExchangeTimeout::CallerDeadline(remaining),
        Some(_) | None => ExchangeTimeout::Transport(configured),
    }
}

impl std::fmt::Debug for PluginTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginTransport")
            .field("executable", &"[VALIDATED]")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Drop for PluginTransport {
    fn drop(&mut self) {
        if let SupervisorState::Running(running) = self.supervisor.get_mut() {
            let _ = running.child.start_kill();
            running.stderr.abort();
        }
    }
}

async fn exchange(
    running: &mut RunningPlugin,
    bytes: Vec<u8>,
    request_id: uuid::Uuid,
) -> Result<PluginResponseResult, ExchangeError> {
    if running.stderr.is_finished() {
        return Err(ExchangeError::Fatal(platform("plugin_stderr_closed", true)));
    }
    running
        .stdin
        .write_all(&bytes)
        .await
        .map_err(|_| ExchangeError::Fatal(platform("plugin_write_failed", true)))?;
    running
        .stdin
        .flush()
        .await
        .map_err(|_| ExchangeError::Fatal(platform("plugin_write_failed", true)))?;
    let response = tokio::select! {
        response = read_frame(&mut running.stdout, MAX_STDOUT_BYTES) => response?,
        stderr = &mut running.stderr => {
            let error = match stderr {
                Ok(Ok(())) => platform("plugin_stderr_closed", true),
                Ok(Err(error)) => error,
                Err(_) => platform("plugin_stderr_failed", true),
            };
            return Err(ExchangeError::Fatal(error));
        }
    };
    if running
        .child
        .try_wait()
        .map_err(|_| ExchangeError::Fatal(platform("plugin_wait_failed", true)))?
        .is_some()
    {
        return Err(ExchangeError::Fatal(platform(
            "plugin_process_exited",
            true,
        )));
    }
    let response: PluginResponse = serde_json::from_slice(&response)
        .map_err(|_| ExchangeError::Fatal(platform("plugin_response_invalid", false)))?;
    if response.abi_version != PLUGIN_ABI_VERSION || response.request_id != request_id {
        return Err(ExchangeError::Fatal(platform(
            "plugin_response_mismatch",
            false,
        )));
    }
    match (response.ok, response.result, response.error) {
        (true, Some(result), None) => Ok(result),
        (false, None, Some(error)) if valid_remote_code(&error.code) => Err(ExchangeError::Remote(
            platform(&format!("plugin_{}", error.code), error.retryable),
        )),
        _ => Err(ExchangeError::Fatal(platform(
            "plugin_response_invalid",
            false,
        ))),
    }
}

async fn read_frame<R>(reader: &mut BufReader<R>, limit: usize) -> Result<Vec<u8>, ExchangeError>
where
    R: AsyncRead + Unpin,
{
    let mut frame = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|_| ExchangeError::Fatal(platform("plugin_read_failed", true)))?;
        if available.is_empty() {
            return Err(ExchangeError::Fatal(platform(
                "plugin_process_exited",
                true,
            )));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.unwrap_or(available.len());
        if frame
            .len()
            .checked_add(count)
            .is_none_or(|observed| observed > limit)
        {
            return Err(ExchangeError::Fatal(platform("plugin_stdout_limit", false)));
        }
        frame.extend_from_slice(&available[..count]);
        let consumed = count + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            if frame.is_empty() || frame.last() == Some(&b'\r') {
                return Err(ExchangeError::Fatal(platform(
                    "plugin_response_invalid",
                    false,
                )));
            }
            return Ok(frame);
        }
    }
}

async fn read_stderr_bounded<R>(mut reader: R, limit: usize) -> DriverResult<()>
where
    R: AsyncRead + Unpin,
{
    let mut observed = 0_usize;
    let mut buffer = [0_u8; 4 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(|_| platform("plugin_stderr_failed", true))?;
        if count == 0 {
            return Ok(());
        }
        observed = observed
            .checked_add(count)
            .ok_or_else(|| platform("plugin_stderr_limit", false))?;
        if observed > limit {
            return Err(platform("plugin_stderr_limit", false));
        }
    }
}

async fn poison(supervisor: &mut SupervisorState) {
    let state = std::mem::replace(supervisor, SupervisorState::Broken);
    let SupervisorState::Running(mut running) = state else {
        return;
    };
    let _ = running.child.start_kill();
    let _ = time::timeout(TERMINATION_GRACE, running.child.wait()).await;
    running.stderr.abort();
    let _ = running.stderr.await;
}

async fn lock_supervisor<'a>(
    supervisor: &'a Mutex<SupervisorState>,
    control: &ExecutionControl,
) -> DriverResult<tokio::sync::MutexGuard<'a, SupervisorState>> {
    let deadline = async {
        match control.remaining() {
            Some(remaining) => time::sleep(remaining).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        guard = supervisor.lock() => Ok(guard),
        _ = control.cancelled() => Err(DriverError::Cancelled),
        _ = deadline => Err(DriverError::TimedOut),
    }
}

fn valid_remote_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 57
        && code.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || (index > 0 && byte.is_ascii_digit())
                || (index > 0 && byte == b'_')
        })
}

#[cfg(unix)]
fn revalidate_executable(path: &Path) -> DriverResult<()> {
    crate::owner_only::require_no_extended_acl_path(path).map_err(|error| match error {
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Present => {
            platform("plugin_executable_changed", false)
        }
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Unavailable => {
            platform("plugin_permissions_unsupported", false)
        }
    })?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| platform("plugin_executable_changed", false))?;
    let canonical =
        std::fs::canonicalize(path).map_err(|_| platform("plugin_executable_changed", false))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || canonical != path
        || executable_permissions_are_unsafe(&metadata)
        || !executable_owned_by_current_process(&metadata)
    {
        return Err(platform("plugin_executable_changed", false));
    }
    Ok(())
}

#[cfg(not(unix))]
fn revalidate_executable(_path: &Path) -> DriverResult<()> {
    Err(platform("plugin_permissions_unsupported", false))
}

#[cfg(unix)]
fn executable_owned_by_current_process(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    // SAFETY: geteuid has no preconditions and does not retain pointers.
    metadata.uid() == unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn executable_permissions_are_unsafe(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = metadata.permissions().mode();
    mode & 0o022 != 0 || mode & 0o111 == 0
}

fn ensure_active(control: &ExecutionControl) -> DriverResult<()> {
    if control.is_cancelled() {
        Err(DriverError::Cancelled)
    } else if control.is_expired() {
        Err(DriverError::TimedOut)
    } else {
        Ok(())
    }
}

fn invalid_config() -> DriverError {
    DriverError::Protocol("invalid plugin transport configuration".to_owned())
}

fn platform(code: &str, retryable: bool) -> DriverError {
    DriverError::Platform {
        code: code.to_owned(),
        retryable,
    }
}

#[cfg(all(test, not(unix)))]
mod tests {
    use super::*;

    #[test]
    fn executable_revalidation_fails_closed_without_acl_proof() {
        let error = revalidate_executable(Path::new("plugin.exe"))
            .expect_err("non-Unix executable revalidation must fail closed");
        assert!(matches!(
            error,
            DriverError::Platform {
                code,
                retryable: false,
            } if code == "plugin_permissions_unsupported"
        ));
    }
}
