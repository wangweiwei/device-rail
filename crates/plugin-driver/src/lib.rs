//! Process-isolated DeviceRail Driver plugins.
//!
//! Plugins are ordinary executables speaking a bounded JSON protocol over
//! stdin/stdout. This crate deliberately does not load native libraries or
//! expose Rust trait objects across the plugin boundary.

mod abi;
mod discovery;
mod driver;
#[cfg(unix)]
mod owner_only;
mod transport;

pub use abi::{
    PLUGIN_ABI_SCHEMA, PLUGIN_ABI_VERSION, PluginCapabilityDeclaration, PluginFrame, PluginHello,
    PluginManifest, PluginManifestDevice, PluginManifestProtocol, PluginOperation,
    PluginRemoteError, PluginRequest, PluginResponse, PluginResponseResult,
};
pub use discovery::{
    DiscoveryConfig, PluginDescriptor, PluginDiscoveryError, discover_plugin_descriptors,
};
pub use driver::{PluginDriver, discover_plugin_drivers};
pub use transport::PluginTransportConfig;

pub const PLUGIN_MANIFEST_SCHEMA: &str = include_str!("../protocol/plugin-manifest-v1.schema.json");
