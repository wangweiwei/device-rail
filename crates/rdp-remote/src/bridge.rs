use std::{fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devicerail_core::{DriverError, DriverResult, ExecutionControl, run_bounded_blocking};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    net::TcpStream,
    time,
};
use url::{Host, Url};
use uuid::Uuid;

pub const BRIDGE_PROTOCOL_VERSION: u16 = 2;
pub const BRIDGE_PROTOCOL_SCHEMA: &str = include_str!("../protocol/bridge-v2.schema.json");
const MAX_ENDPOINT_BYTES: usize = 4_096;
const MAX_TARGET_BYTES: usize = 4_096;
const MAX_TOKEN_BYTES: usize = 4_096;
const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_RESPONSE_BYTES: usize = 24 * 1024 * 1024;
const MAX_SCREENSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 512;
const MAX_REMOTE_CODE_BYTES: usize = 60;
const MAX_TIMEOUT_MS: u64 = 120_000;

#[derive(Clone, PartialEq, Eq)]
pub struct RdpTarget {
    authority: Arc<str>,
}

impl fmt::Debug for RdpTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdpTarget")
            .field("fingerprint", &self.fingerprint())
            .finish()
    }
}

impl RdpTarget {
    pub fn parse(value: impl Into<String>) -> Result<Self, RdpBridgeError> {
        let value = value.into();
        let parsed = Url::parse(&value).map_err(|_| RdpBridgeError::InvalidTarget)?;
        let valid_path = parsed.path().is_empty() || parsed.path() == "/";
        if value.is_empty()
            || value.len() > MAX_TARGET_BYTES
            || value.chars().any(char::is_control)
            || parsed.scheme() != "rdp"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !valid_path
        {
            return Err(RdpBridgeError::InvalidTarget);
        }
        let host = match parsed.host().expect("host checked") {
            Host::Domain(host) => host.to_owned(),
            Host::Ipv4(host) => host.to_string(),
            Host::Ipv6(host) => format!("[{host}]"),
        };
        let authority = match parsed.port() {
            Some(port) => format!("{host}:{port}"),
            None => format!("{host}:3389"),
        };
        Ok(Self {
            authority: Arc::from(authority),
        })
    }

    pub fn fingerprint(&self) -> String {
        hex::encode(Sha256::digest(self.authority.as_bytes()))
    }

    fn authority(&self) -> &str {
        &self.authority
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    endpoint: Arc<str>,
    target: RdpTarget,
    authentication_token: Arc<str>,
    timeout_ms: u64,
}

impl BridgeConfig {
    pub fn new(
        endpoint: impl Into<String>,
        target: RdpTarget,
        authentication_token: impl Into<String>,
    ) -> Result<Self, RdpBridgeError> {
        let endpoint = endpoint.into();
        let token = authentication_token.into();
        if endpoint.is_empty()
            || endpoint.len() > MAX_ENDPOINT_BYTES
            || endpoint.chars().any(char::is_control)
            || token.is_empty()
            || token.len() > MAX_TOKEN_BYTES
            || token.chars().any(char::is_control)
        {
            return Err(RdpBridgeError::InvalidConfiguration);
        }
        let parsed = endpoint
            .parse::<std::net::SocketAddr>()
            .map_err(|_| RdpBridgeError::InvalidConfiguration)?;
        if !parsed.ip().is_loopback() {
            return Err(RdpBridgeError::InvalidConfiguration);
        }
        Ok(Self {
            endpoint: Arc::from(parsed.to_string()),
            target,
            authentication_token: Arc::from(token),
            timeout_ms: 30_000,
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, RdpBridgeError> {
        let timeout_ms: u64 = timeout
            .as_millis()
            .try_into()
            .map_err(|_| RdpBridgeError::InvalidConfiguration)?;
        if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
            return Err(RdpBridgeError::InvalidConfiguration);
        }
        self.timeout_ms = timeout_ms;
        Ok(self)
    }

    pub fn target(&self) -> &RdpTarget {
        &self.target
    }
}

impl fmt::Debug for BridgeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BridgeConfig")
            .field("endpoint", &self.endpoint)
            .field("target", &self.target.fingerprint())
            .field("authentication_token", &"[REDACTED]")
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq)]
pub struct RdpDesktop {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub desktop_name: Option<String>,
    pub server_version: Option<String>,
}

impl fmt::Debug for RdpDesktop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdpDesktop")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("scale_factor", &self.scale_factor)
            .field("has_desktop_name", &self.desktop_name.is_some())
            .field("has_server_version", &self.server_version.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq)]
