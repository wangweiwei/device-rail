use std::{
    fmt::Write as _,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use devicerail_core::{CancellationReason, ExecutionController, TimeoutScope};
use devicerail_session_bundle::{BundleError, read_validated_asset};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::{JoinHandle, JoinSet},
    time,
};
use uuid::Uuid;

use crate::{
    OfflineVisualizer, PageKind, PageQuery, STYLESHEET, VisualizerError,
    png::{PngError, PngPreviewLimits, validate_preview_png},
};

const MAX_HEADER_BYTES_CEILING: usize = 8 * 1024;
const MAX_TARGET_BYTES_CEILING: usize = 2 * 1024;
const MAX_HEADERS_CEILING: usize = 32;
const MAX_CONNECTIONS_CEILING: usize = 32;
const MAX_RENDER_REQUESTS_CEILING: usize = 2;
const MAX_ASSET_REQUESTS_CEILING: usize = 2;
const MAX_HTML_BYTES_CEILING: usize = 2 * 1024 * 1024;
const MAX_INLINE_ASSET_BYTES_CEILING: u64 = 32 * 1024 * 1024;
const MAX_DOWNLOAD_ASSET_BYTES_CEILING: u64 = 64 * 1024 * 1024;
const MAX_REQUEST_TIMEOUT_MS: u64 = 60_000;
const MAX_SHUTDOWN_GRACE_MS: u64 = 10_000;

const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; img-src 'self'; style-src 'self'; script-src 'none'; connect-src 'none'; media-src 'none'; object-src 'none'; frame-src 'none'; font-src 'none'; worker-src 'none'; manifest-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Fixed ceilings for the local read-only HTTP capability server.
///
/// Callers may lower these values for a particular session. Values above the
/// documented absolute ceilings are rejected rather than silently widening
/// the Viewer attack surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerLimits {
    pub port: u16,
    pub max_header_bytes: usize,
    pub max_target_bytes: usize,
    pub max_headers: usize,
    pub request_timeout_ms: u64,
    pub shutdown_grace_ms: u64,
    pub max_connections: usize,
    pub max_render_requests: usize,
    pub max_asset_requests: usize,
    pub max_html_bytes: usize,
    pub max_inline_asset_bytes: u64,
    pub max_download_asset_bytes: u64,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            port: 0,
            max_header_bytes: MAX_HEADER_BYTES_CEILING,
            max_target_bytes: MAX_TARGET_BYTES_CEILING,
            max_headers: MAX_HEADERS_CEILING,
            request_timeout_ms: 5_000,
            shutdown_grace_ms: 2_000,
            max_connections: MAX_CONNECTIONS_CEILING,
            max_render_requests: MAX_RENDER_REQUESTS_CEILING,
            max_asset_requests: MAX_ASSET_REQUESTS_CEILING,
            max_html_bytes: MAX_HTML_BYTES_CEILING,
            max_inline_asset_bytes: MAX_INLINE_ASSET_BYTES_CEILING,
            max_download_asset_bytes: MAX_DOWNLOAD_ASSET_BYTES_CEILING,
        }
    }
}

