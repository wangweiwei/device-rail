use std::{
    fmt,
    future::pending,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devicerail_core::{DriverError, DriverResult, ExecutionControl};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, MutexGuard},
    time,
};
use url::Url;

const BRIDGE_VERSION: u16 = 4;
const MAX_ENDPOINT_BYTES: usize = 4_096;
const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_STDOUT_BYTES: usize = 24 * 1024 * 1024;
const MAX_SCREENSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_COMMAND_TIMEOUT_MS: u64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserKind {
    Chromium,
    Firefox,
    Webkit,
}

#[derive(Clone)]
pub struct BridgeConfig {
    node_program: PathBuf,
    helper_script: PathBuf,
    endpoint: Arc<str>,
    browser: BrowserKind,
    command_timeout_ms: u64,
}

impl BridgeConfig {
    pub fn new(
        node_program: impl Into<PathBuf>,
        helper_script: impl Into<PathBuf>,
        endpoint: impl Into<String>,
        browser: BrowserKind,
    ) -> DriverResult<Self> {
        let node_program = node_program.into();
        let helper_script = helper_script.into();
        let endpoint = endpoint.into();
        let parsed_endpoint = Url::parse(&endpoint).ok();
        if node_program.as_os_str().is_empty()
            || helper_script.as_os_str().is_empty()
            || endpoint.is_empty()
            || endpoint.len() > MAX_ENDPOINT_BYTES
            || endpoint.chars().any(|character| character.is_control())
            || parsed_endpoint.as_ref().is_none_or(|parsed| {
                !matches!(parsed.scheme(), "ws" | "wss")
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.fragment().is_some()
            })
        {
            return Err(DriverError::Protocol(
                "invalid Playwright bridge configuration".to_owned(),
            ));
        }
        Ok(Self {
            node_program,
            helper_script,
            endpoint: Arc::from(endpoint),
            browser,
            command_timeout_ms: 30_000,
        })
    }

    pub fn with_command_timeout_ms(mut self, timeout_ms: u64) -> DriverResult<Self> {
        if timeout_ms == 0 || timeout_ms > MAX_COMMAND_TIMEOUT_MS {
            return Err(DriverError::Protocol(
                "invalid Playwright bridge timeout".to_owned(),
            ));
        }
        self.command_timeout_ms = timeout_ms;
        Ok(self)
    }

    pub fn browser(&self) -> BrowserKind {
        self.browser
    }

    pub fn endpoint_fingerprint(&self) -> String {
        hex::encode(Sha256::digest(self.endpoint.as_bytes()))
    }

    pub fn helper_script(&self) -> &Path {
        &self.helper_script
    }
}

impl fmt::Debug for BridgeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BridgeConfig")
            .field("node_program", &self.node_program)
            .field("helper_script", &self.helper_script)
            .field("endpoint", &"[REDACTED]")
            .field("browser", &self.browser)
            .field("command_timeout_ms", &self.command_timeout_ms)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveredPage {
    pub context_index: usize,
    pub page_index: usize,
    pub fingerprint: String,
    pub url: String,
    pub title: String,
    pub viewport: BridgeViewport,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeViewport {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BridgeActionResult {
    pub page: DiscoveredPage,
    pub output: Option<Value>,
}

#[async_trait]
pub trait PlaywrightBridge: Send + Sync {
    async fn discover(&self, control: &ExecutionControl) -> DriverResult<Vec<DiscoveredPage>>;
    async fn probe(
        &self,
        page: &DiscoveredPage,
        control: &ExecutionControl,
    ) -> DriverResult<DiscoveredPage>;
    async fn observe(
        &self,
        page: &DiscoveredPage,
        control: &ExecutionControl,
    ) -> DriverResult<(DiscoveredPage, Vec<u8>)>;
    async fn execute(
        &self,
        page: &DiscoveredPage,
        action: Value,
        control: &ExecutionControl,
    ) -> DriverResult<BridgeActionResult>;
}

pub struct SystemPlaywrightBridge {
    config: BridgeConfig,
    process: Mutex<Option<BridgeProcess>>,
}

struct BridgeProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl BridgeProcess {
    async fn exchange(&mut self, request: &[u8]) -> DriverResult<Vec<u8>> {
        self.stdin
            .write_all(request)
            .await
            .map_err(|_| platform("bridge_stdio_failed", true))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|_| platform("bridge_stdio_failed", true))?;
        self.stdin
            .flush()
            .await
            .map_err(|_| platform("bridge_stdio_failed", true))?;
        read_bounded_line(&mut self.stdout, MAX_STDOUT_BYTES).await
    }
}

impl SystemPlaywrightBridge {
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            config,
            process: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    async fn request(
        &self,
        operation: BridgeOperation,
        control: &ExecutionControl,
    ) -> DriverResult<BridgeResult> {
        ensure_active(control)?;
        let mut process = lock_process(&self.process, control).await?;
        ensure_active(control)?;
        let timeout_ms = request_timeout_ms(&self.config, control);
        let request = BridgeRequest {
            version: BRIDGE_VERSION,
            browser: self.config.browser,
            endpoint: self.config.endpoint.as_ref(),
            timeout_ms,
            operation,
        };
        let bytes = serde_json::to_vec(&request).map_err(|_| {
            DriverError::Internal("could not serialize Playwright request".to_owned())
        })?;
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(platform("bridge_request_too_large", false));
        }

        let restart = match process.as_mut() {
            Some(active) => !matches!(active.child.try_wait(), Ok(None)),
            None => true,
        };
        if restart {
            terminate_process(process.take());
            *process = Some(spawn_process(&self.config)?);
        }
        let duration = Duration::from_millis(timeout_ms);
        let exchange_result = {
            let exchange = process
                .as_mut()
                .expect("bridge process initialized")
                .exchange(&bytes);
            tokio::pin!(exchange);
            tokio::select! {
                result = &mut exchange => result,
                _ = control.cancelled() => Err(DriverError::Cancelled),
                _ = time::sleep(duration) => Err(if control.is_expired() {
                    DriverError::TimedOut
                } else {
                    platform("bridge_timeout", true)
                }),
            }
        };
        let stdout = match exchange_result {
            Ok(stdout) => stdout,
            Err(error) => {
                terminate_process(process.take());
                return Err(error);
            }
        };
        ensure_active(control)?;
        let response: BridgeResponse = match serde_json::from_slice(&stdout) {
            Ok(response) => response,
            Err(_) => {
                terminate_process(process.take());
                return Err(platform("bridge_invalid_response", false));
            }
        };
        response.into_result()
    }
}