pub struct RdpFrame {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub png: Vec<u8>,
    pub desktop_name: Option<String>,
    pub server_version: Option<String>,
}

impl fmt::Debug for RdpFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdpFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("scale_factor", &self.scale_factor)
            .field("png_bytes", &self.png.len())
            .field("has_desktop_name", &self.desktop_name.is_some())
            .field("has_server_version", &self.server_version.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RdpInput {
    Tap { x: u32, y: u32 },
    PointerMove { x: u32, y: u32 },
    Scroll { delta_x: i32, delta_y: i32 },
    KeyPress { key: String },
    TypeText { text: String },
    InputSecret { text: String },
}

impl fmt::Debug for RdpInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tap { x, y } => formatter
                .debug_struct("Tap")
                .field("x", x)
                .field("y", y)
                .finish(),
            Self::PointerMove { x, y } => formatter
                .debug_struct("PointerMove")
                .field("x", x)
                .field("y", y)
                .finish(),
            Self::Scroll { delta_x, delta_y } => formatter
                .debug_struct("Scroll")
                .field("delta_x", delta_x)
                .field("delta_y", delta_y)
                .finish(),
            Self::KeyPress { .. } => formatter.debug_struct("KeyPress").finish_non_exhaustive(),
            Self::TypeText { text } => formatter
                .debug_struct("TypeText")
                .field("character_count", &text.chars().count())
                .finish_non_exhaustive(),
            Self::InputSecret { .. } => formatter
                .debug_struct("InputSecret")
                .finish_non_exhaustive(),
        }
    }
}

#[async_trait]
pub trait RdpBridge: Send + Sync {
    /// Stable physical-resource identity used to derive the DeviceId.
    fn target_fingerprint(&self) -> String;

    async fn health(&self, device_id: &str, control: &ExecutionControl)
    -> DriverResult<RdpDesktop>;

    async fn connect(
        &self,
        device_id: &str,
        control: &ExecutionControl,
    ) -> DriverResult<RdpDesktop>;
    async fn disconnect(&self, device_id: &str, control: &ExecutionControl) -> DriverResult<()>;
    async fn probe(&self, device_id: &str, control: &ExecutionControl) -> DriverResult<RdpDesktop>;
    async fn capture(&self, device_id: &str, control: &ExecutionControl) -> DriverResult<RdpFrame>;
    async fn input(
        &self,
        device_id: &str,
        call_id: Uuid,
        input: RdpInput,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
}

pub struct SystemRdpBridge {
    config: BridgeConfig,
}

impl SystemRdpBridge {
    pub fn new(config: BridgeConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    async fn request(
        &self,
        device_id: &str,
        operation: BridgeOperation,
        control: &ExecutionControl,
    ) -> DriverResult<BridgePayload> {
        ensure_active(control)?;
        if device_id.trim().is_empty()
            || device_id.len() > 512
            || device_id.chars().any(char::is_control)
        {
            return Err(DriverError::Protocol("invalid RDP device id".to_owned()));
        }
        let timeout_ms = control
            .remaining()
            .map_or(self.config.timeout_ms, |remaining| {
                self.config
                    .timeout_ms
                    .min(remaining.as_millis().try_into().unwrap_or(u64::MAX).max(1))
            });
        let request = BridgeRequest {
            version: BRIDGE_PROTOCOL_VERSION,
            operation_id: Uuid::new_v4(),
            authentication_token: &self.config.authentication_token,
            device_id,
            target: self.config.target.authority(),
            timeout_ms,
            operation,
        };
        let mut bytes = serde_json::to_vec(&request).map_err(|_| {
            DriverError::Internal("could not serialize RDP bridge request".to_owned())
        })?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(platform("bridge_request_too_large", false));
        }
        bytes.push(b'\n');
        let duration = Duration::from_millis(timeout_ms);
        let endpoint = self.config.endpoint.clone();
        let operation = async move {
            let mut stream = TcpStream::connect(endpoint.as_ref())
                .await
                .map_err(|_| platform("bridge_connect_failed", true))?;
            stream
                .write_all(&bytes)
                .await
                .map_err(|_| platform("bridge_write_failed", true))?;
            // Keep the write half open while the bridge performs the
            // operation. Dropping this future closes the full socket, which
            // bridge v2 defines as a mandatory cancellation signal.
            let mut response = Vec::new();
            BufReader::new(&mut stream)
                .take((MAX_RESPONSE_BYTES + 2) as u64)
                .read_until(b'\n', &mut response)
                .await
                .map_err(|_| platform("bridge_read_failed", true))?;
            if response.len() > MAX_RESPONSE_BYTES + 1 {
                return Err(platform("bridge_response_too_large", false));
            }
            if response.last() != Some(&b'\n') {
                return Err(platform("bridge_read_failed", true));
            }
            response.pop();
            Ok(response)
        };
        tokio::pin!(operation);
        let response = tokio::select! {
            result = &mut operation => result?,
            _ = control.cancelled() => return Err(DriverError::Cancelled),
            _ = time::sleep(duration) => {
                return Err(if control.is_expired() {
                    DriverError::TimedOut
                } else {
                    platform("bridge_timeout", true)
                });
            }
        };
        ensure_active(control)?;
        let response: BridgeResponse = serde_json::from_slice(&response)
            .map_err(|_| platform("bridge_invalid_response", false))?;
        response.into_result()
    }
}

#[async_trait]
impl RdpBridge for SystemRdpBridge {
    fn target_fingerprint(&self) -> String {
        self.config.target.fingerprint()
    }

