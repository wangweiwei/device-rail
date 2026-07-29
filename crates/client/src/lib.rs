//! Official asynchronous Rust client for DeviceRail.
//!
//! The control plane uses bounded NDJSON frames carrying strict JSON-RPC 2.0
//! envelopes. Public request and result types are re-exported from
//! [`devicerail_protocol`], so this crate does not maintain a second wire
//! model.

mod client;
mod error;
mod event_stream;
mod framing;
pub mod methods;
mod pagination;

pub use client::{
    CallOptions, ClientOptions, ClientState, ControlTransportKind, DeviceRailClient, RequestHandle,
    SpawnConfig, default_hello,
};
pub use error::{ClientError, EventStreamError, FramingError, WatermarkError};
pub use event_stream::{
    DeviceRailEventStream, EventStreamItem, EventStreamOptions, EventStreamTerminal,
};
pub use framing::{DEFAULT_MAX_FRAME_BYTES, NdjsonDecoder};
pub use pagination::after_sequence_from_watermark;

pub use devicerail_protocol as protocol;