#[async_trait]
impl PlaywrightBridge for SystemPlaywrightBridge {
    async fn discover(&self, control: &ExecutionControl) -> DriverResult<Vec<DiscoveredPage>> {
        match self.request(BridgeOperation::Discover, control).await? {
            BridgeResult::Pages { pages } => Ok(pages),
            BridgeResult::Page { .. } => Err(platform("bridge_invalid_response", false)),
        }
    }

    async fn probe(
        &self,
        page: &DiscoveredPage,
        control: &ExecutionControl,
    ) -> DriverResult<DiscoveredPage> {
        self.page_request(BridgeOperation::Probe { page: page.into() }, control)
            .await
    }

    async fn observe(
        &self,
        page: &DiscoveredPage,
        control: &ExecutionControl,
    ) -> DriverResult<(DiscoveredPage, Vec<u8>)> {
        let result = self
            .page_request_raw(BridgeOperation::Observe { page: page.into() }, control)
            .await?;
        if result.output.is_some() {
            return Err(platform("bridge_invalid_response", false));
        }
        let encoded = result
            .page
            .screenshot_base64
            .as_deref()
            .ok_or_else(|| platform("bridge_invalid_response", false))?;
        let bytes = BASE64
            .decode(encoded)
            .map_err(|_| platform("bridge_invalid_response", false))?;
        if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
            return Err(platform("bridge_screenshot_limit", false));
        }
        Ok((result.page.into_page(), bytes))
    }

    async fn execute(
        &self,
        page: &DiscoveredPage,
        action: Value,
        control: &ExecutionControl,
    ) -> DriverResult<BridgeActionResult> {
        let result = self
            .page_request_raw(
                BridgeOperation::Action {
                    page: page.into(),
                    action,
                },
                control,
            )
            .await?;
        Ok(BridgeActionResult {
            page: result.page.into_page(),
            output: result.output,
        })
    }
}

impl SystemPlaywrightBridge {
    async fn page_request(
        &self,
        operation: BridgeOperation,
        control: &ExecutionControl,
    ) -> DriverResult<DiscoveredPage> {
        let result = self.page_request_raw(operation, control).await?;
        if result.output.is_some() {
            return Err(platform("bridge_invalid_response", false));
        }
        Ok(result.page.into_page())
    }