    async fn health(
        &self,
        device_id: &str,
        control: &ExecutionControl,
    ) -> DriverResult<RdpDesktop> {
        self.desktop_request(device_id, BridgeOperation::Health, control)
            .await
    }

    async fn connect(
        &self,
        device_id: &str,
        control: &ExecutionControl,
    ) -> DriverResult<RdpDesktop> {
        self.desktop_request(device_id, BridgeOperation::Connect, control)
            .await
    }

    async fn disconnect(&self, device_id: &str, control: &ExecutionControl) -> DriverResult<()> {
        match self
            .request(device_id, BridgeOperation::Disconnect, control)
            .await?
        {
            BridgePayload::Empty => Ok(()),
            BridgePayload::Desktop { .. } | BridgePayload::Frame { .. } => {
                Err(platform("bridge_invalid_response", false))
            }
        }
    }

    async fn probe(&self, device_id: &str, control: &ExecutionControl) -> DriverResult<RdpDesktop> {
        self.desktop_request(device_id, BridgeOperation::Probe, control)
            .await
    }

    async fn capture(&self, device_id: &str, control: &ExecutionControl) -> DriverResult<RdpFrame> {
        self.frame_request(device_id, BridgeOperation::Capture, control)
            .await
    }

    async fn input(
        &self,
        device_id: &str,
        call_id: Uuid,
        input: RdpInput,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        let first = self
            .request(
                device_id,
                BridgeOperation::Input {
                    call_id,
                    input: input.clone(),
                },
                control,
            )
            .await;
        let payload = match first {
            Ok(payload) => payload,
            Err(error) if ambiguous_bridge_delivery(&error) => {
                ensure_active(control)?;
                match self
                    .request(
                        device_id,
                        BridgeOperation::Input { call_id, input },
                        control,
                    )
                    .await
                {
                    Ok(payload) => payload,
                    Err(second) if ambiguous_bridge_delivery(&second) => {
                        return Err(platform("rdp_input_indeterminate", false));
                    }
                    Err(second) => return Err(second),
                }
            }
            Err(error) => return Err(error),
        };
        match payload {
            BridgePayload::Empty => Ok(()),
            BridgePayload::Desktop { .. } | BridgePayload::Frame { .. } => {
                Err(platform("bridge_invalid_response", false))
            }
        }
    }
}

impl SystemRdpBridge {
    async fn desktop_request(
        &self,
        device_id: &str,
        operation: BridgeOperation,
        control: &ExecutionControl,
    ) -> DriverResult<RdpDesktop> {
        match self.request(device_id, operation, control).await? {
            BridgePayload::Desktop {
                width,
                height,
                scale_factor,
                desktop_name,
                server_version,
            } => Ok(RdpDesktop {
                width,
                height,
                scale_factor,
                desktop_name,
                server_version,
            }),
            BridgePayload::Empty | BridgePayload::Frame { .. } => {
                Err(platform("bridge_invalid_response", false))
            }
        }
    }

