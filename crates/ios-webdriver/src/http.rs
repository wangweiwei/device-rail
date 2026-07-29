use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devicerail_core::{DriverError, DriverResult, ExecutionControl, run_bounded_blocking};
use devicerail_protocol::Viewport;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use crate::{
    HttpEndpointConfig, WdaAction, WdaPage, WdaSession, WdaStatus, WdaTransport,
    control::{platform, run_controlled},
    transport::IosKey,
};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_JSON_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_SCREENSHOT_BODY_BYTES: usize = 48 * 1024 * 1024;
const MAX_SCREENSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_VIEWPORT_DIMENSION: u32 = 16_384;

#[derive(Clone, Debug)]
pub struct SystemWdaTransport {
    endpoint: HttpEndpointConfig,
}

impl SystemWdaTransport {
    pub fn new(endpoint: HttpEndpointConfig) -> Self {
        Self { endpoint }
    }

    pub fn endpoint(&self) -> &HttpEndpointConfig {
        &self.endpoint
    }

    async fn json_request(
        &self,
        method: Method,
        suffix: &str,
        body: Option<Value>,
        max_response_bytes: usize,
        control: &ExecutionControl,
    ) -> DriverResult<Value> {
        self.json_request_with_progress(method, suffix, body, max_response_bytes, control, None)
            .await
    }

    async fn json_request_with_progress(
        &self,
        method: Method,
        suffix: &str,
        body: Option<Value>,
        max_response_bytes: usize,
        control: &ExecutionControl,
        request_started: Option<&AtomicBool>,
    ) -> DriverResult<Value> {
        let route = self.endpoint.route(suffix)?;
        let body = body
            .map(|value| {
                serde_json::to_vec(&value).map_err(|_| {
                    DriverError::Internal("could not serialize WDA request".to_owned())
                })
            })
            .transpose()?;
        if body.as_ref().is_some_and(|bytes| bytes.len() > 256 * 1024) {
            return Err(platform("wda_request_too_large", false));
        }
        let response = run_controlled(
            control,
            self.endpoint.request_timeout_ms(),
            "wda_transport_timeout",
            request_http(
                &self.endpoint,
                method,
                &route,
                body.as_deref(),
                max_response_bytes,
                request_started,
            ),
        )
        .await?;
        let root: Value = serde_json::from_slice(&response.body).map_err(|_| {
            if (200..300).contains(&response.status) {
                platform("wda_invalid_json", false)
            } else {
                platform("wda_http_status", response.status >= 500)
            }
        })?;
        if let Some(error) = webdriver_error(&root, response.status) {
            return Err(error);
        }
        if !(200..300).contains(&response.status) {
            return Err(platform("wda_http_status", response.status >= 500));
        }
        Ok(root)
    }

    async fn mutation_json_request(
        &self,
        method: Method,
        suffix: &str,
        body: Option<Value>,
        max_response_bytes: usize,
        control: &ExecutionControl,
    ) -> DriverResult<Value> {
        let request_started = AtomicBool::new(false);
        self.json_request_with_progress(
            method,
            suffix,
            body,
            max_response_bytes,
            control,
            Some(&request_started),
        )
        .await
        .map_err(|error| {
            map_ambiguous_mutation_error(error, request_started.load(Ordering::Acquire))
        })
    }

    async fn send_keys(
        &self,
        session: &WdaSession,
        text: &str,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        let route = session_route(session, "/wda/keys");
        let value = text
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        self.mutation_json_request(
            Method::Post,
            &route,
            Some(json!({ "value": value })),
            MAX_JSON_BODY_BYTES,
            control,
        )
        .await?;
        Ok(())
    }

