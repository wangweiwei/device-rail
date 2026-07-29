//! Authenticated, bounded routing for DeviceRail nodes.
//!
//! This crate is a Driver-layer adapter. It has no listener, TLS stack, AI
//! SDK, prompt runtime, recorder, visualizer, or application dependency. A
//! caller supplies an already authenticated stream or an independently
//! authenticated SSH/mTLS tunnel terminating on loopback.

mod config;
mod connector;
mod driver;
mod lease;
mod model;
mod router;
mod security;
mod server;
mod service;
mod telemetry;
mod transport;

pub use config::{
    ConfigError, ConfiguredPeer, ConfiguredPeerServer, ConfiguredPeers,
    EXTERNAL_TUNNEL_SECURITY_MODE,
};
pub use connector::{ConnectorError, connect_configured_peers};
pub use driver::{RemoteDeviceDriver, RemoteDriverConfig};
pub use lease::{CallLedger, CallLedgerDecision, LeaseError, LeaseTable};
pub use model::{
    DISTRIBUTED_PROTOCOL_VERSION, HealthState, InventorySnapshot, ModelError, NodeId, PeerError,
    PeerLease, PeerOperation, PeerProtocolCapabilities, PeerRequest, PeerResponse, PeerResult,
    RemoteDeviceDescriptor,
};
pub use router::{NodeRouter, RemoteNode, RouteError, RouterConfig};
pub use security::{PeerSecurity, SecurityError, SecurityKind};
pub use server::{PeerServerError, serve_peer_stream, serve_peer_stream_until_cancelled};
pub use service::{PeerServiceError, RegistryPeerService};
pub use telemetry::{
    MemoryTelemetry, OperationMethod, OperationOutcome, TelemetryRecord, TelemetrySink,
};
pub use transport::{
    MAX_PEER_FRAME_BYTES, MAX_PEER_TRANSPORT_SHARDS, NdjsonPeerTransport, PeerTransport,
    ShardedPeerTransport, TransportError,
};

pub const PEER_PROTOCOL_SCHEMA: &str = include_str!("../protocol/peer-v2.schema.json");
