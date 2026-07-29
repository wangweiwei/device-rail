//! RDP bridge-backed DeviceRail Driver.

mod bridge;
mod driver;

pub use bridge::{
    BRIDGE_PROTOCOL_SCHEMA, BRIDGE_PROTOCOL_VERSION, BridgeConfig, RdpBridge, RdpBridgeError,
    RdpDesktop, RdpFrame, RdpInput, RdpTarget, SystemRdpBridge,
};
pub use driver::RdpDriver;
