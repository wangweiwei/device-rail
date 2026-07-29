//! Bounded Android Debug Bridge discovery, evidence-backed observation, and
//! a conformant DeviceRail Android Driver.

mod command;
mod device;
mod discovery;
mod driver;
mod error;
mod model;
mod observation;

pub use command::SystemAdbConfig;
pub(crate) use command::{
    AdbCommand, AdbCommandOutput, AdbCommandRunner, AdbInputText, AdbOperation, AdbProperty,
    AndroidKey, AndroidPackageName, ProtectedAdbInput, SystemAdbCommandRunner,
};
pub use device::{AndroidDevice, AndroidDeviceConfig, AndroidHealth};
pub use discovery::AndroidAdb;
pub use driver::AndroidDriver;
pub use error::{AndroidAdbError, AndroidAdbResult};
pub use model::{
    AdbDeviceState, AdbDiscoveryIssue, AdbDiscoveryReport, AdbSerial, DiscoveredAndroidDevice,
};
