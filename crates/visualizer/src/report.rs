//! Atomic, offline export of a validated Session Bundle as a static report.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io,
    path::Path,
};

use devicerail_core::{CancellationReason, ExecutionControl, TimeoutScope};
use devicerail_protocol::{ProtocolVersion, SessionId};
use devicerail_session_bundle::{
    BundleError, CanonicalJsonError, FilesystemError, StagingDir, from_canonical_slice,
    metadata_is_link_like, open_regular_file_nofollow, read_validated_asset, sync_directory,
    to_canonical_bytes,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{
    OfflineVisualizer, PageKind, PageQuery, STYLESHEET, VisualizerError, VisualizerLimits,
    png::{PngPreviewLimits, validate_preview_png},
    render::{STATIC_CONTENT_SECURITY_POLICY, static_page_path},
};

pub const REPORT_MAGIC: &str = "devicerail.static-report";
pub const REPORT_VERSION: u16 = 1;
pub const REPORT_MANIFEST_FILE: &str = "report.json";

const MAX_REPORT_MANIFEST_BYTES_CEILING: u64 = 8 * 1024 * 1024;
const MAX_REPORT_PAGES_CEILING: usize = 20_000;
const MAX_REPORT_ASSET_BYTES_CEILING: u64 = 512 * 1024 * 1024;
const MAX_REPORT_TOTAL_ASSET_BYTES_CEILING: u64 = 4 * 1024 * 1024 * 1024;
const MAX_REPORT_TOTAL_HTML_BYTES_CEILING: u64 = 1024 * 1024 * 1024;
const COPY_CHUNK_BYTES: usize = 64 * 1024;

const PAGE_KINDS: [PageKind; 5] = [
    PageKind::All,
    PageKind::Observations,
    PageKind::Actions,
    PageKind::Errors,
    PageKind::Verdicts,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportLimits {
    pub visualizer: VisualizerLimits,
    pub max_manifest_bytes: u64,
    pub max_pages: usize,
    pub max_asset_bytes: u64,
    pub max_total_asset_bytes: u64,
    pub max_total_html_bytes: u64,
}

impl Default for ReportLimits {
    fn default() -> Self {
        Self {
            visualizer: VisualizerLimits::default(),
            max_manifest_bytes: MAX_REPORT_MANIFEST_BYTES_CEILING,
            max_pages: 10_000,
            max_asset_bytes: 64 * 1024 * 1024,
            max_total_asset_bytes: 512 * 1024 * 1024,
            max_total_html_bytes: 256 * 1024 * 1024,
        }
    }
}

impl ReportLimits {
    fn validate(self) -> Result<Self, ReportError> {
        if self.max_manifest_bytes == 0
            || self.max_manifest_bytes > MAX_REPORT_MANIFEST_BYTES_CEILING
            || self.max_pages < PAGE_KINDS.len()
            || self.max_pages > MAX_REPORT_PAGES_CEILING
            || self.max_asset_bytes == 0
            || self.max_asset_bytes > MAX_REPORT_ASSET_BYTES_CEILING
            || self.max_total_asset_bytes == 0
            || self.max_total_asset_bytes > MAX_REPORT_TOTAL_ASSET_BYTES_CEILING
            || self.max_total_asset_bytes < self.max_asset_bytes
            || self.max_total_html_bytes == 0
            || self.max_total_html_bytes > MAX_REPORT_TOTAL_HTML_BYTES_CEILING
        {
            return Err(ReportError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportSummary {
    pub session_id: SessionId,
    pub event_count: u64,
    pub page_count: u64,
    pub asset_count: u64,
    pub asset_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportFile {
    pub path: String,
    pub sha256: String,
    pub byte_length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportPage {
    pub kind: String,
    pub page: u64,
    #[serde(flatten)]
    pub file: ReportFile,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportAsset {
    pub sha256: String,
    pub media_type: String,
    pub byte_length: u64,
    pub path: String,
    pub previewable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportManifest {
    pub magic: String,
    pub report_version: u16,
    pub event_protocol_version: ProtocolVersion,
    pub session_id: SessionId,
    pub event_count: u64,
    pub source_asset_count: u64,
    pub source_asset_bytes: u64,
    pub stylesheet: ReportFile,
    pub pages: Vec<ReportPage>,
    pub assets: Vec<ReportAsset>,
}

#[derive(Debug, Error)]
pub enum ReportError {
    #[error("static report limits are invalid")]
    InvalidLimits,
    #[error("the Session Bundle could not be opened for report export")]
    Bundle(#[source] BundleError),
    #[error("the report target already exists")]
    TargetExists,
    #[error("the report target must be outside the source Bundle tree")]
    InvalidTarget,
    #[error("the report filesystem operation failed")]
    Filesystem(#[source] FilesystemError),
    #[error("the report was published, but parent-directory durability is unknown")]
    PublishedDurabilityUnknown,
    #[error("the report manifest is not canonical or does not match its strict model")]
    Manifest(#[source] CanonicalJsonError),
    #[error("the report manifest exceeds its configured byte limit")]
    ManifestTooLarge,
    #[error("the report directory contains an unexpected, missing, or unsafe node")]
    InvalidTree,
    #[error("the report manifest is internally inconsistent")]
    InvalidManifest,
    #[error("the report page limit was exceeded")]
    PageLimitExceeded,
    #[error("the report HTML byte limit was exceeded")]
    HtmlLimitExceeded,
    #[error("the report Evidence byte limit was exceeded")]
    AssetLimitExceeded,
    #[error("PNG Evidence did not pass bounded static-image validation")]
    UnsafePng,
    #[error("the report stylesheet does not match the built-in static stylesheet")]
    UnsafeStylesheet,
    #[error("a report page contains unsafe or non-local HTML")]
    UnsafeHtml,
    #[error("a generated report file failed read-back integrity validation")]
    IntegrityMismatch,
    #[error("the report operation was cancelled: {reason:?}")]
    Cancelled { reason: CancellationReason },
    #[error("the report operation timed out after {timeout_ms} ms")]
    TimedOut {
        scope: TimeoutScope,
        timeout_ms: u64,
    },
    #[error("the report I/O operation failed")]
    Io(#[source] io::Error),
    #[error("the bounded PNG validation task failed")]
    Task,
    #[error(transparent)]
    Visualizer(#[from] VisualizerError),
}

/// Export a validated Bundle into a script-free, relative-link static report.
///
/// Publication is atomic and no-clobber. Every source Evidence object is
/// reopened and hashed after Bundle validation; exact `image/png` objects must
/// additionally pass the complete bounded PNG decoder before receiving a
/// `.png` filename or an inline `<img>` reference. Other bytes use `.bin`.
pub async fn export_static_report(
    bundle_root: impl AsRef<Path>,
    target: impl AsRef<Path>,
    limits: ReportLimits,
    control: &ExecutionControl,
) -> Result<ReportSummary, ReportError> {
    let limits = limits.validate()?;
    check_control(control)?;
    let viewer = OfflineVisualizer::open(bundle_root.as_ref(), limits.visualizer, control)
        .await
        .map_err(map_visualizer_open_error)?;
    let target = target.as_ref();
    ensure_target_outside_bundle(viewer.bundle_root(), target)?;
    let staging = StagingDir::create(target).map_err(map_filesystem_error)?;
    create_private_directory(&staging.path().join("pages"))?;

    let mut previewable_pngs = BTreeSet::new();
    let mut report_assets = Vec::with_capacity(viewer.validated().assets.len());
    let mut total_asset_bytes = 0_u64;
    if !viewer.validated().assets.is_empty() {
        create_private_directory(&staging.path().join("assets"))?;
        create_private_directory(&staging.path().join("assets").join("sha256"))?;
    }

    for asset in &viewer.validated().assets {
        check_control(control)?;
        let read =
            read_validated_asset(viewer.bundle_root(), asset, limits.max_asset_bytes, control)
                .await
                .map_err(map_bundle_error)?;
        total_asset_bytes = total_asset_bytes
            .checked_add(read.byte_length)
            .ok_or(ReportError::AssetLimitExceeded)?;
        if total_asset_bytes > limits.max_total_asset_bytes {
            return Err(ReportError::AssetLimitExceeded);
        }

        let previewable = read.media_type == "image/png";
        let bytes = if previewable {
            let bytes = read.bytes;
            let png_limits = PngPreviewLimits {
                max_encoded_bytes: usize::try_from(limits.max_asset_bytes).unwrap_or(usize::MAX),
                ..PngPreviewLimits::default()
            };
            let (validation, bytes) = tokio::task::spawn_blocking(move || {
                let validation = validate_preview_png(&bytes, png_limits);
                (validation, bytes)
            })
            .await
            .map_err(|_| ReportError::Task)?;
            validation.map_err(|_| ReportError::UnsafePng)?;
            previewable_pngs.insert(read.sha256.clone());
            bytes
        } else {
            read.bytes
        };
        let path = static_asset_path(&read.sha256, previewable);
        write_private_file(&staging.path().join(&path), &bytes, control).await?;
        report_assets.push(ReportAsset {
            sha256: read.sha256,
            media_type: read.media_type,
            byte_length: read.byte_length,
            path,
            previewable,
        });
    }

    let mut total_pages = 0_usize;
    for kind in PAGE_KINDS {
        total_pages = total_pages
            .checked_add(viewer.page_count(kind))
            .ok_or(ReportError::PageLimitExceeded)?;
    }
    if total_pages > limits.max_pages {
        return Err(ReportError::PageLimitExceeded);
    }

    let mut report_pages = Vec::with_capacity(total_pages);
    let mut total_html_bytes = 0_u64;
    for kind in PAGE_KINDS {
        for page in 1..=viewer.page_count(kind) {
            check_control(control)?;
            let html = viewer.render_static_page(PageQuery::new(kind, page), &previewable_pngs)?;
            let html_bytes = html.as_bytes();
            total_html_bytes = total_html_bytes
                .checked_add(html_bytes.len() as u64)
                .ok_or(ReportError::HtmlLimitExceeded)?;
            if total_html_bytes > limits.max_total_html_bytes {
                return Err(ReportError::HtmlLimitExceeded);
            }
            let path = static_page_path(kind, page);
            write_private_file(&staging.path().join(&path), html_bytes, control).await?;
            report_pages.push(ReportPage {
                kind: kind.slug().to_owned(),
                page: page as u64,
                file: report_file(path, html_bytes),
            });
        }
    }

    write_private_file(
        staging.path().join("style.css").as_path(),
        STYLESHEET.as_bytes(),
        control,
    )
    .await?;
    let stylesheet = report_file("style.css".to_owned(), STYLESHEET.as_bytes());
    let manifest = ReportManifest {
        magic: REPORT_MAGIC.to_owned(),
        report_version: REPORT_VERSION,
        event_protocol_version: viewer.validated().event_protocol_version,
        session_id: viewer.validated().summary.session_id.clone(),
        event_count: viewer.validated().summary.event_count,
        source_asset_count: viewer.validated().summary.asset_count,
        source_asset_bytes: viewer.validated().summary.asset_bytes,
        stylesheet,
        pages: report_pages,
        assets: report_assets,
    };
    validate_manifest_model(&manifest, limits)?;
    let manifest_bytes = to_canonical_bytes(&manifest).map_err(ReportError::Manifest)?;
    if manifest_bytes.len() as u64 > limits.max_manifest_bytes {
        return Err(ReportError::ManifestTooLarge);
    }
    write_private_file(
        &staging.path().join(REPORT_MANIFEST_FILE),
        &manifest_bytes,
        control,
    )
    .await?;

    sync_report_directories(staging.path(), !manifest.assets.is_empty())?;
    let expected = summary_from_manifest(&manifest)?;
    let read_back = validate_static_report(staging.path(), limits, control).await?;
    if read_back != expected {
        return Err(ReportError::IntegrityMismatch);
    }
    check_control(control)?;
    staging.commit().map_err(map_filesystem_error)?;
    Ok(expected)
}

/// Validate an exported report directory, including exact tree shape and every
/// generated page, stylesheet, and Evidence digest.
pub async fn validate_static_report(
    root: impl AsRef<Path>,
    limits: ReportLimits,
    control: &ExecutionControl,
) -> Result<ReportSummary, ReportError> {
    let limits = limits.validate()?;
    let root = root.as_ref();
    require_real_directory(root)?;
    let manifest_bytes = read_bounded_file(
        &root.join(REPORT_MANIFEST_FILE),
        limits.max_manifest_bytes,
        control,
    )
    .await?;
    let manifest: ReportManifest =
        from_canonical_slice(&manifest_bytes).map_err(ReportError::Manifest)?;
    validate_manifest_model(&manifest, limits)?;
    validate_tree(root, &manifest)?;

    let stylesheet = read_validated_file(
        root,
        &manifest.stylesheet,
        limits.max_manifest_bytes,
        control,
    )
    .await?;
    if stylesheet.as_slice() != STYLESHEET.as_bytes() {
        return Err(ReportError::UnsafeStylesheet);
    }
    let mut total_html = 0_u64;
    for page in &manifest.pages {
        total_html = total_html
            .checked_add(page.file.byte_length)
            .ok_or(ReportError::HtmlLimitExceeded)?;
        if total_html > limits.max_total_html_bytes {
            return Err(ReportError::HtmlLimitExceeded);
        }
        let html = read_validated_file(
            root,
            &page.file,
            limits.visualizer.max_html_bytes as u64,
            control,
        )
        .await?;
        validate_static_html(&html, page, &manifest)?;
    }

    let mut total_assets = 0_u64;
    for asset in &manifest.assets {
        total_assets = total_assets
            .checked_add(asset.byte_length)
            .ok_or(ReportError::AssetLimitExceeded)?;
        if total_assets > limits.max_total_asset_bytes {
            return Err(ReportError::AssetLimitExceeded);
        }
        let bytes =
            read_bounded_file(&root.join(&asset.path), limits.max_asset_bytes, control).await?;
        if bytes.len() as u64 != asset.byte_length || sha256(&bytes) != asset.sha256 {
            return Err(ReportError::IntegrityMismatch);
        }
        if asset.previewable {
            let png_limits = PngPreviewLimits {
                max_encoded_bytes: usize::try_from(limits.max_asset_bytes).unwrap_or(usize::MAX),
                ..PngPreviewLimits::default()
            };
            tokio::task::spawn_blocking(move || validate_preview_png(&bytes, png_limits))
                .await
                .map_err(|_| ReportError::Task)?
                .map_err(|_| ReportError::UnsafePng)?;
        }
    }
    check_control(control)?;
    summary_from_manifest(&manifest)
}

fn validate_manifest_model(
    manifest: &ReportManifest,
    limits: ReportLimits,
) -> Result<(), ReportError> {
    if manifest.magic != REPORT_MAGIC
        || manifest.report_version != REPORT_VERSION
        || manifest.pages.len() > limits.max_pages
        || manifest.pages.len() < PAGE_KINDS.len()
        || manifest.assets.len() as u64 != manifest.source_asset_count
        || manifest.stylesheet.path != "style.css"
        || manifest.stylesheet.byte_length != STYLESHEET.len() as u64
        || manifest.stylesheet.sha256 != sha256(STYLESHEET.as_bytes())
    {
        return Err(ReportError::InvalidManifest);
    }

    let mut page_index = 0_usize;
    for kind in PAGE_KINDS {
        let mut expected_page = 1_u64;
        while let Some(page) = manifest.pages.get(page_index) {
            if page.kind != kind.slug() {
                break;
            }
            let page_number =
                usize::try_from(page.page).map_err(|_| ReportError::InvalidManifest)?;
            if page.page != expected_page
                || page.file.path != static_page_path(kind, page_number)
                || page.file.byte_length == 0
                || page.file.byte_length > limits.visualizer.max_html_bytes as u64
                || !is_digest(&page.file.sha256)
            {
                return Err(ReportError::InvalidManifest);
            }
            expected_page += 1;
            page_index += 1;
        }
        if expected_page == 1 {
            return Err(ReportError::InvalidManifest);
        }
    }
    if page_index != manifest.pages.len() {
        return Err(ReportError::InvalidManifest);
    }

    let mut previous_digest: Option<&str> = None;
    let mut total_assets = 0_u64;
    for asset in &manifest.assets {
        if !is_digest(&asset.sha256)
            || previous_digest.is_some_and(|previous| previous >= asset.sha256.as_str())
            || asset.path != static_asset_path(&asset.sha256, asset.previewable)
            || asset.previewable != (asset.media_type == "image/png")
            || asset.byte_length > limits.max_asset_bytes
        {
            return Err(ReportError::InvalidManifest);
        }
        total_assets = total_assets
            .checked_add(asset.byte_length)
            .ok_or(ReportError::AssetLimitExceeded)?;
        if total_assets > limits.max_total_asset_bytes {
            return Err(ReportError::AssetLimitExceeded);
        }
        previous_digest = Some(&asset.sha256);
    }
    if total_assets != manifest.source_asset_bytes {
        return Err(ReportError::InvalidManifest);
    }
    Ok(())
}

fn validate_tree(root: &Path, manifest: &ReportManifest) -> Result<(), ReportError> {
    let mut expected_root = BTreeSet::from([
        REPORT_MANIFEST_FILE.to_owned(),
        "index.html".to_owned(),
        "pages".to_owned(),
        "style.css".to_owned(),
    ]);
    if !manifest.assets.is_empty() {
        expected_root.insert("assets".to_owned());
    }
    if directory_entries(root, expected_root.len())? != expected_root {
        return Err(ReportError::InvalidTree);
    }
    require_regular_file(&root.join(REPORT_MANIFEST_FILE))?;
    require_regular_file(&root.join("index.html"))?;
    require_regular_file(&root.join("style.css"))?;
    require_real_directory(&root.join("pages"))?;

    let expected_pages = manifest
        .pages
        .iter()
        .filter(|page| page.file.path != "index.html")
        .map(|page| {
            page.file
                .path
                .strip_prefix("pages/")
                .map(ToOwned::to_owned)
                .ok_or(ReportError::InvalidManifest)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if expected_pages.len() + 1 != manifest.pages.len()
        || directory_entries(&root.join("pages"), expected_pages.len())? != expected_pages
    {
        return Err(ReportError::InvalidTree);
    }
    for page in &manifest.pages {
        require_regular_file(&root.join(&page.file.path))?;
    }

    if manifest.assets.is_empty() {
        return Ok(());
    }
    let assets_root = root.join("assets");
    let sha_root = assets_root.join("sha256");
    require_real_directory(&assets_root)?;
    if directory_entries(&assets_root, 1)? != BTreeSet::from(["sha256".to_owned()]) {
        return Err(ReportError::InvalidTree);
    }
    require_real_directory(&sha_root)?;
    let expected_assets = manifest
        .assets
        .iter()
        .map(|asset| {
            asset
                .path
                .strip_prefix("assets/sha256/")
                .map(ToOwned::to_owned)
                .ok_or(ReportError::InvalidManifest)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if directory_entries(&sha_root, expected_assets.len())? != expected_assets {
        return Err(ReportError::InvalidTree);
    }
    for asset in &manifest.assets {
        require_regular_file(&root.join(&asset.path))?;
    }
    Ok(())
}

fn validate_static_html(
    bytes: &[u8],
    page: &ReportPage,
    manifest: &ReportManifest,
) -> Result<(), ReportError> {
    let html = std::str::from_utf8(bytes).map_err(|_| ReportError::UnsafeHtml)?;
    if !html.starts_with("<!doctype html><html lang=\"en\"><head>")
        || !html.ends_with("</body></html>")
        || html.matches("<!doctype html>").count() != 1
    {
        return Err(ReportError::UnsafeHtml);
    }
    let expected_csp = format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"{STATIC_CONTENT_SECURITY_POLICY}\">"
    );
    if html.matches(&expected_csp).count() != 1 {
        return Err(ReportError::UnsafeHtml);
    }

    let nested = page.file.path != "index.html";
    let page_paths = manifest
        .pages
        .iter()
        .map(|candidate| candidate.file.path.as_str())
        .collect::<BTreeSet<_>>();
    let asset_paths = manifest
        .assets
        .iter()
        .map(|asset| asset.path.as_str())
        .collect::<BTreeSet<_>>();
    let preview_paths = manifest
        .assets
        .iter()
        .filter(|asset| asset.previewable)
        .map(|asset| asset.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut csp_tags = 0_usize;
    let mut stylesheet_tags = 0_usize;
    let mut cursor = 0_usize;
    while let Some(relative_start) = html[cursor..].find('<') {
        let start = cursor + relative_start;
        let end = html[start + 1..]
            .find('>')
            .map(|relative| start + 1 + relative)
            .ok_or(ReportError::UnsafeHtml)?;
        let source = &html[start + 1..end];
        if source == "!doctype html" {
            if start != 0 {
                return Err(ReportError::UnsafeHtml);
            }
            cursor = end + 1;
            continue;
        }
        let tag = parse_html_tag(source)?;
        if !allowed_static_tag(&tag.name) {
            return Err(ReportError::UnsafeHtml);
        }
        if tag.closing {
            if !tag.attributes.is_empty() {
                return Err(ReportError::UnsafeHtml);
            }
            cursor = end + 1;
            continue;
        }

        let mut names = BTreeSet::new();
        for (name, value) in &tag.attributes {
            if !names.insert(name.as_str()) || !allowed_static_attribute(name) {
                return Err(ReportError::UnsafeHtml);
            }
            if value.is_none() && !(tag.name == "a" && name == "download") {
                return Err(ReportError::UnsafeHtml);
            }
        }
        let attribute = |name: &str| {
            tag.attributes
                .iter()
                .find(|(candidate, _)| candidate == name)
                .and_then(|(_, value)| value.as_deref())
        };

        if let Some(value) = attribute("http-equiv") {
            if tag.name != "meta"
                || value != "Content-Security-Policy"
                || attribute("content") != Some(STATIC_CONTENT_SECURITY_POLICY)
            {
                return Err(ReportError::UnsafeHtml);
            }
            csp_tags += 1;
        }
        if tag.name == "link" {
            if tag.attributes.len() != 2
                || attribute("rel") != Some("stylesheet")
                || resolve_report_url(attribute("href").ok_or(ReportError::UnsafeHtml)?, nested)
                    .as_deref()
                    != Some("style.css")
            {
                return Err(ReportError::UnsafeHtml);
            }
            stylesheet_tags += 1;
        }
        if let Some(href) = attribute("href") {
            if tag.name != "a" && tag.name != "link" {
                return Err(ReportError::UnsafeHtml);
            }
            if tag.name == "a" && href != "#timeline" {
                let resolved = resolve_report_url(href, nested).ok_or(ReportError::UnsafeHtml)?;
                if !page_paths.contains(resolved.as_str())
                    && !asset_paths.contains(resolved.as_str())
                {
                    return Err(ReportError::UnsafeHtml);
                }
            }
        } else if tag.name == "a" {
            return Err(ReportError::UnsafeHtml);
        }
        if let Some(src) = attribute("src") {
            if tag.name != "img" {
                return Err(ReportError::UnsafeHtml);
            }
            let resolved = resolve_report_url(src, nested).ok_or(ReportError::UnsafeHtml)?;
            if !preview_paths.contains(resolved.as_str()) {
                return Err(ReportError::UnsafeHtml);
            }
        } else if tag.name == "img" {
            return Err(ReportError::UnsafeHtml);
        }
        if let Some(rel) = attribute("rel")
            && !((tag.name == "link" && rel == "stylesheet")
                || (tag.name == "a" && matches!(rel, "prev" | "next")))
        {
            return Err(ReportError::UnsafeHtml);
        }
        cursor = end + 1;
    }
    if csp_tags != 1 || stylesheet_tags != 1 {
        return Err(ReportError::UnsafeHtml);
    }
    Ok(())
}

struct HtmlTag {
    name: String,
    closing: bool,
    attributes: Vec<(String, Option<String>)>,
}

fn parse_html_tag(source: &str) -> Result<HtmlTag, ReportError> {
    let bytes = source.as_bytes();
    let mut cursor = 0_usize;
    let closing = bytes.first() == Some(&b'/');
    if closing {
        cursor += 1;
    }
    let name_start = cursor;
    while bytes
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        cursor += 1;
    }
    if cursor == name_start {
        return Err(ReportError::UnsafeHtml);
    }
    let name = source[name_start..cursor].to_owned();
    let mut attributes = Vec::new();
    while cursor < bytes.len() {
        if !bytes[cursor].is_ascii_whitespace() {
            return Err(ReportError::UnsafeHtml);
        }
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let attribute_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b':')
        }) {
            cursor += 1;
        }
        if cursor == attribute_start {
            return Err(ReportError::UnsafeHtml);
        }
        let attribute = source[attribute_start..cursor].to_owned();
        let attribute_end = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            cursor += 1;
        }
        let value = if bytes.get(cursor) == Some(&b'=') {
            cursor += 1;
            while bytes
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                cursor += 1;
            }
            if bytes.get(cursor) != Some(&b'"') {
                return Err(ReportError::UnsafeHtml);
            }
            cursor += 1;
            let value_start = cursor;
            while bytes.get(cursor).is_some_and(|byte| *byte != b'"') {
                if bytes[cursor] == b'<' {
                    return Err(ReportError::UnsafeHtml);
                }
                cursor += 1;
            }
            if bytes.get(cursor) != Some(&b'"') {
                return Err(ReportError::UnsafeHtml);
            }
            let value = source[value_start..cursor].to_owned();
            cursor += 1;
            Some(value)
        } else {
            cursor = attribute_end;
            None
        };
        attributes.push((attribute, value));
    }
    Ok(HtmlTag {
        name,
        closing,
        attributes,
    })
}

fn allowed_static_tag(name: &str) -> bool {
    matches!(
        name,
        "html"
            | "head"
            | "meta"
            | "title"
            | "link"
            | "body"
            | "a"
            | "header"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "bdi"
            | "dl"
            | "div"
            | "dt"
            | "dd"
            | "aside"
            | "strong"
            | "nav"
            | "ul"
            | "ol"
            | "li"
            | "main"
            | "section"
            | "article"
            | "span"
            | "figure"
            | "img"
            | "figcaption"
            | "details"
            | "summary"
            | "pre"
            | "footer"
    )
}

fn allowed_static_attribute(name: &str) -> bool {
    matches!(
        name,
        "alt"
            | "aria-current"
            | "aria-label"
            | "charset"
            | "class"
            | "content"
            | "data-sequence"
            | "download"
            | "height"
            | "href"
            | "http-equiv"
            | "id"
            | "lang"
            | "loading"
            | "media"
            | "name"
            | "rel"
            | "role"
            | "src"
            | "tabindex"
            | "translate"
            | "width"
    )
}

fn resolve_report_url(value: &str, nested: bool) -> Option<String> {
    if value.is_empty()
        || value == "#timeline"
        || value.bytes().any(|byte| {
            !(byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'/' | b'.' | b'-' | b'_'))
        })
    {
        return None;
    }
    let resolved = if nested {
        value
            .strip_prefix("../")
            .map_or_else(|| format!("pages/{value}"), ToOwned::to_owned)
    } else {
        value.to_owned()
    };
    if resolved.starts_with('/')
        || resolved.contains("//")
        || resolved
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return None;
    }
    Some(resolved)
}

async fn read_validated_file(
    root: &Path,
    file: &ReportFile,
    max_bytes: u64,
    control: &ExecutionControl,
) -> Result<Vec<u8>, ReportError> {
    let bytes = read_bounded_file(&root.join(&file.path), max_bytes, control).await?;
    if bytes.len() as u64 != file.byte_length || sha256(&bytes) != file.sha256 {
        return Err(ReportError::IntegrityMismatch);
    }
    Ok(bytes)
}

async fn read_bounded_file(
    path: &Path,
    max_bytes: u64,
    control: &ExecutionControl,
) -> Result<Vec<u8>, ReportError> {
    check_control(control)?;
    let file = open_regular_file_nofollow(path).map_err(map_filesystem_error)?;
    let length = file.metadata().map_err(ReportError::Io)?.len();
    if length > max_bytes {
        return Err(ReportError::IntegrityMismatch);
    }
    let capacity = usize::try_from(length).map_err(|_| ReportError::IntegrityMismatch)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| ReportError::IntegrityMismatch)?;
    let mut file = tokio::fs::File::from_std(file);
    let mut buffer = vec![0_u8; COPY_CHUNK_BYTES];
    loop {
        check_control(control)?;
        let count = file.read(&mut buffer).await.map_err(ReportError::Io)?;
        if count == 0 {
            break;
        }
        if bytes
            .len()
            .checked_add(count)
            .is_none_or(|observed| observed as u64 > max_bytes)
        {
            return Err(ReportError::IntegrityMismatch);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    check_control(control)?;
    Ok(bytes)
}

async fn write_private_file(
    path: &Path,
    bytes: &[u8],
    control: &ExecutionControl,
) -> Result<(), ReportError> {
    check_control(control)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(ReportError::Io)?;
    let mut file = tokio::fs::File::from_std(file);
    for chunk in bytes.chunks(COPY_CHUNK_BYTES) {
        check_control(control)?;
        file.write_all(chunk).await.map_err(ReportError::Io)?;
    }
    file.sync_all().await.map_err(ReportError::Io)?;
    check_control(control)
}

fn create_private_directory(path: &Path) -> Result<(), ReportError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path).map_err(ReportError::Io)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path).map_err(ReportError::Io)
    }
}

fn sync_report_directories(root: &Path, has_assets: bool) -> Result<(), ReportError> {
    sync_directory(&root.join("pages")).map_err(map_filesystem_error)?;
    if has_assets {
        sync_directory(&root.join("assets").join("sha256")).map_err(map_filesystem_error)?;
        sync_directory(&root.join("assets")).map_err(map_filesystem_error)?;
    }
    sync_directory(root).map_err(map_filesystem_error)
}

fn directory_entries(path: &Path, maximum: usize) -> Result<BTreeSet<String>, ReportError> {
    require_real_directory(path)?;
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(path).map_err(ReportError::Io)? {
        let entry = entry.map_err(ReportError::Io)?;
        if names.len() >= maximum {
            return Err(ReportError::InvalidTree);
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| ReportError::InvalidTree)?;
        if !names.insert(name) {
            return Err(ReportError::InvalidTree);
        }
    }
    Ok(names)
}

fn ensure_target_outside_bundle(bundle_root: &Path, target: &Path) -> Result<(), ReportError> {
    let name = target.file_name().ok_or(ReportError::InvalidTarget)?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(ReportError::InvalidTarget);
    }
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    require_real_directory(parent)?;
    let source = bundle_root.canonicalize().map_err(ReportError::Io)?;
    let parent = parent.canonicalize().map_err(ReportError::Io)?;
    let target = parent.join(name);
    if target.starts_with(&source) || source.starts_with(&target) {
        return Err(ReportError::InvalidTarget);
    }
    Ok(())
}

fn require_real_directory(path: &Path) -> Result<(), ReportError> {
    let metadata = fs::symlink_metadata(path).map_err(ReportError::Io)?;
    if metadata_is_link_like(&metadata) || !metadata.is_dir() {
        return Err(ReportError::InvalidTree);
    }
    Ok(())
}

fn require_regular_file(path: &Path) -> Result<(), ReportError> {
    let metadata = fs::symlink_metadata(path).map_err(ReportError::Io)?;
    if metadata_is_link_like(&metadata) || !metadata.is_file() {
        return Err(ReportError::InvalidTree);
    }
    Ok(())
}

fn report_file(path: String, bytes: &[u8]) -> ReportFile {
    ReportFile {
        path,
        sha256: sha256(bytes),
        byte_length: bytes.len() as u64,
    }
}

fn summary_from_manifest(manifest: &ReportManifest) -> Result<ReportSummary, ReportError> {
    Ok(ReportSummary {
        session_id: manifest.session_id.clone(),
        event_count: manifest.event_count,
        page_count: manifest
            .pages
            .len()
            .try_into()
            .map_err(|_| ReportError::InvalidManifest)?,
        asset_count: manifest.source_asset_count,
        asset_bytes: manifest.source_asset_bytes,
    })
}

fn static_asset_path(digest: &str, previewable: bool) -> String {
    let extension = if previewable { "png" } else { "bin" };
    format!("assets/sha256/{digest}.{extension}")
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn check_control(control: &ExecutionControl) -> Result<(), ReportError> {
    if let Some(reason) = control.cancellation_reason() {
        return Err(ReportError::Cancelled { reason });
    }
    if control.is_expired() {
        let (scope, timeout_ms) = control.timeout().unwrap_or((TimeoutScope::Request, 0));
        return Err(ReportError::TimedOut { scope, timeout_ms });
    }
    Ok(())
}

fn map_visualizer_open_error(error: VisualizerError) -> ReportError {
    match error {
        VisualizerError::Bundle(error) => ReportError::Bundle(error),
        other => ReportError::Visualizer(other),
    }
}

fn map_bundle_error(error: BundleError) -> ReportError {
    match error {
        BundleError::AssetLimitExceeded => ReportError::AssetLimitExceeded,
        BundleError::Cancelled { reason } => ReportError::Cancelled { reason },
        BundleError::TimedOut { scope, timeout_ms } => ReportError::TimedOut { scope, timeout_ms },
        other => ReportError::Bundle(other),
    }
}

fn map_filesystem_error(error: FilesystemError) -> ReportError {
    match error {
        FilesystemError::DestinationExists(_) => ReportError::TargetExists,
        FilesystemError::PublishedDurabilityUnknown { .. } => {
            ReportError::PublishedDurabilityUnknown
        }
        other => ReportError::Filesystem(other),
    }
}