    async fn frame_request(
        &self,
        device_id: &str,
        operation: BridgeOperation,
        control: &ExecutionControl,
    ) -> DriverResult<RdpFrame> {
        match self.request(device_id, operation, control).await? {
            BridgePayload::Frame {
                width,
                height,
                scale_factor,
                png_base64,
                desktop_name,
                server_version,
            } => {
                let png = run_bounded_blocking(
                    control,
                    move || {
                        let png = BASE64
                            .decode(png_base64)
                            .map_err(|_| platform("bridge_invalid_screenshot", false))?;
                        if png.len() > MAX_SCREENSHOT_BYTES {
                            return Err(platform("bridge_screenshot_too_large", false));
                        }
                        Ok(png)
                    },
                    || platform("bridge_invalid_screenshot", false),
                )
                .await?;
                Ok(RdpFrame {
                    width,
                    height,
                    scale_factor,
                    png,
                    desktop_name,
                    server_version,
                })
            }
            BridgePayload::Empty | BridgePayload::Desktop { .. } => {
                Err(platform("bridge_invalid_response", false))
            }
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeRequest<'a> {
    version: u16,
    operation_id: Uuid,
    authentication_token: &'a str,
    device_id: &'a str,
    target: &'a str,
    timeout_ms: u64,
    #[serde(flatten)]
    operation: BridgeOperation,
}

#[derive(Serialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum BridgeOperation {
    Health,
    Connect,
    Disconnect,
    Probe,
    Capture,
    Input { call_id: Uuid, input: RdpInput },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BridgeResponse {
    version: u16,
    ok: bool,
    #[serde(default)]
    payload: Option<BridgePayload>,
    #[serde(default)]
    error: Option<BridgeFailure>,
}

impl BridgeResponse {
    fn into_result(self) -> DriverResult<BridgePayload> {
        if self.version != BRIDGE_PROTOCOL_VERSION {
            return Err(platform("bridge_version_mismatch", false));
        }
        match (self.ok, self.payload, self.error) {
            (true, Some(payload), None) => Ok(payload),
            (false, None, Some(error)) => Err(error.into_driver_error()),
            _ => Err(platform("bridge_invalid_response", false)),
        }
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum BridgePayload {
    Empty,
    Desktop {
        width: u32,
        height: u32,
        scale_factor: f64,
        #[serde(default)]
        desktop_name: Option<String>,
        #[serde(default)]
        server_version: Option<String>,
    },
    Frame {
        width: u32,
        height: u32,
        scale_factor: f64,
        png_base64: String,
        #[serde(default)]
        desktop_name: Option<String>,
        #[serde(default)]
        server_version: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BridgeFailure {
    code: String,
    retryable: bool,
    #[serde(default)]
    diagnostic: Option<String>,
}

impl BridgeFailure {
    fn into_driver_error(self) -> DriverError {
        let valid_code = !self.code.is_empty()
            && self.code.len() <= MAX_REMOTE_CODE_BYTES
            && self.code.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'-' | b'.')
            });
        let diagnostic_valid = self.diagnostic.as_ref().is_none_or(|value| {
            value.len() <= MAX_DIAGNOSTIC_BYTES && !value.contains('\r') && !value.contains('\n')
        });
        if !valid_code || !diagnostic_valid {
            return platform("bridge_invalid_error", false);
        }
        platform(&format!("rdp_{}", self.code), self.retryable)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RdpBridgeError {
    #[error("invalid RDP target")]
    InvalidTarget,
    #[error("invalid RDP bridge configuration")]
    InvalidConfiguration,
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

fn ambiguous_bridge_delivery(error: &DriverError) -> bool {
    matches!(
        error,
        DriverError::Platform { code, .. }
            if matches!(
                code.as_str(),
                "bridge_write_failed" | "bridge_read_failed" | "bridge_timeout"
            )
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use devicerail_core::{CancellationReason, DriverError, ExecutionControl, ExecutionController};
    use serde_json::json;
    use tokio::{
        io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
        net::TcpListener,
        sync::Notify,
    };
    use uuid::Uuid;

    use super::{BridgeConfig, RdpBridge, RdpInput, RdpTarget, SystemRdpBridge};

    #[test]
    fn target_rejects_credentials_and_non_rdp_urls() {
        for value in [
            "https://host",
            "rdp://user:secret@host",
            "rdp://host/path",
            "rdp://host?x=1",
        ] {
            assert!(RdpTarget::parse(value).is_err(), "{value}");
        }
        let target = RdpTarget::parse("rdp://server.example:3390").expect("target");
        let config =
            BridgeConfig::new("127.0.0.1:7766", target, "secret-token").expect("bridge config");
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret-token"));
        assert!(!debug.contains("server.example"));
        let target = RdpTarget::parse("rdp://server.example").expect("target");
        assert!(BridgeConfig::new("192.0.2.1:7766", target, "secret-token").is_err());
        let ipv6 = RdpTarget::parse("rdp://[::1]").expect("IPv6 target");
        assert_eq!(ipv6.authority(), "[::1]:3389");
        assert!(!format!("{ipv6:?}").contains("::1"));
    }

    #[tokio::test]
    async fn system_bridge_uses_one_bounded_versioned_loopback_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let endpoint = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept bridge request");
            let mut stream = BufReader::new(stream);
            let mut request = String::new();
            stream
                .read_line(&mut request)
                .await
                .expect("read bridge request");
            let request: serde_json::Value = serde_json::from_str(&request).expect("JSON request");
            assert_eq!(request["version"], 2);
            assert_eq!(request["operation"], "connect");
            assert_eq!(request["authenticationToken"], "test-token");
            assert_eq!(request["target"], "rdp.example:3389");
            assert!(
                Uuid::parse_str(request["operationId"].as_str().expect("operation id")).is_ok()
            );
            let response = format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "version": 2,
                    "ok": true,
                    "payload": {
                        "kind": "desktop",
                        "width": 1280,
                        "height": 720,
                        "scaleFactor": 1.0,
                        "desktopName": "Fixture",
                        "serverVersion": "RDP 10"
                    }
                }))
                .expect("response")
            );
            stream
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .expect("write bridge response");
        });
        let target = RdpTarget::parse("rdp://rdp.example").expect("target");
        let bridge = SystemRdpBridge::new(
            BridgeConfig::new(endpoint.to_string(), target, "test-token").expect("config"),
        );
        let desktop = bridge
            .connect("rdp-fixture", &ExecutionControl::unbounded())
            .await
            .expect("bridge desktop");
        assert_eq!((desktop.width, desktop.height), (1280, 720));
        server.await.expect("server task");
    }

