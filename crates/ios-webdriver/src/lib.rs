//! Bounded Direct WebDriverAgent, MJPEG, and Appium XCUITest adapters for iOS.
//!
//! This crate does not launch WebDriverAgent or Appium and does not discover
//! USB devices. A caller supplies a stable device descriptor and exactly one
//! backend endpoint. All network I/O lives behind injectable traits so tests
//! never require an iOS device and product code can choose its own process,
//! tunnel, and lifecycle boundary. The stock Driver uses only its fixed typed
//! operation set; bounded Appium capability/script extensions remain explicit
//! trusted-embedder APIs rather than DeviceRail wire operations.

mod appium;
mod appium_driver;
mod config;
mod control;
mod driver;
mod http;
mod mjpeg;
mod semantic;
mod transport;

pub use appium::{
    AppiumButton, AppiumContext, AppiumDrag, AppiumElement, AppiumLocatorStrategy, AppiumSession,
    AppiumSessionRequest, AppiumStatus, AppiumTransport, SystemAppiumTransport,
};
pub use appium_driver::AppiumIosDriver;
pub use config::{HttpEndpointConfig, IosDeviceConfig};
pub use driver::IosDriver;
pub use http::SystemWdaTransport;
pub use mjpeg::{MjpegFrame, MjpegFrameSource, SystemMjpegFrameSource};
pub use transport::{IosKey, WdaAction, WdaPage, WdaSession, WdaStatus, WdaTransport};
