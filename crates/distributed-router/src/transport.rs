use std::{collections::BTreeMap, future::pending, sync::Arc, time::Duration};

use async_trait::async_trait;
use devicerail_core::ExecutionControl;
use thiserror::Error;
use tokio::{
    io::{
        AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _,
        BufStream,
    },
    sync::Mutex,
};

use crate::{
    NodeId, PeerOperation, PeerRequest, PeerResponse, PeerSecurity,
    model::{
        MAX_DEVICE_KEY_BYTES, MAX_INVENTORY_DEVICES, MAX_REQUEST_TIMEOUT_MS, ModelError,
        valid_identifier, wire_protocol_version, wire_schema_accepts,
    },
};

pub const MAX_PEER_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_PEER_TRANSPORT_SHARDS: usize = 4;
const CANCEL_WRITE_GRACE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum TransportError {
    #[error("peer request is invalid")]
    InvalidRequest,
    #[error("peer frame exceeds the bounded size limit")]
    FrameTooLarge,
    #[error("peer transport is closed")]
    ClosedBeforeSend,
    #[error("peer transport failed after a request may have been sent")]
    FailedAfterSend,
    #[error("peer response violates the distributed protocol")]
    Protocol,
    #[error("peer protocol version is unsupported")]
    UnsupportedVersion,
    #[error("peer request was cancelled")]
    Cancelled,
    #[error("peer request was cancelled after bytes may have been sent")]
    CancelledAfterSend,
    #[error("peer request deadline elapsed")]
    TimedOut,
    #[error("peer request deadline elapsed after bytes may have been sent")]
    TimedOutAfterSend,
}

impl TransportError {
    pub fn may_have_reached_peer(self) -> bool {
        matches!(
            self,
            Self::FailedAfterSend | Self::CancelledAfterSend | Self::TimedOutAfterSend
        )
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "peer_request_invalid",
            Self::FrameTooLarge => "peer_frame_too_large",
            Self::ClosedBeforeSend => "peer_closed_before_send",
            Self::FailedAfterSend => "peer_failed_after_send",
            Self::Protocol => "peer_protocol_error",
            Self::UnsupportedVersion => "peer_protocol_version_unsupported",
            Self::Cancelled => "peer_cancelled",
            Self::CancelledAfterSend => "peer_cancelled_after_send",
            Self::TimedOut => "peer_timed_out",
            Self::TimedOutAfterSend => "peer_timed_out_after_send",
        }
    }
}

#[async_trait]
pub trait PeerTransport: Send + Sync {
    fn expected_node_id(&self) -> &NodeId;
    fn security(&self) -> &PeerSecurity;

    /// Exchanges exactly one bounded request on a long-lived authenticated
    /// channel. Implementations must preserve request order. Cancellation must
    /// either deliver `cancel` or close the channel so remote work cannot be
    /// silently detached.
    async fn request(
        &self,
        request: PeerRequest,
        control: &ExecutionControl,
    ) -> Result<PeerResponse, TransportError>;
}

/// Routes each remote device to one of a bounded set of independently
/// authenticated transports. A device remains pinned to one shard, so its
/// connection-bound lease and cancellation/outcome semantics are unchanged,
/// while unrelated devices no longer share one transport mutex.
pub struct ShardedPeerTransport {
    expected_node_id: NodeId,
    security: PeerSecurity,
    routes: BTreeMap<String, usize>,
    shards: Vec<Arc<dyn PeerTransport>>,
}