    async fn viewport(
        &self,
        session: &WdaSession,
        control: &ExecutionControl,
    ) -> DriverResult<Viewport> {
        let size_root = self
            .json_request(
                Method::Get,
                &session_route(session, "/window/size"),
                None,
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        let size = size_root
            .get("value")
            .ok_or_else(|| platform("wda_invalid_viewport", false))?;
        let width = json_u32(size.get("width"))?;
        let height = json_u32(size.get("height"))?;
        if width == 0
            || height == 0
            || width > MAX_VIEWPORT_DIMENSION
            || height > MAX_VIEWPORT_DIMENSION
        {
            return Err(platform("wda_invalid_viewport", false));
        }
        Ok(Viewport {
            width,
            height,
            scale_factor: 1.0,
        })
    }
}

#[async_trait]
impl WdaTransport for SystemWdaTransport {
    async fn status(&self, control: &ExecutionControl) -> DriverResult<WdaStatus> {
        let root = self
            .json_request(Method::Get, "/status", None, MAX_JSON_BODY_BYTES, control)
            .await?;
        let value = root.get("value").unwrap_or(&root);
        let ready = value.get("ready").and_then(Value::as_bool).unwrap_or(false);
        let os_version = value
            .get("os")
            .and_then(|os| os.get("version"))
            .and_then(Value::as_str)
            .filter(|version| version.len() <= 256 && !version.chars().any(char::is_control))
            .map(str::to_owned);
        Ok(WdaStatus { ready, os_version })
    }

