use std::{collections::BTreeMap, fmt};

use devicerail_protocol::{DeviceId, DeviceInfo, Platform};

use crate::{AndroidAdbError, AndroidAdbResult};

const MAX_SERIAL_BYTES: usize = 512;

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AdbSerial(String);

impl AdbSerial {
    pub fn parse(value: impl Into<String>) -> AndroidAdbResult<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_SERIAL_BYTES
            || value.chars().any(char::is_whitespace)
            || value.chars().any(char::is_control)
        {
            return Err(AndroidAdbError::InvalidSerial(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn device_id(&self) -> DeviceId {
        DeviceId::new(format!("android-adb:{}", self.0))
    }
}

impl fmt::Debug for AdbSerial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("AdbSerial").field(&self.0).finish()
    }
}

impl fmt::Display for AdbSerial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdbDeviceState {
    Ready,
    Offline,
    Unauthorized,
    Authorizing,
    Recovery,
    Sideload,
    Bootloader,
    NoPermissions,
    Unknown(String),
}

impl AdbDeviceState {
    pub fn parse(value: &str, remainder: &str) -> Self {
        match value {
            "device" => Self::Ready,
            "offline" => Self::Offline,
            "unauthorized" => Self::Unauthorized,
            "authorizing" => Self::Authorizing,
            "recovery" => Self::Recovery,
            "sideload" => Self::Sideload,
            "bootloader" => Self::Bootloader,
            "no" if remainder.starts_with("permissions") => Self::NoPermissions,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredAndroidDevice {
    pub serial: AdbSerial,
    pub state: AdbDeviceState,
    pub product: Option<String>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub transport_id: Option<u64>,
    pub extensions: BTreeMap<String, String>,
}

impl DiscoveredAndroidDevice {
    pub fn device_info(&self) -> DeviceInfo {
        let name = self
            .model
            .as_deref()
            .or(self.product.as_deref())
            .or(self.device.as_deref())
            .map(|value| value.replace('_', " "))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| format!("Android ({})", self.serial));
        DeviceInfo {
            id: self.serial.device_id(),
            name,
            platform: Platform::Android,
            os_version: None,
            connected: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbDiscoveryIssue {
    pub line: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdbDiscoveryReport {
    pub devices: Vec<DiscoveredAndroidDevice>,
    pub issues: Vec<AdbDiscoveryIssue>,
}