impl ShardedPeerTransport {
    pub fn new(
        shards: Vec<Arc<dyn PeerTransport>>,
        device_keys: impl IntoIterator<Item = String>,
    ) -> Result<Arc<Self>, TransportError> {
        if shards.is_empty() || shards.len() > MAX_PEER_TRANSPORT_SHARDS {
            return Err(TransportError::InvalidRequest);
        }
        let expected_node_id = shards[0].expected_node_id().clone();
        let security = shards[0].security().clone();
        if shards.iter().any(|shard| {
            shard.expected_node_id() != &expected_node_id || shard.security() != &security
        }) {
            return Err(TransportError::InvalidRequest);
        }
        let mut routes = BTreeMap::new();
        for (index, device_key) in device_keys.into_iter().enumerate() {
            if index >= MAX_INVENTORY_DEVICES
                || !valid_identifier(&device_key, MAX_DEVICE_KEY_BYTES)
                || routes.insert(device_key, index % shards.len()).is_some()
            {
                return Err(TransportError::InvalidRequest);
            }
        }
        if routes.is_empty() {
            return Err(TransportError::InvalidRequest);
        }
        Ok(Arc::new(Self {
            expected_node_id,
            security,
            routes,
            shards,
        }))
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    fn shard(&self, request: &PeerRequest) -> &Arc<dyn PeerTransport> {
        let index = request
            .operation
            .device_key()
            .and_then(|device_key| self.routes.get(device_key))
            .copied()
            .unwrap_or(0);
        &self.shards[index]
    }
}

impl std::fmt::Debug for ShardedPeerTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShardedPeerTransport")
            .field("expected_node_id", &self.expected_node_id)
            .field("security", &self.security)
            .field("device_count", &self.routes.len())
            .field("shard_count", &self.shards.len())
            .finish()
    }
}

#[async_trait]
impl PeerTransport for ShardedPeerTransport {
    fn expected_node_id(&self) -> &NodeId {
        &self.expected_node_id
    }

    fn security(&self) -> &PeerSecurity {
        &self.security
    }

    async fn request(
        &self,
        request: PeerRequest,
        control: &ExecutionControl,
    ) -> Result<PeerResponse, TransportError> {
        self.shard(&request).request(request, control).await
    }
}

/// Sequential, long-lived NDJSON peer transport over an authenticated stream.
///
/// On cancellation or timeout it writes a best-effort cancel frame and then
/// poisons/closes the stream. Closing is intentional: a late response cannot
/// be mistaken for the next request. Callers must reconnect and rediscover the
/// node before issuing more work.
pub struct NdjsonPeerTransport<S> {
    expected_node_id: NodeId,
    security: PeerSecurity,
    stream: Mutex<Option<BufStream<S>>>,
}

impl<S> std::fmt::Debug for NdjsonPeerTransport<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NdjsonPeerTransport")
            .field("expected_node_id", &self.expected_node_id)
            .field("security", &self.security)
            .finish_non_exhaustive()
    }
}

impl<S> NdjsonPeerTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(stream: S, expected_node_id: NodeId, security: PeerSecurity) -> Arc<Self> {
        Arc::new(Self {
            expected_node_id,
            security,
            stream: Mutex::new(Some(BufStream::new(stream))),
        })
    }

    pub async fn is_open(&self) -> bool {
        self.stream.lock().await.is_some()
    }
}

enum WaitResult {
    Response(Result<Vec<u8>, TransportError>),
    Cancelled,
    TimedOut,
}