    async fn create_session(&self, control: &ExecutionControl) -> DriverResult<WdaSession> {
        let root = self
            .mutation_json_request(
                Method::Post,
                "/session",
                Some(json!({
                    "capabilities": {
                        "alwaysMatch": { "platformName": "iOS" },
                        "firstMatch": [{}]
                    }
                })),
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        let id = root
            .get("sessionId")
            .or_else(|| root.get("value").and_then(|value| value.get("sessionId")))
            .and_then(Value::as_str)
            .ok_or_else(|| platform("wda_missing_session", false))?;
        WdaSession::parse(id).map_err(|_| platform("wda_invalid_session_response", false))
    }

    async fn delete_session(
        &self,
        session: &WdaSession,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        self.mutation_json_request(
            Method::Delete,
            &session_route(session, ""),
            None,
            MAX_JSON_BODY_BYTES,
            control,
        )
        .await?;
        Ok(())
    }

    async fn probe_session(
        &self,
        session: &WdaSession,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        self.viewport(session, control).await.map(drop)
    }

    async fn inspect(
        &self,
        session: &WdaSession,
        control: &ExecutionControl,
    ) -> DriverResult<WdaPage> {
        let source_root = self
            .json_request(
                Method::Get,
                &session_route(session, "/source"),
                None,
                MAX_SOURCE_BYTES + 1024,
                control,
            )
            .await?;
        let source = source_root
            .get("value")
            .and_then(Value::as_str)
            .filter(|source| source.len() <= MAX_SOURCE_BYTES)
            .ok_or_else(|| platform("wda_invalid_source", false))?
            .to_owned();
        let viewport = self.viewport(session, control).await?;
        Ok(WdaPage { source, viewport })
    }

    async fn screenshot_png(
        &self,
        session: &WdaSession,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<u8>> {
        let mut root = self
            .json_request(
                Method::Get,
                &session_route(session, "/screenshot"),
                None,
                MAX_SCREENSHOT_BODY_BYTES,
                control,
            )
            .await?;
        let encoded = match root.get_mut("value").map(Value::take) {
            Some(Value::String(encoded)) => encoded,
            _ => return Err(platform("wda_invalid_screenshot", false)),
        };
        run_bounded_blocking(
            control,
            move || {
                let bytes = BASE64
                    .decode(encoded)
                    .map_err(|_| platform("wda_invalid_screenshot", false))?;
                if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
                    return Err(platform("wda_invalid_screenshot", false));
                }
                Ok(bytes)
            },
            || platform("wda_invalid_screenshot", false),
        )
        .await
    }

    async fn perform(
        &self,
        session: &WdaSession,
        action: WdaAction,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        match action {
            WdaAction::Tap { x, y } => {
                self.mutation_json_request(
                    Method::Post,
                    &session_route(session, "/wda/tap"),
                    Some(json!({ "x": x, "y": y })),
                    MAX_JSON_BODY_BYTES,
                    control,
                )
                .await?;
            }
            WdaAction::TypeText(text) => self.send_keys(session, &text, control).await?,
            WdaAction::PressKey(key) => match key {
                IosKey::Home | IosKey::VolumeUp | IosKey::VolumeDown => {
                    let name = match key {
                        IosKey::Home => "home",
                        IosKey::VolumeUp => "volumeUp",
                        IosKey::VolumeDown => "volumeDown",
                        _ => unreachable!(),
                    };
                    self.mutation_json_request(
                        Method::Post,
                        &session_route(session, "/wda/pressButton"),
                        Some(json!({ "name": name })),
                        MAX_JSON_BODY_BYTES,
                        control,
                    )
                    .await?;
                }
                key => {
                    let webdriver_key = match key {
                        IosKey::Enter => "\u{e007}",
                        IosKey::Tab => "\u{e004}",
                        IosKey::Escape => "\u{e00c}",
                        IosKey::Delete => "\u{e003}",
                        IosKey::Space => " ",
                        _ => unreachable!(),
                    };
                    self.send_keys(session, webdriver_key, control).await?;
                }
            },
            WdaAction::Drag {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
            } => {
                self.mutation_json_request(
                    Method::Post,
                    &session_route(session, "/wda/dragfromtoforduration"),
                    Some(json!({
                        "fromX": start_x,
                        "fromY": start_y,
                        "toX": end_x,
                        "toY": end_y,
                        "duration": f64::from(duration_ms) / 1_000.0
                    })),
                    MAX_JSON_BODY_BYTES,
                    control,
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

async fn request_http(
    endpoint: &HttpEndpointConfig,
    method: Method,
    route: &str,
    body: Option<&[u8]>,
    max_response_bytes: usize,
    request_started: Option<&AtomicBool>,
) -> DriverResult<HttpResponse> {
    let mut stream = TcpStream::connect((endpoint.host(), endpoint.port()))
        .await
        .map_err(|_| platform("wda_connect_failed", true))?;
    let body = body.unwrap_or_default();
    let request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        method.as_str(),
        route,
        endpoint.authority(),
        body.len()
    );
    if let Some(request_started) = request_started {
        // A write failure can be partial. Once the first write is attempted,
        // only a definitive WDA response can prove a mutation did not run.
        request_started.store(true, Ordering::Release);
    }
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| platform("wda_write_failed", true))?;
    if !body.is_empty() {
        stream
            .write_all(body)
            .await
            .map_err(|_| platform("wda_write_failed", true))?;
    }
    // Keep both directions open until WDA has returned its response. usbmuxd
    // treats a client write-side shutdown as the end of the proxied device
    // connection, so half-closing here can discard WDA's response before its
    // headers reach the host.
    read_http_response(&mut stream, max_response_bytes, "wda_read_failed").await
}

pub(crate) async fn read_http_head(
    stream: &mut TcpStream,
    read_code: &'static str,
) -> DriverResult<(u16, BTreeMap<String, String>, Vec<u8>)> {
    let mut bytes = Vec::with_capacity(4 * 1024);
    let header_end = loop {
        if let Some(index) = find_subslice(&bytes, b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err(platform("http_headers_too_large", false));
        }
        let mut chunk = [0_u8; 4 * 1024];
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|_| platform(read_code, true))?;
        if read == 0 {
            return Err(platform("http_truncated_headers", true));
        }
        bytes.extend_from_slice(&chunk[..read]);
    };
    let head = std::str::from_utf8(&bytes[..header_end - 4])
        .map_err(|_| platform("http_invalid_headers", false))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| platform("http_invalid_headers", false))?;
    let mut status_fields = status_line.split_whitespace();
    let version = status_fields.next().unwrap_or_default();
    let status = status_fields
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| (100..=599).contains(value))
        .ok_or_else(|| platform("http_invalid_headers", false))?;
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(platform("http_invalid_headers", false));
    }
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| platform("http_invalid_headers", false))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        if name.is_empty() || headers.insert(name, value).is_some() {
            return Err(platform("http_invalid_headers", false));
        }
    }
    Ok((status, headers, bytes[header_end..].to_vec()))
}

