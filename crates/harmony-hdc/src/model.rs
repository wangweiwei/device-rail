use std::{collections::BTreeMap, fmt};

use devicerail_protocol::{DeviceId, DeviceInfo, Platform};

use crate::{HarmonyHdcError, HarmonyHdcResult};

const MAX_TARGET_BYTES: usize = 512;

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HdcTarget(String);

impl HdcTarget {
    pub fn parse(value: impl Into<String>) -> HarmonyHdcResult<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_TARGET_BYTES
            && value.is_ascii()
            && !value.starts_with('-')
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'_' | b'#')
            });
        if !valid {
            return Err(HarmonyHdcError::InvalidTarget(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn device_id(&self) -> DeviceId {
        DeviceId::new(format!("harmony-hdc:{}", self.0))
    }
}

impl fmt::Debug for HdcTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("HdcTarget").field(&self.0).finish()
    }
}

impl fmt::Display for HdcTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HdcTargetState {
    Ready,
    Offline,
    Unauthorized,
    Unknown(String),
}

impl HdcTargetState {
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "connected" | "device" | "online" | "ready" => Self::Ready,
            "offline" | "disconnected" => Self::Offline,
            "unauthorized" | "authorizing" => Self::Unauthorized,
            other => Self::Unknown(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Ready => "ready",
            Self::Offline => "offline",
            Self::Unauthorized => "unauthorized",
            Self::Unknown(value) => value,
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredHarmonyDevice {
    pub target: HdcTarget,
    pub state: HdcTargetState,
    pub name: Option<String>,
    pub os_version: Option<String>,
    pub extensions: BTreeMap<String, String>,
}

impl DiscoveredHarmonyDevice {
    pub fn device_info(&self, connected: bool) -> DeviceInfo {
        DeviceInfo {
            id: self.target.device_id(),
            name: self
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("HarmonyOS ({})", self.target)),
            platform: Platform::HarmonyOs,
            os_version: self.os_version.clone(),
            connected,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarmonyDiscoveryReport {
    pub devices: Vec<DiscoveredHarmonyDevice>,
    pub ignored_diagnostics: Vec<String>,
}