impl ServerLimits {
    fn validate(self) -> Result<Self, ServerError> {
        if self.max_header_bytes == 0
            || self.max_header_bytes > MAX_HEADER_BYTES_CEILING
            || self.max_target_bytes == 0
            || self.max_target_bytes > MAX_TARGET_BYTES_CEILING
            || self.max_headers == 0
            || self.max_headers > MAX_HEADERS_CEILING
            || self.request_timeout_ms == 0
            || self.request_timeout_ms > MAX_REQUEST_TIMEOUT_MS
            || self.shutdown_grace_ms == 0
            || self.shutdown_grace_ms > MAX_SHUTDOWN_GRACE_MS
            || self.max_connections == 0
            || self.max_connections > MAX_CONNECTIONS_CEILING
            || self.max_render_requests == 0
            || self.max_render_requests > MAX_RENDER_REQUESTS_CEILING
            || self.max_render_requests > self.max_connections
            || self.max_asset_requests == 0
            || self.max_asset_requests > MAX_ASSET_REQUESTS_CEILING
            || self.max_asset_requests > self.max_connections
            || self.max_html_bytes == 0
            || self.max_html_bytes > MAX_HTML_BYTES_CEILING
            || self.max_inline_asset_bytes == 0
            || self.max_inline_asset_bytes > MAX_INLINE_ASSET_BYTES_CEILING
            || self.max_download_asset_bytes == 0
            || self.max_download_asset_bytes > MAX_DOWNLOAD_ASSET_BYTES_CEILING
        {
            return Err(ServerError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("Visualizer server limits are invalid")]
    InvalidLimits,
    #[error("the Visualizer could not bind its loopback listener")]
    Bind(#[source] io::Error),
    #[error("the Visualizer loopback listener failed")]
    Accept(#[source] io::Error),
    #[error("the Visualizer server task failed")]
    Task,
    #[error("the Visualizer did not shut down within its configured grace period")]
    ShutdownTimedOut,
}

/// Running loopback Viewer. Dropping the handle stops admission and aborts
/// any remaining task; [`shutdown`](Self::shutdown) performs bounded cleanup.
pub struct ViewerServer {
    addr: SocketAddr,
    base_path: String,
    url: String,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), ServerError>>>,
    shutdown_grace: Duration,
}

impl ViewerServer {
    pub async fn bind(
        viewer: OfflineVisualizer,
        limits: ServerLimits,
    ) -> Result<Self, ServerError> {
        let limits = limits.validate()?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, limits.port))
            .await
            .map_err(ServerError::Bind)?;
        let addr = listener.local_addr().map_err(ServerError::Bind)?;
        if addr.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err(ServerError::Bind(io::Error::other(
                "listener is not IPv4 loopback",
            )));
        }

        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let base_path = format!("/v/{token}");
        let expected_host = format!("127.0.0.1:{}", addr.port());
        let url = format!("http://{expected_host}{base_path}");
        let (shutdown, receiver) = watch::channel(false);
        let state = Arc::new(ServerState {
            viewer: Arc::new(viewer),
            limits,
            base_path: base_path.clone(),
            expected_host,
            connections: Arc::new(Semaphore::new(limits.max_connections)),
            renders: Arc::new(Semaphore::new(limits.max_render_requests)),
            assets: Arc::new(Semaphore::new(limits.max_asset_requests)),
        });
        let task = tokio::spawn(accept_loop(listener, state, receiver));

        Ok(Self {
            addr,
            base_path,
            url,
            shutdown,
            task: Some(task),
            shutdown_grace: Duration::from_millis(limits.shutdown_grace_ms),
        })
    }

    pub const fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Stop admission and wait for all tracked connections. Calling this more
    /// than once is a no-op after the first successful join.
    pub async fn shutdown(&mut self) -> Result<(), ServerError> {
        let _ = self.shutdown.send(true);
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        let outer_grace = self.shutdown_grace.saturating_add(Duration::from_secs(1));
        match time::timeout(outer_grace, &mut task).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ServerError::Task),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err(ServerError::ShutdownTimedOut)
            }
        }
    }
}

impl Drop for ViewerServer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct ServerState {
    viewer: Arc<OfflineVisualizer>,
    limits: ServerLimits,
    base_path: String,
    expected_host: String,
    connections: Arc<Semaphore>,
    renders: Arc<Semaphore>,
    assets: Arc<Semaphore>,
}

