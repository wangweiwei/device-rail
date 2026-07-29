use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{UiSnapshotOmissionReason, UiSnapshotRef};

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct DeviceId(pub String);

impl DeviceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Platform {
    Web,
    Android,
    Ios,
    HarmonyOs,
    MacOs,
    Windows,
    Linux,
    Rdp,
    Mock,
    Other(String),
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub platform: Platform,
    pub os_version: Option<String>,
    pub connected: bool,
}

/// Result returned by `devices.list`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DevicesListResult {
    pub devices: Vec<DeviceInfo>,
    pub selected_device_id: Option<DeviceId>,
}

/// Parameters accepted by `device.select`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceSelectParams {
    pub device_id: DeviceId,
}

/// Result returned by `device.select`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceSelectResult {
    pub device: DeviceInfo,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    #[cfg_attr(feature = "schema", schemars(range(max = 4_294_967_295_u64)))]
    pub width: u32,
    #[cfg_attr(feature = "schema", schemars(range(max = 4_294_967_295_u64)))]
    pub height: u32,
    pub scale_factor: f64,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRef {
    pub id: String,
    pub media_type: String,
    pub uri: String,
    pub sha256: Option<String>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ScreenshotOmissionReason {
    Policy,
    ProtectedAction,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub id: Uuid,
    pub device_id: DeviceId,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(feature = "schema", schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub captured_at_ms: u64,
    pub viewport: Viewport,
    pub screenshot: Option<AssetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_omission: Option<ScreenshotOmissionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_snapshot: Option<UiSnapshotRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_snapshot_omission: Option<UiSnapshotOmissionReason>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl Observation {
    /// Returns every typed Evidence reference owned by this Observation.
    pub fn asset_refs(&self) -> impl Iterator<Item = &AssetRef> {
        self.screenshot
            .iter()
            .chain(self.ui_snapshot.iter().map(|snapshot| &snapshot.evidence))
    }

    /// A snapshot and its omission reason are mutually exclusive. Both absent
    /// preserves the pre-1.5 Observation shape and represents no claim.
    pub const fn ui_snapshot_state_is_valid(&self) -> bool {
        !(self.ui_snapshot.is_some() && self.ui_snapshot_omission.is_some())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};
    use uuid::Uuid;

    use super::{
        DeviceId, DeviceSelectParams, DeviceSelectResult, DevicesListResult, Observation,
        ScreenshotOmissionReason, Viewport,
    };

    #[test]
    fn device_ids_have_stable_lexical_order() {
        let mut ids = [
            DeviceId::new("web-z"),
            DeviceId::new("android-2"),
            DeviceId::new("android-10"),
        ];
        ids.sort();

        assert_eq!(ids.map(|id| id.0), ["android-10", "android-2", "web-z"]);
    }

    #[test]
    fn routing_models_are_strict_and_use_camel_case() {
        let params: DeviceSelectParams = serde_json::from_value(json!({
            "deviceId": "android-emulator-5554"
        }))
        .expect("select params");
        assert_eq!(params.device_id, DeviceId::new("android-emulator-5554"));
        assert!(
            serde_json::from_value::<DeviceSelectParams>(json!({
                "deviceId": "android-emulator-5554",
                "unknown": true
            }))
            .is_err()
        );

        let list: DevicesListResult = serde_json::from_value(json!({
            "devices": [],
            "selectedDeviceId": null
        }))
        .expect("list result");
        assert!(list.devices.is_empty());
        assert!(list.selected_device_id.is_none());
        assert_eq!(
            serde_json::to_value(list).expect("serialize list result"),
            json!({ "devices": [], "selectedDeviceId": null })
        );

        assert!(
            serde_json::from_value::<DeviceSelectResult>(json!({
                "device": {
                    "id": "android-emulator-5554",
                    "name": "Pixel",
                    "platform": { "kind": "android" },
                    "osVersion": "15",
                    "connected": true
                },
                "selectedDeviceId": "android-emulator-5554"
            }))
            .is_err()
        );
    }

    #[test]
    fn screenshot_omission_is_optional_and_typed() {
        let base = Observation {
            id: Uuid::nil(),
            device_id: DeviceId::new("mock-1"),
            captured_at_ms: 1,
            viewport: Viewport {
                width: 1,
                height: 1,
                scale_factor: 1.0,
            },
            screenshot: None,
            screenshot_omission: None,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata: Map::new(),
        };
        let legacy = serde_json::to_value(&base).expect("legacy observation");
        assert!(legacy.get("screenshotOmission").is_none());
        assert!(legacy.get("uiSnapshot").is_none());
        assert!(legacy.get("uiSnapshotOmission").is_none());

        let omitted = Observation {
            screenshot_omission: Some(ScreenshotOmissionReason::ProtectedAction),
            ..base
        };
        assert_eq!(
            serde_json::to_value(omitted).expect("omitted observation")["screenshotOmission"],
            "protectedAction"
        );
    }
}
