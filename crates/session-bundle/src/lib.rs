//! Canonical, platform-neutral DeviceRail Session Bundle support.

mod bundle_io;
mod canonical;
mod event_validation;
mod filesystem;
mod model;

pub use bundle_io::{
    BundleError, BundleEvidence, BundleEvidenceSource, ValidatedAssetBytes, export_directory,
    read_validated_asset, validate_directory,
};
pub use canonical::{
    CanonicalJsonError, from_canonical_slice, to_canonical_bytes, verify_canonical_slice,
};
pub use event_validation::{ModelError, validate_manifest_events, validate_source};
pub use filesystem::{
    FilesystemError, StagingDir, metadata_is_link_like, open_regular_file_nofollow, sync_directory,
};
pub use model::{
    ASSET_PATH_PREFIX, BUNDLE_MAGIC, BUNDLE_VERSION, BundleAsset, BundleLimits, BundleManifest,
    BundleSource, BundleSummary, MANIFEST_FILE_NAME, ValidatedBundle,
};