async fn accept_loop(
    listener: TcpListener,
    state: Arc<ServerState>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), ServerError> {
    let mut tasks = JoinSet::new();
    let mut terminal_error = None;
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        terminal_error = Some(ServerError::Accept(error));
                        break;
                    }
                };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let Ok(permit) = Arc::clone(&state.connections).try_acquire_owned() else {
                    continue;
                };
                tasks.spawn(handle_connection(
                    stream,
                    Arc::clone(&state),
                    shutdown.clone(),
                    permit,
                ));
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    drop(listener);

    let grace = Duration::from_millis(state.limits.shutdown_grace_ms);
    let drained = time::timeout(grace, async {
        while tasks.join_next().await.is_some() {}
        wait_for_memory_permits(&state).await;
    })
    .await;
    if drained.is_err() {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        time::timeout(Duration::from_millis(250), wait_for_memory_permits(&state))
            .await
            .map_err(|_| ServerError::ShutdownTimedOut)?;
    }
    terminal_error.map_or(Ok(()), Err)
}

async fn wait_for_memory_permits(state: &ServerState) {
    let render_permits =
        u32::try_from(state.limits.max_render_requests).expect("render semaphore ceiling fits u32");
    let asset_permits =
        u32::try_from(state.limits.max_asset_requests).expect("asset semaphore ceiling fits u32");
    let _renders = Arc::clone(&state.renders)
        .acquire_many_owned(render_permits)
        .await;
    let _assets = Arc::clone(&state.assets)
        .acquire_many_owned(asset_permits)
        .await;
}

async fn handle_connection(
    mut stream: TcpStream,
    state: Arc<ServerState>,
    mut shutdown: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
) {
    let duration = Duration::from_millis(state.limits.request_timeout_ms);
    let mut request_shutdown = shutdown.clone();
    let result = tokio::select! {
        _ = shutdown.changed() => return,
        result = time::timeout(duration, serve_one(&mut stream, &state, &mut request_shutdown)) => result,
    };
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let response = response_for_request_error(error);
            let _ = time::timeout(duration, write_response(&mut stream, response)).await;
        }
        Err(_) => {
            let _ = time::timeout(
                duration,
                write_response(&mut stream, Response::text(408, "Request Timeout")),
            )
            .await;
        }
    }
    let _ = stream.shutdown().await;
}

async fn serve_one(
    stream: &mut TcpStream,
    state: &ServerState,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), RequestError> {
    let request = read_request(stream, state.limits).await?;
    if request.host != state.expected_host {
        return Err(RequestError::NotFound);
    }
    if !target_has_capability(&request.target, &state.base_path) {
        return Err(RequestError::NotFound);
    }
    if request.method != "GET" {
        return Err(RequestError::MethodNotAllowed);
    }

    let response = route_request(&request.target, state, shutdown).await?;
    write_response(stream, response)
        .await
        .map_err(|_| RequestError::Write)
}