async fn read_http_response(
    stream: &mut TcpStream,
    max_body_bytes: usize,
    read_code: &'static str,
) -> DriverResult<HttpResponse> {
    let (status, headers, mut body) = read_http_head(stream, read_code).await?;
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        read_to_end_bounded(stream, &mut body, max_body_bytes * 2, read_code).await?;
        body = decode_chunked(&body, max_body_bytes)?;
    } else if let Some(content_length) = headers.get("content-length") {
        let length = content_length
            .parse::<usize>()
            .ok()
            .filter(|length| *length <= max_body_bytes)
            .ok_or_else(|| platform("http_invalid_content_length", false))?;
        while body.len() < length {
            read_more_bounded(stream, &mut body, length, read_code).await?;
        }
        body.truncate(length);
    } else {
        read_to_end_bounded(stream, &mut body, max_body_bytes, read_code).await?;
    }
    if body.len() > max_body_bytes {
        return Err(platform("http_body_too_large", false));
    }
    Ok(HttpResponse { status, body })
}

pub(crate) async fn read_more_bounded(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    max_bytes: usize,
    read_code: &'static str,
) -> DriverResult<()> {
    if bytes.len() >= max_bytes {
        return Err(platform("http_body_too_large", false));
    }
    let remaining = (max_bytes - bytes.len()).min(16 * 1024);
    let mut chunk = vec![0_u8; remaining];
    let read = stream
        .read(&mut chunk)
        .await
        .map_err(|_| platform(read_code, true))?;
    if read == 0 {
        return Err(platform("http_truncated_body", true));
    }
    bytes.extend_from_slice(&chunk[..read]);
    Ok(())
}

async fn read_to_end_bounded(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    max_bytes: usize,
    read_code: &'static str,
) -> DriverResult<()> {
    loop {
        if bytes.len() > max_bytes {
            return Err(platform("http_body_too_large", false));
        }
        let mut chunk = [0_u8; 16 * 1024];
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|_| platform(read_code, true))?;
        if read == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

fn decode_chunked(bytes: &[u8], max_body_bytes: usize) -> DriverResult<Vec<u8>> {
    let mut cursor = 0_usize;
    let mut output = Vec::new();
    loop {
        let line_end = find_subslice(&bytes[cursor..], b"\r\n")
            .map(|index| cursor + index)
            .ok_or_else(|| platform("http_invalid_chunked_body", false))?;
        let size_text = std::str::from_utf8(&bytes[cursor..line_end])
            .ok()
            .and_then(|line| line.split(';').next())
            .ok_or_else(|| platform("http_invalid_chunked_body", false))?;
        let size = usize::from_str_radix(size_text.trim(), 16)
            .map_err(|_| platform("http_invalid_chunked_body", false))?;
        cursor = line_end + 2;
        if size == 0 {
            return Ok(output);
        }
        let end = cursor
            .checked_add(size)
            .filter(|end| end.checked_add(2).is_some_and(|tail| tail <= bytes.len()))
            .ok_or_else(|| platform("http_invalid_chunked_body", false))?;
        if &bytes[end..end + 2] != b"\r\n" || output.len() + size > max_body_bytes {
            return Err(platform("http_invalid_chunked_body", false));
        }
        output.extend_from_slice(&bytes[cursor..end]);
        cursor = end + 2;
    }
}

fn webdriver_error(root: &Value, status: u16) -> Option<DriverError> {
    let error = root
        .get("value")
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)?;
    Some(match error {
        "invalid session id" => platform("wda_invalid_session", false),
        "session not created" => platform("wda_session_not_created", true),
        "timeout" | "script timeout" => platform("wda_webdriver_timeout", true),
        "unknown command" | "unsupported operation" => platform("wda_unsupported_command", false),
        _ => platform("wda_webdriver_error", status >= 500),
    })
}

