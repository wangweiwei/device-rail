//! Conformant DeviceRail Driver for pages hosted by a remote Playwright server.
//!
//! Playwright remains in one private persistent Node helper. This crate owns
//! the Rust `DeviceDriver`, Session Evidence, cancellation, action schemas,
//! and the strict bounded NDJSON bridge process contract.

mod bridge;
mod driver;

pub use bridge::{
    BridgeActionResult, BridgeConfig, BrowserKind, DiscoveredPage, PlaywrightBridge,
    SystemPlaywrightBridge,
};
pub use driver::{PlaywrightDriver, discover_playwright_drivers};
