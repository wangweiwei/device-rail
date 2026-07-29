//! Trusted, offline HTML rendering for validated DeviceRail Session Bundles.
//!
//! It validates once into a trusted in-memory snapshot, renders bounded HTML,
//! and can expose that snapshot through a GET-only loopback capability server.
//! The browser never parses a Bundle manifest or receives a filesystem path.

mod model;
pub mod png;
mod render;
pub mod report;
mod server;

use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
};

use devicerail_core::ExecutionControl;
use devicerail_session_bundle::{BundleAsset, BundleError, ValidatedBundle, validate_directory};
use thiserror::Error;

use model::TimelineIndex;

pub use model::{MAX_EVENTS_PER_PAGE, PageKind, PageQuery, VisualizerLimits};
pub use render::STYLESHEET;
pub use server::{ServerError, ServerLimits, ViewerServer};

/// A fully validated, immutable-in-memory view of one offline Bundle.
///
/// The source directory is retained for the capability server, but it is never
/// serialized into HTML. A serving layer must retain the same local
/// filesystem authority assumptions as the Bundle validator and re-check
/// assets before opening them if the directory can be mutated after `open`.
pub struct OfflineVisualizer {
    root: PathBuf,
    snapshot: ValidatedBundle,
    timeline_index: TimelineIndex,
    limits: VisualizerLimits,
}

impl fmt::Debug for OfflineVisualizer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfflineVisualizer")
            .field("session_id", &self.snapshot.summary.session_id)
            .field("event_count", &self.snapshot.summary.event_count)
            .field("asset_count", &self.snapshot.summary.asset_count)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum VisualizerError {
    #[error("offline Session Bundle validation failed")]
    Bundle(#[from] BundleError),
    #[error("Visualizer limits are invalid: {0}")]
    InvalidLimits(&'static str),
    #[error("page numbers are one-based")]
    InvalidPage,
    #[error("requested page {page} is outside the available range 1..={total_pages}")]
    PageOutOfRange { page: usize, total_pages: usize },
    #[error("the capability base must be a bounded, absolute same-origin URL path")]
    InvalidCapabilityBase,
    #[error("the rendered page exceeds the configured HTML byte limit")]
    RenderLimitExceeded,
}

impl OfflineVisualizer {
    /// Validate a Bundle directory before retaining any data for rendering.
    pub async fn open(
        root: impl AsRef<Path>,
        limits: VisualizerLimits,
        control: &ExecutionControl,
    ) -> Result<Self, VisualizerError> {
        limits.validate()?;
        let root = root.as_ref().to_path_buf();
        let snapshot = validate_directory(&root, &limits.bundle, control).await?;
        let timeline_index = TimelineIndex::build(&snapshot.session_export.events);
        Ok(Self {
            root,
            snapshot,
            timeline_index,
            limits,
        })
    }

    /// Render one complete, script-free HTML document.
    ///
    /// `capability_base` is a same-origin absolute path such as
    /// `/offline/unguessable-token`. Asset links are derived only from this
    /// base and a validator-confirmed SHA-256 digest.
    pub fn render_page(
        &self,
        query: PageQuery,
        capability_base: &str,
    ) -> Result<String, VisualizerError> {
        render::render_page(
            &self.snapshot,
            &self.timeline_index,
            &self.limits,
            query,
            capability_base,
            self.limits.max_html_bytes,
        )
    }

    /// Render one self-contained static-report page with only relative links.
    ///
    /// `previewable_pngs` must contain only digests whose exact Bundle bytes
    /// passed [`png::validate_preview_png`]. Other Evidence remains available
    /// as a download but is never embedded as active image content.
    pub fn render_static_page(
        &self,
        query: PageQuery,
        previewable_pngs: &BTreeSet<String>,
    ) -> Result<String, VisualizerError> {
        render::render_static_page(
            &self.snapshot,
            &self.timeline_index,
            &self.limits,
            query,
            previewable_pngs,
            self.limits.max_html_bytes,
        )
    }

    /// Number of one-based pages for a validated filter, including one empty
    /// state page when no event matches.
    pub fn page_count(&self, kind: PageKind) -> usize {
        self.timeline_index
            .page_count(kind, self.limits.events_per_page)
    }

    pub(crate) fn render_page_with_max_bytes(
        &self,
        query: PageQuery,
        capability_base: &str,
        max_html_bytes: usize,
    ) -> Result<String, VisualizerError> {
        render::render_page(
            &self.snapshot,
            &self.timeline_index,
            &self.limits,
            query,
            capability_base,
            max_html_bytes.min(self.limits.max_html_bytes),
        )
    }

    /// The validated source root for a capability server. This path must never
    /// be exposed to the browser or included in generated diagnostics.
    pub fn bundle_root(&self) -> &Path {
        &self.root
    }

    pub fn validated(&self) -> &ValidatedBundle {
        &self.snapshot
    }

    /// Look up validator-confirmed metadata without accepting a filesystem
    /// path or an `AssetRef.uri` from the caller.
    pub fn asset_by_digest(&self, digest: &str) -> Option<&BundleAsset> {
        self.snapshot
            .assets
            .binary_search_by(|asset| asset.sha256.as_str().cmp(digest))
            .ok()
            .map(|index| &self.snapshot.assets[index])
    }
}