fn map_ambiguous_mutation_error(error: DriverError, request_started: bool) -> DriverError {
    if !request_started {
        return error;
    }
    match &error {
        DriverError::Cancelled | DriverError::TimedOut => command_outcome_unknown(),
        DriverError::Platform { code, .. }
            if matches!(
                code.as_str(),
                "wda_write_failed"
                    | "wda_read_failed"
                    | "wda_transport_timeout"
                    | "wda_invalid_json"
                    | "http_truncated_headers"
                    | "http_truncated_body"
                    | "http_headers_too_large"
                    | "http_invalid_headers"
                    | "http_invalid_content_length"
                    | "http_invalid_chunked_body"
                    | "http_body_too_large"
            ) =>
        {
            command_outcome_unknown()
        }
        _ => error,
    }
}

fn command_outcome_unknown() -> DriverError {
    platform("wda_command_outcome_unknown", false)
}

pub(crate) fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        Some(0)
    } else {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}

fn session_route(session: &WdaSession, suffix: &str) -> String {
    format!("/session/{}{suffix}", session.as_str())
}

fn json_u32(value: Option<&Value>) -> DriverResult<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| platform("wda_invalid_viewport", false))
}

#[cfg(test)]
mod tests {
    use std::{future::pending, time::Duration};

    use devicerail_core::{CancellationReason, DriverError, ExecutionControl, ExecutionController};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::timeout,
    };

    use super::{SystemWdaTransport, decode_chunked, webdriver_error};
    use crate::{HttpEndpointConfig, WdaAction, WdaSession, WdaTransport};

    #[test]
    fn chunked_decoder_is_bounded_and_strict() {
        assert_eq!(
            decode_chunked(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n", 9).expect("decode"),
            b"Wikipedia"
        );
        assert!(decode_chunked(b"4\r\nWiki0\r\n\r\n", 32).is_err());
        assert!(decode_chunked(b"a\r\n0123456789\r\n0\r\n\r\n", 9).is_err());
    }

    #[test]
    fn webdriver_invalid_session_is_a_stable_explicit_error() {
        let error = webdriver_error(
            &serde_json::json!({
                "value": {
                    "error": "invalid session id",
                    "message": "session is not active"
                }
            }),
            404,
        )
        .expect("WebDriver error");
        assert!(matches!(
            error,
            DriverError::Platform { code, retryable: false }
                if code == "wda_invalid_session"
        ));
    }

    #[tokio::test]
    async fn system_transport_sends_a_bounded_status_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = vec![0_u8; 2 * 1024];
            let read = socket.read(&mut request).await.expect("read request");
            let request = std::str::from_utf8(&request[..read]).expect("HTTP request");
            assert!(request.starts_with("GET /wd/status HTTP/1.1\r\n"));
            assert!(request.contains("Content-Length: 0\r\n"));
            let body = br#"{"value":{"ready":true,"os":{"version":"18.1"}}}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write headers");
            socket.write_all(body).await.expect("write body");
        });
        let transport = SystemWdaTransport::new(
            HttpEndpointConfig::new(format!("http://{address}/wd")).expect("endpoint"),
        );
        let status = transport
            .status(&ExecutionControl::unbounded())
            .await
            .expect("status");
        assert!(status.ready);
        assert_eq!(status.os_version.as_deref(), Some("18.1"));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn system_transport_keeps_write_side_open_until_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0_u8; 512];
                let read = socket.read(&mut chunk).await.expect("read request");
                assert_ne!(read, 0, "request ended before its headers");
                request.extend_from_slice(&chunk[..read]);
            }

            let mut next = [0_u8; 1];
            match timeout(Duration::from_millis(50), socket.read(&mut next)).await {
                Ok(Ok(0)) => return,
                Ok(Ok(_)) => panic!("unexpected request body"),
                Ok(Err(error)) => panic!("failed to probe request connection: {error}"),
                Err(_) => {}
            }

            let body = br#"{"value":{"ready":true}}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write headers");
            socket.write_all(body).await.expect("write body");
        });
        let transport = SystemWdaTransport::new(
            HttpEndpointConfig::new(format!("http://{address}")).expect("endpoint"),
        );

        let status = transport
            .status(&ExecutionControl::unbounded())
            .await
            .expect("status");

        assert!(status.ready);
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn system_transport_uses_the_current_appium_wda_tap_route() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let header_end = loop {
                if let Some(index) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                    break index + 4;
                }
                let mut chunk = [0_u8; 512];
                let read = socket.read(&mut chunk).await.expect("read request");
                assert_ne!(read, 0, "request ended before its headers");
                request.extend_from_slice(&chunk[..read]);
            };
            let headers = std::str::from_utf8(&request[..header_end]).expect("request headers");
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length: ")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .expect("content length");
            while request.len() < header_end + content_length {
                let mut chunk = [0_u8; 512];
                let read = socket.read(&mut chunk).await.expect("read request body");
                assert_ne!(read, 0, "request ended before its body");
                request.extend_from_slice(&chunk[..read]);
            }
            let request = std::str::from_utf8(&request).expect("HTTP request");
            assert!(request.starts_with("POST /session/session-1/wda/tap HTTP/1.1\r\n"));
            assert!(request.ends_with(r#"{"x":153,"y":785}"#));

            let body = br#"{"value":null}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write headers");
            socket.write_all(body).await.expect("write body");
        });
        let transport = SystemWdaTransport::new(
            HttpEndpointConfig::new(format!("http://{address}")).expect("endpoint"),
        );

        transport
            .perform(
                &WdaSession::parse("session-1").expect("session"),
                WdaAction::Tap { x: 153, y: 785 },
                &ExecutionControl::unbounded(),
            )
            .await
            .expect("tap");

        server.await.expect("server task");
    }

    #[tokio::test]
    async fn system_transport_parses_invalid_session_before_http_status() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0_u8; 2 * 1024];
            let read = socket.read(&mut request).await.expect("read request");
            let request = std::str::from_utf8(&request[..read]).expect("HTTP request");
            assert!(request.starts_with("GET /session/stale/window/size HTTP/1.1\r\n"));
            let body = br#"{"value":{"error":"invalid session id","message":"gone"}}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write headers");
            socket.write_all(body).await.expect("write body");
        });
        let transport = SystemWdaTransport::new(
            HttpEndpointConfig::new(format!("http://{address}")).expect("endpoint"),
        );

        let error = transport
            .probe_session(
                &WdaSession::parse("stale").expect("session"),
                &ExecutionControl::unbounded(),
            )
            .await
            .expect_err("stale Session must fail");

        assert!(matches!(
            error,
            DriverError::Platform { code, retryable: false }
                if code == "wda_invalid_session"
        ));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn interrupted_mutation_is_an_unknown_outcome_and_is_not_replayed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0_u8; 512];
                let read = socket.read(&mut chunk).await.expect("read request");
                assert_ne!(read, 0, "request ended before headers");
                request.extend_from_slice(&chunk[..read]);
            }
            assert!(
                std::str::from_utf8(&request)
                    .expect("HTTP request")
                    .starts_with("POST /session/session-1/wda/tap HTTP/1.1\r\n")
            );
            received_tx.send(()).expect("signal request received");
            pending::<()>().await;
        });
        let transport = SystemWdaTransport::new(
            HttpEndpointConfig::new(format!("http://{address}")).expect("endpoint"),
        );
        let (controller, control) = ExecutionController::new();
        let session = WdaSession::parse("session-1").expect("session");
        let mutation = transport.perform(&session, WdaAction::Tap { x: 1, y: 2 }, &control);
        tokio::pin!(mutation);
        tokio::select! {
            received = received_rx => received.expect("request signal"),
            result = &mut mutation => panic!("mutation completed before cancellation: {result:?}"),
        }
        assert!(controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            mutation.await,
            Err(DriverError::Platform { code, retryable: false })
                if code == "wda_command_outcome_unknown"
        ));
        server.abort();
        let _ = server.await;
    }
}