fn target_has_capability(target: &str, base_path: &str) -> bool {
    let path = target.split('?').next().unwrap_or_default();
    path == base_path
        || path
            .strip_prefix(base_path)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[derive(Debug)]
struct Request {
    method: String,
    target: String,
    host: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestError {
    BadRequest,
    HeaderTooLarge,
    NotFound,
    MethodNotAllowed,
    PayloadTooLarge,
    Unprocessable,
    Internal,
    Write,
}

async fn read_request(
    stream: &mut TcpStream,
    limits: ServerLimits,
) -> Result<Request, RequestError> {
    let mut bytes = Vec::with_capacity(limits.max_header_bytes.min(1024));
    let header_end;
    loop {
        if bytes.len() >= limits.max_header_bytes {
            return Err(RequestError::HeaderTooLarge);
        }
        let mut buffer = [0_u8; 1024];
        let allowed = (limits.max_header_bytes - bytes.len()).min(buffer.len());
        let count = stream
            .read(&mut buffer[..allowed])
            .await
            .map_err(|_| RequestError::BadRequest)?;
        if count == 0 {
            return Err(RequestError::BadRequest);
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(index) = find_header_end(&bytes) {
            header_end = index;
            if index + 4 != bytes.len() {
                return Err(RequestError::BadRequest);
            }
            break;
        }
    }

    let header = &bytes[..header_end];
    if !header.is_ascii() || contains_invalid_header_control(header) {
        return Err(RequestError::BadRequest);
    }
    let text = std::str::from_utf8(header).map_err(|_| RequestError::BadRequest)?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().ok_or(RequestError::BadRequest)?;
    let mut parts = request_line.split(' ');
    let method = parts.next().ok_or(RequestError::BadRequest)?;
    let target = parts.next().ok_or(RequestError::BadRequest)?;
    let version = parts.next().ok_or(RequestError::BadRequest)?;
    if method.is_empty()
        || target.is_empty()
        || parts.next().is_some()
        || version != "HTTP/1.1"
        || !is_http_token(method)
        || target.len() > limits.max_target_bytes
        || !valid_origin_target(target)
    {
        return Err(RequestError::BadRequest);
    }

    let mut host = None;
    let mut header_count = 0_usize;
    for line in lines {
        header_count = header_count
            .checked_add(1)
            .ok_or(RequestError::BadRequest)?;
        if header_count > limits.max_headers || line.is_empty() || line.starts_with([' ', '\t']) {
            return Err(RequestError::BadRequest);
        }
        let (name, raw_value) = line.split_once(':').ok_or(RequestError::BadRequest)?;
        if !is_http_token(name) || name.ends_with([' ', '\t']) {
            return Err(RequestError::BadRequest);
        }
        let value = raw_value.trim_matches([' ', '\t']);
        if value.is_empty()
            || value.bytes().any(|byte| byte.is_ascii_control())
            || matches_ignore_ascii_case(name, "transfer-encoding")
            || matches_ignore_ascii_case(name, "content-length")
            || matches_ignore_ascii_case(name, "expect")
            || matches_ignore_ascii_case(name, "upgrade")
        {
            return Err(RequestError::BadRequest);
        }
        if matches_ignore_ascii_case(name, "host") && host.replace(value.to_owned()).is_some() {
            return Err(RequestError::BadRequest);
        }
    }

    Ok(Request {
        method: method.to_owned(),
        target: target.to_owned(),
        host: host.ok_or(RequestError::NotFound)?,
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn contains_invalid_header_control(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .any(|byte| byte.is_ascii_control() && !matches!(*byte, b'\r' | b'\n'))
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn matches_ignore_ascii_case(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn valid_origin_target(target: &str) -> bool {
    target.starts_with('/')
        && !target.starts_with("//")
        && !target.contains("//")
        && !target.contains(['%', '\\', '#'])
        && target.is_ascii()
        && target
            .split('?')
            .next()
            .is_some_and(|path| !path.split('/').any(|segment| matches!(segment, "." | "..")))
        && target.matches('?').count() <= 1
}

async fn route_request(
    target: &str,
    state: &ServerState,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Response, RequestError> {
    let (path, query) = target
        .split_once('?')
        .map_or((target, None), |(path, query)| (path, Some(query)));

    if path == state.base_path {
        let page = parse_page_query(query)?;
        let permit = tokio::select! {
            _ = shutdown.changed() => return Err(RequestError::Internal),
            result = Arc::clone(&state.renders).acquire_owned() => {
                result.map_err(|_| RequestError::Internal)?
            }
        };
        let viewer = Arc::clone(&state.viewer);
        let base_path = state.base_path.clone();
        let max_html_bytes = state.limits.max_html_bytes;
        let render = tokio::task::spawn_blocking(move || {
            let result = viewer.render_page_with_max_bytes(page, &base_path, max_html_bytes);
            (result, permit)
        });
        let (html, permit) = tokio::select! {
            _ = shutdown.changed() => return Err(RequestError::Internal),
            result = render => result.map_err(|_| RequestError::Internal)?,
        };
        let html = html.map_err(map_render_error)?;
        return Ok(Response::html(html.into_bytes(), permit));
    }
    if query.is_some() {
        return Err(RequestError::NotFound);
    }
    if path == format!("{}/style.css", state.base_path) {
        return Ok(Response::css(STYLESHEET.as_bytes().to_vec()));
    }

    let image_prefix = format!("{}/image/", state.base_path);
    if let Some(digest) = path.strip_prefix(&image_prefix) {
        return serve_asset(digest, true, state, shutdown).await;
    }
    let download_prefix = format!("{}/download/", state.base_path);
    if let Some(digest) = path.strip_prefix(&download_prefix) {
        return serve_asset(digest, false, state, shutdown).await;
    }
    Err(RequestError::NotFound)
}

fn parse_page_query(query: Option<&str>) -> Result<PageQuery, RequestError> {
    let Some(query) = query else {
        return Ok(PageQuery::default());
    };
    if query.is_empty() {
        return Err(RequestError::BadRequest);
    }
    let mut kind = None;
    let mut page = None;
    for field in query.split('&') {
        let (name, value) = field.split_once('=').ok_or(RequestError::BadRequest)?;
        match name {
            "kind" if kind.is_none() => {
                kind = Some(match value {
                    "all" => PageKind::All,
                    "observations" => PageKind::Observations,
                    "actions" => PageKind::Actions,
                    "errors" => PageKind::Errors,
                    "verdicts" => PageKind::Verdicts,
                    _ => return Err(RequestError::BadRequest),
                });
            }
            "page" if page.is_none() => {
                if value.is_empty()
                    || (value.len() > 1 && value.starts_with('0'))
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(RequestError::BadRequest);
                }
                let parsed = value
                    .parse::<usize>()
                    .map_err(|_| RequestError::BadRequest)?;
                if parsed == 0 {
                    return Err(RequestError::BadRequest);
                }
                page = Some(parsed);
            }
            _ => return Err(RequestError::BadRequest),
        }
    }
    Ok(PageQuery::new(kind.unwrap_or_default(), page.unwrap_or(1)))
}

fn map_render_error(error: VisualizerError) -> RequestError {
    match error {
        VisualizerError::InvalidPage | VisualizerError::PageOutOfRange { .. } => {
            RequestError::NotFound
        }
        VisualizerError::RenderLimitExceeded => RequestError::PayloadTooLarge,
        _ => RequestError::Internal,
    }
}

async fn serve_asset(
    digest: &str,
    inline: bool,
    state: &ServerState,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Response, RequestError> {
    if !is_digest(digest) {
        return Err(RequestError::NotFound);
    }
    let asset = state
        .viewer
        .asset_by_digest(digest)
        .ok_or(RequestError::NotFound)?;
    if inline && asset.media_type != "image/png" {
        return Err(RequestError::NotFound);
    }

    let max_bytes = if inline {
        state.limits.max_inline_asset_bytes
    } else {
        state.limits.max_download_asset_bytes
    };
    let permit = tokio::select! {
        _ = shutdown.changed() => return Err(RequestError::Internal),
        result = Arc::clone(&state.assets).acquire_owned() => {
            result.map_err(|_| RequestError::Internal)?
        }
    };
    let (controller, control) =
        ExecutionController::with_timeout(state.limits.request_timeout_ms, TimeoutScope::Request);
    let read = tokio::select! {
        _ = shutdown.changed() => {
            controller.cancel(CancellationReason::Shutdown);
            return Err(RequestError::Internal);
        }
        result = read_validated_asset(state.viewer.bundle_root(), asset, max_bytes, &control) => {
            result.map_err(map_asset_error)?
        }
    };
    if read.sha256 != asset.sha256
        || read.media_type != asset.media_type
        || read.byte_length != asset.byte_length
        || read.bytes.len() as u64 != read.byte_length
    {
        return Err(RequestError::Unprocessable);
    }

    let response = if inline {
        let png_limits = PngPreviewLimits {
            max_encoded_bytes: usize::try_from(max_bytes).unwrap_or(usize::MAX),
            ..PngPreviewLimits::default()
        };
        let validation = tokio::task::spawn_blocking(move || {
            let result = validate_preview_png(&read.bytes, png_limits);
            (result, read.bytes, permit)
        });
        let (validated, bytes, permit) = tokio::select! {
            _ = shutdown.changed() => return Err(RequestError::Internal),
            result = validation => result.map_err(|_| RequestError::Internal)?,
        };
        validated.map_err(map_png_error)?;
        Response::png(bytes, permit)
    } else {
        Response::download(read.bytes, digest, permit)
    };
    Ok(response)
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn map_asset_error(error: BundleError) -> RequestError {
    match error {
        BundleError::AssetLimitExceeded | BundleError::ManifestTooLarge => {
            RequestError::PayloadTooLarge
        }
        _ => RequestError::Unprocessable,
    }
}

fn map_png_error(error: PngError) -> RequestError {
    match error {
        PngError::EncodedLimitExceeded
        | PngError::InvalidDimensions
        | PngError::PixelLimitExceeded
        | PngError::DecodedLimitExceeded
        | PngError::MetadataLimitExceeded
        | PngError::ChunkLimitExceeded => RequestError::PayloadTooLarge,
        _ => RequestError::Unprocessable,
    }
}

struct Response {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    allow_get: bool,
    disposition: Option<String>,
    memory_permit: Option<OwnedSemaphorePermit>,
}

impl Response {
    fn text(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            content_type: "text/plain; charset=utf-8",
            body: format!("{reason}\n").into_bytes(),
            allow_get: false,
            disposition: None,
            memory_permit: None,
        }
    }

    fn html(body: Vec<u8>, permit: OwnedSemaphorePermit) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/html; charset=utf-8",
            body,
            allow_get: false,
            disposition: None,
            memory_permit: Some(permit),
        }
    }

    fn css(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/css; charset=utf-8",
            body,
            allow_get: false,
            disposition: None,
            memory_permit: None,
        }
    }

    fn png(body: Vec<u8>, permit: OwnedSemaphorePermit) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "image/png",
            body,
            allow_get: false,
            disposition: None,
            memory_permit: Some(permit),
        }
    }

    fn download(body: Vec<u8>, digest: &str, permit: OwnedSemaphorePermit) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "application/octet-stream",
            body,
            allow_get: false,
            disposition: Some(format!(
                "attachment; filename=\"evidence-{}.bin\"",
                &digest[..16]
            )),
            memory_permit: Some(permit),
        }
    }
}

fn response_for_request_error(error: RequestError) -> Response {
    match error {
        RequestError::BadRequest => Response::text(400, "Bad Request"),
        RequestError::HeaderTooLarge => Response::text(431, "Request Header Fields Too Large"),
        RequestError::NotFound => Response::text(404, "Not Found"),
        RequestError::MethodNotAllowed => {
            let mut response = Response::text(405, "Method Not Allowed");
            response.allow_get = true;
            response
        }
        RequestError::PayloadTooLarge => Response::text(413, "Content Too Large"),
        RequestError::Unprocessable => Response::text(422, "Evidence Unavailable"),
        RequestError::Internal | RequestError::Write => {
            Response::text(500, "Internal Server Error")
        }
    }
}

async fn write_response(stream: &mut TcpStream, response: Response) -> io::Result<()> {
    let _memory_permit = response.memory_permit;
    let mut header = String::with_capacity(1024);
    let _ = write!(
        header,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
        response.status,
        response.reason,
        response.content_type,
        response.body.len()
    );
    header.push_str("Connection: close\r\nCache-Control: no-store, max-age=0\r\nPragma: no-cache\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nCross-Origin-Resource-Policy: same-origin\r\nCross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Embedder-Policy: require-corp\r\nPermissions-Policy: camera=(), microphone=(), geolocation=(), usb=(), payment=()\r\n");
    let _ = write!(
        header,
        "Content-Security-Policy: {CONTENT_SECURITY_POLICY}\r\n"
    );
    if response.allow_get {
        header.push_str("Allow: GET\r\n");
    }
    if let Some(disposition) = response.disposition {
        let _ = write!(header, "Content-Disposition: {disposition}\r\n");
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    stream.flush().await
}
