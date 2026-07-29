//! Bounded HarmonyOS HDC discovery, hierarchy-backed observation, and a
//! conformant DeviceRail Driver.

mod command;
mod discovery;
mod driver;
mod error;
mod model;

pub use command::{
    HarmonyAbilityName, HarmonyBundleName, HarmonyKey, HdcCommand, HdcCommandOutput,
    HdcCommandRunner, HdcInputText, HdcOperation, HdcProperty, SystemHdcCommandRunner,
    SystemHdcConfig,
};
pub use discovery::HarmonyHdc;
pub use driver::HarmonyHdcDriver;
pub use error::{HarmonyHdcError, HarmonyHdcResult};
pub use model::{DiscoveredHarmonyDevice, HarmonyDiscoveryReport, HdcTarget, HdcTargetState};
