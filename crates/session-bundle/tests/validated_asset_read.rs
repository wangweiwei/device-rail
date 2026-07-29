use std::{fs, path::Path};

use devicerail_core::{CancellationReason, ExecutionControl, ExecutionController};
use devicerail_session_bundle::{BundleAsset, BundleError, ModelError, read_validated_asset};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const MEDIA_TYPE: &str = "image/png";
const ASSET_BYTES: &[u8] = b"DeviceRail safe asset read fixture";

fn fixture() -> (TempDir, BundleAsset) {
    let temporary = TempDir::new().expect("temporary Bundle root");
    let digest = hex::encode(Sha256::digest(ASSET_BYTES));
    let asset = BundleAsset {
        sha256: digest.clone(),
        media_type: MEDIA_TYPE.to_owned(),
        byte_length: ASSET_BYTES.len() as u64,
        path: BundleAsset::canonical_path(&digest),
    };
    let directory = temporary.path().join("assets").join("sha256");
    fs::create_dir_all(&directory).expect("asset directory");
    fs::write(directory.join(&digest), ASSET_BYTES).expect("asset bytes");
    (temporary, asset)
}

fn asset_path(root: &Path, asset: &BundleAsset) -> std::path::PathBuf {
    root.join("assets").join("sha256").join(&asset.sha256)
}

#[tokio::test]
async fn returns_owned_verified_bytes_without_a_path() {
    let (temporary, asset) = fixture();
    let read = read_validated_asset(
        temporary.path(),
        &asset,
        ASSET_BYTES.len() as u64,
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("read validated asset");

    assert_eq!(read.sha256, asset.sha256);
    assert_eq!(read.media_type, MEDIA_TYPE);
    assert_eq!(read.byte_length, ASSET_BYTES.len() as u64);
    assert_eq!(read.bytes, ASSET_BYTES);
    let debug = format!("{read:?}");
    assert!(!debug.contains("DeviceRail safe asset read fixture"));
    assert!(debug.contains("byte_length"));
}

#[tokio::test]
async fn rejects_tampering_and_truncation_after_snapshot_validation() {
    let (temporary, asset) = fixture();
    let path = asset_path(temporary.path(), &asset);
    let mut tampered = ASSET_BYTES.to_vec();
    tampered[0] ^= 0xff;
    fs::write(&path, tampered).expect("tamper same-size asset");
    assert!(matches!(
        read_validated_asset(
            temporary.path(),
            &asset,
            ASSET_BYTES.len() as u64,
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(BundleError::EvidenceDigestMismatch)
    ));

    fs::write(&path, &ASSET_BYTES[..ASSET_BYTES.len() - 1]).expect("truncate asset");
    assert!(matches!(
        read_validated_asset(
            temporary.path(),
            &asset,
            ASSET_BYTES.len() as u64,
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(BundleError::EvidenceSizeMismatch)
    ));
}

#[tokio::test]
async fn rejects_a_lower_read_budget_before_returning_bytes() {
    let (temporary, asset) = fixture();
    assert!(matches!(
        read_validated_asset(
            temporary.path(),
            &asset,
            asset.byte_length - 1,
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(BundleError::AssetLimitExceeded)
    ));
}

#[tokio::test]
async fn rejects_untrusted_snapshot_fields_and_never_uses_the_supplied_path() {
    let (temporary, asset) = fixture();

    let mut bad_path = asset.clone();
    bad_path.path = "../../outside".to_owned();
    assert!(matches!(
        read_validated_asset(
            temporary.path(),
            &bad_path,
            asset.byte_length,
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(BundleError::Model(ModelError::InvalidAssetPath))
    ));

    let mut bad_media_type = asset.clone();
    bad_media_type.media_type = "not a media type".to_owned();
    assert!(matches!(
        read_validated_asset(
            temporary.path(),
            &bad_media_type,
            asset.byte_length,
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(BundleError::Model(ModelError::InvalidAssetIndexEntry))
    ));

    let mut bad_digest = asset;
    bad_digest.sha256 = "A".repeat(64);
    assert!(matches!(
        read_validated_asset(
            temporary.path(),
            &bad_digest,
            bad_digest.byte_length,
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(BundleError::Model(ModelError::InvalidAssetIndexEntry))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn refuses_a_symlink_replacement_in_the_final_component() {
    use std::os::unix::fs::symlink;

    let (temporary, asset) = fixture();
    let path = asset_path(temporary.path(), &asset);
    let outside = temporary.path().join("outside");
    fs::write(&outside, ASSET_BYTES).expect("outside bytes");
    fs::remove_file(&path).expect("remove original asset");
    symlink(&outside, &path).expect("replace asset with symlink");

    assert!(matches!(
        read_validated_asset(
            temporary.path(),
            &asset,
            asset.byte_length,
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(BundleError::Filesystem(_))
    ));
}

#[cfg(windows)]
#[tokio::test]
async fn refuses_a_reparse_point_replacement_in_the_final_component() {
    use std::os::windows::fs::symlink_file;

    let (temporary, asset) = fixture();
    let path = asset_path(temporary.path(), &asset);
    let outside = temporary.path().join("outside");
    fs::write(&outside, ASSET_BYTES).expect("outside bytes");
    fs::remove_file(&path).expect("remove original asset");
    symlink_file(&outside, &path).expect("replace asset with file symlink");

    assert!(matches!(
        read_validated_asset(
            temporary.path(),
            &asset,
            asset.byte_length,
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(BundleError::Filesystem(_))
    ));
}

#[tokio::test]
async fn honors_preexisting_cancellation_and_timeout() {
    let (temporary, asset) = fixture();
    let (controller, cancelled) = ExecutionController::new();
    assert!(controller.cancel(CancellationReason::Requested));
    assert!(matches!(
        read_validated_asset(temporary.path(), &asset, asset.byte_length, &cancelled,).await,
        Err(BundleError::Cancelled {
            reason: CancellationReason::Requested
        })
    ));

    let (_, expired) = ExecutionController::with_timeout(0, devicerail_core::TimeoutScope::Request);
    assert!(matches!(
        read_validated_asset(temporary.path(), &asset, asset.byte_length, &expired,).await,
        Err(BundleError::TimedOut { .. })
    ));
}
