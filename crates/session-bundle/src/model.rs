use devicerail_protocol::{ProtocolVersion, SessionExport, SessionId, SessionInfo, TestEvent};
use serde::{Deserialize, Serialize};

pub const BUNDLE_MAGIC: &str = "devicerail.session-bundle";
pub const BUNDLE_VERSION: u16 = 1;
pub const MANIFEST_FILE_NAME: &str = "manifest.json";
pub const ASSET_PATH_PREFIX: &str = "assets/sha256/";

/// Strict, local-only input captured from an ended daemon Session.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleSource {
    pub event_protocol_version: ProtocolVersion,
    pub session_export: SessionExport,
}

/// The complete, canonical Bundle v1 manifest.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleManifest {
    pub magic: String,
    pub bundle_version: u16,
    pub event_protocol_version: ProtocolVersion,
    pub session: SessionInfo,
    pub events: Vec<TestEvent>,
    pub assets: Vec<BundleAsset>,
}

impl BundleManifest {
    pub fn from_source(source: &BundleSource, assets: Vec<BundleAsset>) -> Self {
        Self {
            magic: BUNDLE_MAGIC.to_owned(),
            bundle_version: BUNDLE_VERSION,
            event_protocol_version: source.event_protocol_version,
            session: source.session_export.session.clone(),
            events: source.session_export.events.clone(),
            assets,
        }
    }

    pub fn session_export(&self) -> SessionExport {
        SessionExport {
            session: self.session.clone(),
            events: self.events.clone(),
        }
    }
}

/// One content-addressed file in the Bundle.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleAsset {
    pub sha256: String,
    pub media_type: String,
    pub byte_length: u64,
    pub path: String,
}

impl BundleAsset {
    pub fn canonical_path(digest: &str) -> String {
        format!("{ASSET_PATH_PREFIX}{digest}")
    }
}

/// Resource ceilings applied before allocating or reading Bundle content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundleLimits {
    pub max_manifest_bytes: u64,
    pub max_events: usize,
    pub max_assets: usize,
    /// Maximum nesting of arbitrary JSON values carried by event fields.
    pub max_json_depth: usize,
    /// Maximum nodes in any one arbitrary JSON value before serialization.
    pub max_json_nodes: usize,
    /// Counts every typed `AssetRef` occurrence, before digest de-duplication.
    pub max_typed_references: usize,
    pub max_asset_bytes: u64,
    pub max_total_asset_bytes: u64,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 16 * 1024 * 1024,
            max_events: 100_000,
            max_assets: 10_000,
            max_json_depth: 128,
            max_json_nodes: 1_000_000,
            max_typed_references: 100_000,
            max_asset_bytes: 512 * 1024 * 1024,
            max_total_asset_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleSummary {
    pub session_id: SessionId,
    pub event_count: u64,
    pub asset_count: u64,
    pub asset_bytes: u64,
}

impl BundleSummary {
    pub fn from_manifest(manifest: &BundleManifest) -> Self {
        Self {
            session_id: manifest.session.id.clone(),
            event_count: manifest.events.len() as u64,
            asset_count: manifest.assets.len() as u64,
            asset_bytes: manifest.assets.iter().fold(0_u64, |total, asset| {
                total.saturating_add(asset.byte_length)
            }),
        }
    }
}

/// Successful validation result, including the reconstructed replay input.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedBundle {
    pub event_protocol_version: ProtocolVersion,
    pub session_export: SessionExport,
    pub assets: Vec<BundleAsset>,
    pub summary: BundleSummary,
}

impl ValidatedBundle {
    pub fn from_manifest(manifest: &BundleManifest) -> Self {
        Self {
            event_protocol_version: manifest.event_protocol_version,
            session_export: manifest.session_export(),
            assets: manifest.assets.clone(),
            summary: BundleSummary::from_manifest(manifest),
        }
    }
}