    #[test]
    fn bridge_v2_schema_accepts_all_golden_frames() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../protocol/bridge-v2.schema.json"))
                .expect("bridge schema JSON");
        jsonschema::meta::validate(&schema).expect("valid bridge schema");
        let validator = jsonschema::validator_for(&schema).expect("compile bridge schema");
        for fixture in [
            include_str!("../protocol/fixtures/connect.request.json"),
            include_str!("../protocol/fixtures/input-secret.request.json"),
            include_str!("../protocol/fixtures/desktop.response.json"),
        ] {
            let value: serde_json::Value = serde_json::from_str(fixture).expect("fixture JSON");
            assert!(validator.is_valid(&value), "invalid fixture: {fixture}");
        }
    }

    #[tokio::test]
    async fn repeated_call_id_is_available_for_bridge_side_exactly_once_deduplication() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let endpoint = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let mut committed = std::collections::BTreeSet::new();
            let mut applied = 0;
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.expect("accept input");
                let mut stream = BufReader::new(stream);
                let mut request = String::new();
                stream.read_line(&mut request).await.expect("read input");
                let request: serde_json::Value =
                    serde_json::from_str(&request).expect("input JSON");
                let key = (
                    request["deviceId"].as_str().expect("device id").to_owned(),
                    request["callId"].as_str().expect("call id").to_owned(),
                );
                if committed.insert(key) {
                    applied += 1;
                }
                let response = format!(
                    "{}\n",
                    serde_json::to_string(&json!({
                        "version": 2,
                        "ok": true,
                        "payload": { "kind": "empty" }
                    }))
                    .expect("response")
                );
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .expect("write input response");
            }
            applied
        });
        let target = RdpTarget::parse("rdp://rdp.example").expect("target");
        let bridge = SystemRdpBridge::new(
            BridgeConfig::new(endpoint.to_string(), target, "test-token").expect("config"),
        );
        let call_id = Uuid::new_v4();
        for _ in 0..2 {
            bridge
                .input(
                    "rdp-fixture",
                    call_id,
                    RdpInput::Tap { x: 1, y: 1 },
                    &ExecutionControl::unbounded(),
                )
                .await
                .expect("idempotent input response");
        }
        assert_eq!(server.await.expect("server task"), 1);
    }

    #[tokio::test]
    async fn lost_commit_response_retries_once_with_the_same_call_id() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let endpoint = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let mut committed = std::collections::BTreeSet::new();
            let mut seen_call_ids = Vec::new();
            for attempt in 0..2 {
                let (stream, _) = listener.accept().await.expect("accept input");
                let mut stream = BufReader::new(stream);
                let mut request = String::new();
                stream.read_line(&mut request).await.expect("read input");
                let request: serde_json::Value =
                    serde_json::from_str(&request).expect("input JSON");
                let call_id = request["callId"].as_str().expect("call id").to_owned();
                committed.insert(call_id.clone());
                seen_call_ids.push(call_id);
                if attempt == 0 {
                    // Simulate a bridge that committed input and lost the
                    // response. Closing the socket makes delivery ambiguous.
                    continue;
                }
                let response = format!(
                    "{}\n",
                    serde_json::to_string(&json!({
                        "version": 2,
                        "ok": true,
                        "payload": { "kind": "empty" }
                    }))
                    .expect("response")
                );
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .expect("write cached response");
            }
            (committed.len(), seen_call_ids)
        });
        let target = RdpTarget::parse("rdp://rdp.example").expect("target");
        let bridge = SystemRdpBridge::new(
            BridgeConfig::new(endpoint.to_string(), target, "test-token").expect("config"),
        );
        let call_id = Uuid::new_v4();
        bridge
            .input(
                "rdp-fixture",
                call_id,
                RdpInput::Tap { x: 1, y: 1 },
                &ExecutionControl::unbounded(),
            )
            .await
            .expect("cached commit result");
        let (applied, seen) = server.await.expect("server task");
        assert_eq!(applied, 1);
        assert_eq!(seen, vec![call_id.to_string(), call_id.to_string()]);
    }

    #[tokio::test]
    async fn input_connect_failure_is_retryable_but_never_indeterminate() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve loopback endpoint");
        let endpoint = listener.local_addr().expect("listener address");
        drop(listener);

        let target = RdpTarget::parse("rdp://rdp.example").expect("target");
        let bridge = SystemRdpBridge::new(
            BridgeConfig::new(endpoint.to_string(), target, "test-token").expect("config"),
        );
        assert!(matches!(
            bridge
                .input(
                    "rdp-fixture",
                    Uuid::new_v4(),
                    RdpInput::Tap { x: 1, y: 1 },
                    &ExecutionControl::unbounded(),
                )
                .await,
            Err(DriverError::Platform {
                code,
                retryable: true,
            }) if code == "bridge_connect_failed"
        ));
    }

    #[tokio::test]
    async fn cancelling_input_closes_the_socket_as_bridge_v2_cancel_signal() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let endpoint = listener.local_addr().expect("listener address");
        let request_seen = Arc::new(Notify::new());
        let server_seen = Arc::clone(&request_seen);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept input");
            let mut stream = BufReader::new(stream);
            let mut request = String::new();
            stream.read_line(&mut request).await.expect("read input");
            server_seen.notify_one();
            let mut byte = [0_u8; 1];
            let read = stream
                .get_mut()
                .read(&mut byte)
                .await
                .expect("read cancellation EOF");
            assert_eq!(read, 0, "cancel must close the bridge socket");
        });
        let target = RdpTarget::parse("rdp://rdp.example").expect("target");
        let bridge = Arc::new(SystemRdpBridge::new(
            BridgeConfig::new(endpoint.to_string(), target, "test-token").expect("config"),
        ));
        let (controller, control) = ExecutionController::new();
        let task_bridge = Arc::clone(&bridge);
        let input = tokio::spawn(async move {
            task_bridge
                .input(
                    "rdp-fixture",
                    Uuid::new_v4(),
                    RdpInput::Tap { x: 1, y: 1 },
                    &control,
                )
                .await
        });
        request_seen.notified().await;
        assert!(controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            input.await.expect("input task"),
            Err(DriverError::Cancelled)
        ));
        server.await.expect("server task");
    }
}