#[async_trait]
impl<S> PeerTransport for NdjsonPeerTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    fn expected_node_id(&self) -> &NodeId {
        &self.expected_node_id
    }

    fn security(&self) -> &PeerSecurity {
        &self.security
    }

    async fn request(
        &self,
        mut request: PeerRequest,
        control: &ExecutionControl,
    ) -> Result<PeerResponse, TransportError> {
        if request.node_id != self.expected_node_id {
            return Err(TransportError::InvalidRequest);
        }
        if control.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if control.is_expired() {
            return Err(TransportError::TimedOut);
        }
        if request.timeout_ms.is_none()
            && let Some(remaining) = control.remaining()
        {
            request.timeout_ms = Some(
                (remaining
                    .as_millis()
                    .min(u128::from(MAX_REQUEST_TIMEOUT_MS)) as u64)
                    .max(1),
            );
        }
        request.validate().map_err(|error| match error {
            ModelError::UnsupportedVersion => TransportError::UnsupportedVersion,
            _ => TransportError::InvalidRequest,
        })?;
        let frame = encode_frame(&request)?;
        let mut connection = tokio::select! {
            connection = self.stream.lock() => connection,
            _ = control.cancelled() => return Err(TransportError::Cancelled),
            _ = deadline(control.remaining()) => return Err(TransportError::TimedOut),
        };
        if control.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if control.is_expired() {
            return Err(TransportError::TimedOut);
        }
        if connection.is_none() {
            return Err(TransportError::ClosedBeforeSend);
        }
        let write = {
            let stream = connection.as_mut().expect("stream remains present");
            tokio::select! {
                result = async {
                    stream.write_all(&frame).await?;
                    stream.flush().await
                } => result.map_err(|_| TransportError::FailedAfterSend),
                _ = control.cancelled() => Err(TransportError::CancelledAfterSend),
                _ = deadline(control.remaining()) => Err(TransportError::TimedOutAfterSend),
            }
        };
        if let Err(error) = write {
            *connection = None;
            return Err(error);
        }

        let wait = {
            let stream = connection.as_mut().expect("stream remains present");
            tokio::select! {
                response = read_frame(stream) => WaitResult::Response(response),
                _ = control.cancelled() => WaitResult::Cancelled,
                _ = deadline(control.remaining()) => WaitResult::TimedOut,
            }
        };
        let bytes = match wait {
            WaitResult::Response(Ok(bytes)) => bytes,
            WaitResult::Response(Err(error)) => {
                *connection = None;
                return Err(error);
            }
            WaitResult::Cancelled => {
                if let Some(stream) = connection.as_mut() {
                    let _ =
                        tokio::time::timeout(CANCEL_WRITE_GRACE, write_cancel(stream, &request))
                            .await;
                }
                *connection = None;
                return Err(TransportError::CancelledAfterSend);
            }
            WaitResult::TimedOut => {
                if let Some(stream) = connection.as_mut() {
                    let _ =
                        tokio::time::timeout(CANCEL_WRITE_GRACE, write_cancel(stream, &request))
                            .await;
                }
                *connection = None;
                return Err(TransportError::TimedOutAfterSend);
            }
        };
        if !wire_schema_accepts(&bytes) {
            *connection = None;
            if wire_protocol_version(&bytes)
                .is_some_and(|version| version != crate::DISTRIBUTED_PROTOCOL_VERSION)
            {
                return Err(TransportError::UnsupportedVersion);
            }
            return Err(TransportError::Protocol);
        }
        let response: PeerResponse = match serde_json::from_slice(&bytes) {
            Ok(response) => response,
            Err(_) => {
                *connection = None;
                return Err(TransportError::Protocol);
            }
        };
        if response.validate_for(&request).is_err() {
            *connection = None;
            return Err(TransportError::Protocol);
        }
        Ok(response)
    }
}

fn encode_frame(request: &PeerRequest) -> Result<Vec<u8>, TransportError> {
    let mut bytes = serde_json::to_vec(request).map_err(|_| TransportError::InvalidRequest)?;
    if bytes.is_empty() || bytes.len() >= MAX_PEER_FRAME_BYTES {
        return Err(TransportError::FrameTooLarge);
    }
    bytes.push(b'\n');
    Ok(bytes)
}

async fn read_frame<S>(stream: &mut BufStream<S>) -> Result<Vec<u8>, TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut bytes = Vec::new();
    let mut bounded = stream.take((MAX_PEER_FRAME_BYTES + 1) as u64);
    let read = bounded
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|_| TransportError::FailedAfterSend)?;
    if read == 0 {
        return Err(TransportError::FailedAfterSend);
    }
    if bytes.len() > MAX_PEER_FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err(TransportError::FrameTooLarge);
    }
    bytes.pop();
    if bytes.is_empty() || bytes.last() == Some(&b'\r') {
        return Err(TransportError::Protocol);
    }
    Ok(bytes)
}

async fn deadline(remaining: Option<Duration>) {
    match remaining {
        Some(remaining) => tokio::time::sleep(remaining).await,
        None => pending::<()>().await,
    }
}

async fn write_cancel<S>(
    stream: &mut BufStream<S>,
    request: &PeerRequest,
) -> Result<(), TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(epoch) = request.node_epoch else {
        return Ok(());
    };
    let mut cancel = PeerRequest::new(
        request.node_id.clone(),
        Some(epoch),
        PeerOperation::Cancel {
            target_request_id: request.request_id,
            call_id: request.call_id,
        },
    );
    cancel.trace_id = request.trace_id;
    let frame = encode_frame(&cancel)?;
    stream
        .write_all(&frame)
        .await
        .map_err(|_| TransportError::FailedAfterSend)?;
    stream
        .flush()
        .await
        .map_err(|_| TransportError::FailedAfterSend)
}

impl From<ModelError> for TransportError {
    fn from(error: ModelError) -> Self {
        match error {
            ModelError::UnsupportedVersion => Self::UnsupportedVersion,
            _ => Self::Protocol,
        }
    }
}