    async fn page_request_raw(
        &self,
        operation: BridgeOperation,
        control: &ExecutionControl,
    ) -> DriverResult<BridgePageResult> {
        match self.request(operation, control).await? {
            BridgeResult::Page { page, output } => Ok(BridgePageResult { page, output }),
            BridgeResult::Pages { .. } => Err(platform("bridge_invalid_response", false)),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeRequest<'a> {
    version: u16,
    browser: BrowserKind,
    endpoint: &'a str,
    timeout_ms: u64,
    operation: BridgeOperation,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum BridgeOperation {
    Discover,
    Probe {
        page: BridgePageIdentity,
    },
    Observe {
        page: BridgePageIdentity,
    },
    Action {
        page: BridgePageIdentity,
        action: Value,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgePageIdentity {
    context_index: usize,
    page_index: usize,
    fingerprint: String,
}

impl From<&DiscoveredPage> for BridgePageIdentity {
    fn from(page: &DiscoveredPage) -> Self {
        Self {
            context_index: page.context_index,
            page_index: page.page_index,
            fingerprint: page.fingerprint.clone(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BridgeResponse {
    ok: bool,
    #[serde(default)]
    result: Option<BridgeResult>,
    #[serde(default)]
    error: Option<BridgeFailure>,
}

impl BridgeResponse {
    fn into_result(self) -> DriverResult<BridgeResult> {
        match (self.ok, self.result, self.error) {
            (true, Some(result), None) => Ok(result),
            (false, None, Some(error)) => Err(map_failure(error)),
            _ => Err(platform("bridge_invalid_response", false)),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum BridgeResult {
    Pages {
        pages: Vec<DiscoveredPage>,
    },
    Page {
        page: BridgePage,
        #[serde(default)]
        output: Option<Value>,
    },
}

struct BridgePageResult {
    page: BridgePage,
    output: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BridgePage {
    context_index: usize,
    page_index: usize,
    fingerprint: String,
    url: String,
    title: String,
    viewport: BridgeViewport,
    #[serde(default)]
    screenshot_base64: Option<String>,
}

impl BridgePage {
    fn into_page(self) -> DiscoveredPage {
        DiscoveredPage {
            context_index: self.context_index,
            page_index: self.page_index,
            fingerprint: self.fingerprint,
            url: self.url,
            title: self.title,
            viewport: self.viewport,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BridgeFailure {
    code: String,
    retryable: bool,
}

fn map_failure(error: BridgeFailure) -> DriverError {
    let code = match error.code.as_str() {
        "invalid_request" => "bridge_invalid_request",
        "connect_failed" => "bridge_connect_failed",
        "page_not_found" => "page_not_found",
        "page_identity_unavailable" => "page_identity_unavailable",
        "page_identity_changed" => "page_identity_changed",
        "operation_timed_out" => "playwright_timeout",
        "operation_failed" => "playwright_operation_failed",
        "response_too_large" => "bridge_response_too_large",
        _ => "bridge_unknown_failure",
    };
    platform(code, error.retryable)
}

fn request_timeout_ms(config: &BridgeConfig, control: &ExecutionControl) -> u64 {
    control
        .remaining()
        .map_or(config.command_timeout_ms, |remaining| {
            config.command_timeout_ms.min(
                u64::try_from(remaining.as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1),
            )
        })
}

async fn lock_process<'a>(
    process: &'a Mutex<Option<BridgeProcess>>,
    control: &ExecutionControl,
) -> DriverResult<MutexGuard<'a, Option<BridgeProcess>>> {
    ensure_active(control)?;
    let deadline = async {
        match control.remaining() {
            Some(remaining) => time::sleep(remaining).await,
            None => pending::<()>().await,
        }
    };
    tokio::select! {
        guard = process.lock() => Ok(guard),
        _ = control.cancelled() => Err(DriverError::Cancelled),
        _ = deadline => Err(DriverError::TimedOut),
    }
}

fn spawn_process(config: &BridgeConfig) -> DriverResult<BridgeProcess> {
    let mut command = Command::new(&config.node_program);
    command
        .arg(&config.helper_script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| platform("bridge_process_failed", true))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| platform("bridge_process_failed", true))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| platform("bridge_process_failed", true))?;
    Ok(BridgeProcess {
        child,
        stdin,
        stdout: BufReader::new(stdout),
    })
}

fn terminate_process(process: Option<BridgeProcess>) {
    if let Some(mut process) = process {
        let _ = process.child.start_kill();
    }
}

async fn read_bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> DriverResult<Vec<u8>> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    loop {
        let buffer = reader
            .fill_buf()
            .await
            .map_err(|_| platform("bridge_stdio_failed", true))?;
        if buffer.is_empty() {
            return Err(platform("bridge_process_failed", true));
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(buffer.len(), |index| index + 1);
        if bytes
            .len()
            .checked_add(count)
            .is_none_or(|observed| observed > limit)
        {
            return Err(platform("bridge_output_limit", false));
        }
        bytes.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(bytes);
        }
    }
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

fn platform(code: &str, retryable: bool) -> DriverError {
    DriverError::Platform {
        code: code.to_owned(),
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncWriteExt as _, BufReader, duplex};

    use super::read_bounded_line;

    #[tokio::test]
    async fn bounded_reader_returns_one_line_without_waiting_for_eof() {
        let (mut writer, reader) = duplex(256);
        writer
            .write_all(b"{\"request\":1}\n{\"request\":2}\n")
            .await
            .expect("write bridge responses");
        let mut reader = BufReader::new(reader);

        assert_eq!(
            read_bounded_line(&mut reader, 128)
                .await
                .expect("first response"),
            b"{\"request\":1}\n"
        );
        assert_eq!(
            read_bounded_line(&mut reader, 128)
                .await
                .expect("second response"),
            b"{\"request\":2}\n"
        );
    }

    #[tokio::test]
    async fn bounded_reader_rejects_an_oversized_line() {
        let (mut writer, reader) = duplex(256);
        writer
            .write_all(b"123456789\n")
            .await
            .expect("write oversized response");
        let mut reader = BufReader::new(reader);

        let error = read_bounded_line(&mut reader, 8)
            .await
            .expect_err("line must remain bounded");
        assert!(matches!(
            error,
            devicerail_core::DriverError::Platform { ref code, retryable: false }
                if code == "bridge_output_limit"
        ));
    }
}
