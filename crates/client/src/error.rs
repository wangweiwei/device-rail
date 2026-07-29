use devicerail_protocol::{EventsStreamTermination, RpcError, RpcId};
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FramingError {
    #[error("NDJSON frame exceeds the {limit}-byte limit")]
    FrameTooLarge { limit: usize },
    #[error("NDJSON frame is not valid UTF-8")]
    InvalidUtf8,
    #[error("response stream ended with an incomplete NDJSON frame")]
    IncompleteFrame,
    #[error("NDJSON decoder has already ended")]
    AlreadyEnded,
}

#[derive(Clone, Debug, Error)]
pub enum ClientError {
    #[error("DeviceRail transport failed: {0}")]
    Transport(String),
    #[error(transparent)]
    Framing(#[from] FramingError),
    #[error("DeviceRail protocol violation: {0}")]
    ProtocolViolation(String),
    #[error("failed to serialize or deserialize DeviceRail data: {0}")]
    Serialization(String),
    #[error("DeviceRail RPC {request_id:?} failed")]
    RemoteRpc {
        request_id: RpcId,
        error: Box<RpcError>,
    },
    #[error("{method} requires negotiated feature {feature}")]
    FeatureNotNegotiated {
        method: &'static str,
        feature: &'static str,
    },
    #[error("DeviceRail client operation is invalid while the client is {0}")]
    HandshakeState(&'static str),
    #[error("DeviceRail client requires an active Tokio runtime")]
    RuntimeUnavailable,
    #[error("DeviceRail client reached its pending request limit of {0}")]
    PendingRequestLimit(usize),
    #[error("DeviceRail client exceeded its late-response budget of {0} abandoned requests")]
    AbandonedRequestLimit(usize),
    #[error("DeviceRail write queue is full (maximum {max_frames} frames / {max_bytes} bytes)")]
    WriteQueueFull { max_frames: usize, max_bytes: usize },
    #[error("DeviceRail frame is {actual} bytes; the limit is {limit}")]
    WriteFrameTooLarge { actual: usize, limit: usize },
    #[error("DeviceRail client is closed")]
    Closed,
    #[error("DeviceRail client task failed: {0}")]
    Internal(String),
}

impl ClientError {
    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::ProtocolViolation(message.into())
    }

    pub(crate) fn transport(message: impl Into<String>) -> Self {
        Self::Transport(message.into())
    }

    pub(crate) fn serialization(error: impl std::fmt::Display) -> Self {
        Self::Serialization(error.to_string())
    }
}

#[derive(Clone, Debug, Error)]
pub enum EventStreamError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("event stream endpoint is invalid: {0}")]
    InvalidEndpoint(String),
    #[error("event stream WebSocket failed: {0}")]
    Transport(String),
    #[error("event stream {phase} timed out")]
    Timeout { phase: &'static str },
    #[error("event stream protocol violation: {0}")]
    ProtocolViolation(String),
    #[error("event stream cursor is invalid: {0}")]
    Cursor(String),
    #[error("event stream local queue is full (maximum {max_events} events / {max_bytes} bytes)")]
    QueueOverflow { max_events: usize, max_bytes: usize },
    #[error("event stream terminated remotely")]
    RemoteTermination(EventsStreamTermination),
    #[error("event stream was cancelled")]
    Cancelled,
    #[error("event stream is already active")]
    Active,
    #[error("event stream must be drained before it can resume")]
    NotDrained,
}

/// Error returned when an application watermark cannot be represented on the
/// DeviceRail wire protocol.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WatermarkError {
    /// The watermark exceeds JavaScript's maximum safe integer.
    #[error("event watermark {watermark} exceeds the cross-language safe integer limit {max}")]
    ExceedsSafeInteger { watermark: u64, max: u64 },
}
