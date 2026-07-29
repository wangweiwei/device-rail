use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io;

use devicerail_protocol::{
    ActionOutcome, AssetRef, ErrorInfo, Observation, RpcId, ScreenshotOmissionReason, TestEvent,
    TestEventPayload, VerdictStatus,
};
use devicerail_session_bundle::ValidatedBundle;
use serde::Serialize;

use crate::{PageKind, PageQuery, VisualizerError, VisualizerLimits, model::TimelineIndex};

pub const STYLESHEET: &str = include_str!("style.css");
pub(crate) const STATIC_CONTENT_SECURITY_POLICY: &str = "default-src 'none'; img-src 'self'; style-src 'self'; script-src 'none'; connect-src 'none'; media-src 'none'; object-src 'none'; frame-src 'none'; font-src 'none'; worker-src 'none'; manifest-src 'none'; base-uri 'none'; form-action 'none'";

const MAX_CAPABILITY_BASE_BYTES: usize = 512;
const MAX_EVIDENCE_LINKS_PER_BLOCK: usize = 50;
const PREVIEW_WIDTH: u32 = 960;
const PREVIEW_HEIGHT: u32 = 540;

pub(crate) fn render_page(
    snapshot: &ValidatedBundle,
    timeline_index: &TimelineIndex,
    limits: &VisualizerLimits,
    query: PageQuery,
    capability_base: &str,
    max_html_bytes: usize,
) -> Result<String, VisualizerError> {
    validate_capability_base(capability_base)?;
    render_page_with_links(
        snapshot,
        timeline_index,
        limits,
        query,
        &CapabilityLinks {
            base: capability_base,
        },
        max_html_bytes,
    )
}

pub(crate) fn render_static_page(
    snapshot: &ValidatedBundle,
    timeline_index: &TimelineIndex,
    limits: &VisualizerLimits,
    query: PageQuery,
    previewable_pngs: &BTreeSet<String>,
    max_html_bytes: usize,
) -> Result<String, VisualizerError> {
    render_page_with_links(
        snapshot,
        timeline_index,
        limits,
        query,
        &StaticLinks {
            current: query,
            previewable_pngs,
        },
        max_html_bytes,
    )
}

fn render_page_with_links(
    snapshot: &ValidatedBundle,
    timeline_index: &TimelineIndex,
    limits: &VisualizerLimits,
    query: PageQuery,
    links: &dyn RenderLinks,
    max_html_bytes: usize,
) -> Result<String, VisualizerError> {
    if query.page == 0 {
        return Err(VisualizerError::InvalidPage);
    }

    let page_size = limits.events_per_page;
    let matching_event_indices = timeline_index.event_indices(query.kind);
    let matching_events = matching_event_indices.len();
    let total_pages = matching_events.div_ceil(page_size).max(1);
    if query.page > total_pages {
        return Err(VisualizerError::PageOutOfRange {
            page: query.page,
            total_pages,
        });
    }
    let start = (query.page - 1)
        .checked_mul(page_size)
        .ok_or(VisualizerError::PageOutOfRange {
            page: query.page,
            total_pages,
        })?;
    let stylesheet_url = links.stylesheet_url();
    let mut html = BoundedHtmlWriter::new(max_html_bytes);
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    html.push_str("<meta name=\"theme-color\" content=\"#f6f7fb\" media=\"(prefers-color-scheme: light)\"><meta name=\"theme-color\" content=\"#101521\" media=\"(prefers-color-scheme: dark)\">");
    html.push_str("<meta name=\"referrer\" content=\"no-referrer\"><meta http-equiv=\"Content-Security-Policy\" content=\"");
    html.push_str(STATIC_CONTENT_SECURITY_POLICY);
    html.push_str("\">");
    html.push_str("<title>DeviceRail offline session</title><link rel=\"stylesheet\" href=\"");
    html.push_str(&stylesheet_url);
    html.push_str("\"></head><body><a class=\"skip-link\" href=\"#timeline\">Skip to timeline</a>");
    html.push_str("<header class=\"page-header\"><p class=\"eyebrow\">DeviceRail offline visualizer</p><h1>Session <bdi translate=\"no\">");
    html.push_str(&escape_text(
        &snapshot.session_export.session.id.to_string(),
        limits.max_text_chars,
    ));
    html.push_str("</bdi></h1><dl class=\"summary\"><div><dt>Events</dt><dd>");
    let _ = write!(html, "{}", snapshot.summary.event_count);
    html.push_str("</dd></div><div><dt>Assets</dt><dd>");
    let _ = write!(html, "{}", snapshot.summary.asset_count);
    html.push_str("</dd></div><div><dt>Protocol</dt><dd>");
    let _ = write!(
        html,
        "{}.{}",
        snapshot.event_protocol_version.major, snapshot.event_protocol_version.minor
    );
    html.push_str("</dd></div></dl></header>");
    html.push_str("<aside class=\"warning\" role=\"note\"><strong>Unsigned Bundle.</strong> Integrity was checked, but origin authenticity is not established.</aside>");

    render_filters(&mut html, links, query.kind);
    html.push_str("<main id=\"timeline\" tabindex=\"-1\"><div class=\"timeline-heading\"><h2>");
    html.push_str(query.kind.label());
    html.push_str("</h2><p>");
    let _ = write!(html, "Page {} of {}", query.page, total_pages);
    html.push_str("</p></div>");

    if matching_events == 0 {
        html.push_str("<section class=\"empty-state\"><h3>No matching events</h3><p>This validated Session contains no events for the selected filter.</p></section>");
    } else {
        html.push_str("<ol class=\"timeline\">");
        let end = start.saturating_add(page_size).min(matching_events);
        for &event_index in &matching_event_indices[start..end] {
            if html.exceeded() {
                break;
            }
            let event = &snapshot.session_export.events[event_index];
            render_event(&mut html, event, links, limits);
        }
        html.push_str("</ol>");
    }
    render_pagination(&mut html, links, query.kind, query.page, total_pages);
    html.push_str("</main><footer><p>Rendered from a fully validated, read-only Session snapshot.</p></footer></body></html>");
    html.finish()
}

fn render_filters(html: &mut BoundedHtmlWriter, links: &dyn RenderLinks, selected: PageKind) {
    html.push_str("<nav class=\"filters\" aria-label=\"Timeline filters\"><ul>");
    for kind in [
        PageKind::All,
        PageKind::Observations,
        PageKind::Actions,
        PageKind::Errors,
        PageKind::Verdicts,
    ] {
        html.push_str("<li><a href=\"");
        html.push_str(&links.page_url(kind, 1));
        html.push('"');
        if kind == selected {
            html.push_str(" aria-current=\"page\"");
        }
        html.push('>');
        html.push_str(kind.label());
        html.push_str("</a></li>");
    }
    html.push_str("</ul></nav>");
}

fn render_pagination(
    html: &mut BoundedHtmlWriter,
    links: &dyn RenderLinks,
    kind: PageKind,
    page: usize,
    total_pages: usize,
) {
    html.push_str("<nav class=\"pagination\" aria-label=\"Timeline pages\">");
    if page > 1 {
        html.push_str("<a rel=\"prev\" href=\"");
        html.push_str(&links.page_url(kind, page - 1));
        html.push_str("\">Previous page</a>");
    }
    if page < total_pages {
        html.push_str("<a rel=\"next\" href=\"");
        html.push_str(&links.page_url(kind, page + 1));
        html.push_str("\">Next page</a>");
    }
    html.push_str("</nav>");
}

fn render_event(
    html: &mut BoundedHtmlWriter,
    event: &TestEvent,
    links: &dyn RenderLinks,
    limits: &VisualizerLimits,
) {
    let sequence = event.sequence.get();
    html.push_str("<li><article class=\"event ");
    html.push_str(event_class(&event.payload));
    html.push_str("\" id=\"event-");
    let _ = write!(html, "{sequence}");
    html.push_str("\" data-sequence=\"");
    let _ = write!(html, "{sequence}");
    html.push_str("\"><header><span class=\"sequence\">#");
    let _ = write!(html, "{sequence}");
    html.push_str("</span><h3>");
    html.push_str(event_title(&event.payload));
    html.push_str("</h3><span class=\"event-time\">");
    let _ = write!(html, "{} ms", event.at_ms);
    html.push_str("</span></header>");

    if let Some(device_id) = &event.device_id {
        html.push_str("<p><strong>Device:</strong> <bdi translate=\"no\">");
        html.push_str(&escape_text(&device_id.0, limits.max_text_chars));
        html.push_str("</bdi></p>");
    }
    if let Some(request_id) = &event.request_id {
        html.push_str("<p><strong>Request:</strong> <bdi translate=\"no\">");
        match request_id {
            RpcId::String(value) => {
                html.push_str(&escape_text(value, limits.max_text_chars));
            }
            RpcId::Number(value) => {
                let _ = write!(html, "{value}");
            }
        }
        html.push_str("</bdi></p>");
    }

    match &event.payload {
        TestEventPayload::SessionStarted => {
            html.push_str("<p>The Session started.</p>");
        }
        TestEventPayload::SessionEnded { outcome, reason } => {
            html.push_str("<p><span class=\"status status-session\">");
            html.push_str(match outcome {
                devicerail_protocol::SessionOutcome::Completed => "Completed",
                devicerail_protocol::SessionOutcome::Failed => "Failed",
                devicerail_protocol::SessionOutcome::Cancelled => "Cancelled",
                devicerail_protocol::SessionOutcome::Shutdown => "Shutdown",
            });
            html.push_str("</span></p>");
            if let Some(reason) = reason {
                render_labeled_text(html, "Reason", reason, limits);
            }
        }
        TestEventPayload::ObservationCaptured { observation } => {
            render_observation(html, "Observation", observation, links, limits);
        }
        TestEventPayload::ActionStarted { call } => {
            render_labeled_identifier(html, "Action", &call.name, limits);
            html.push_str("<p><strong>Call:</strong> <bdi translate=\"no\">");
            html.push_str(&escape_text(&call.id.to_string(), limits.max_text_chars));
            html.push_str("</bdi></p>");
            if call.arguments_redacted {
                html.push_str("<p class=\"omission protected\"><strong>Protected action:</strong> arguments were deliberately omitted.</p>");
            } else {
                render_json(html, "Arguments", &call.arguments, limits);
            }
        }
        TestEventPayload::ActionCompleted { call_id, outcome } => {
            html.push_str("<p><strong>Call:</strong> <bdi translate=\"no\">");
            html.push_str(&escape_text(&call_id.to_string(), limits.max_text_chars));
            html.push_str("</bdi></p>");
            match outcome {
                ActionOutcome::Succeeded { result } => {
                    html.push_str(
                        "<p><span class=\"status status-succeeded\">Succeeded</span></p>",
                    );
                    render_json(html, "Output", &result.output, limits);
                    if let Some(before) = &result.before {
                        render_observation(html, "Before", before, links, limits);
                    }
                    if let Some(after) = &result.after {
                        render_observation(html, "After", after, links, limits);
                    }
                    render_evidence(html, "Action evidence", &result.evidence, links, limits);
                }
                ActionOutcome::Failed { error } => {
                    html.push_str("<p><span class=\"status status-failed\">Failed</span></p>");
                    render_error(html, error, limits);
                }
                ActionOutcome::Cancelled { error } => {
                    html.push_str(
                        "<p><span class=\"status status-cancelled\">Cancelled</span></p>",
                    );
                    render_error(html, error, limits);
                }
                ActionOutcome::TimedOut { error, timeout_ms } => {
                    html.push_str(
                        "<p><span class=\"status status-timed-out\">Timed out</span> after ",
                    );
                    let _ = write!(html, "{timeout_ms} ms");
                    html.push_str("</p>");
                    render_error(html, error, limits);
                }
            }
        }
        TestEventPayload::MediaStreamStarted { stream } => {
            render_labeled_identifier(html, "Stream", &stream.id.to_string(), limits);
            render_labeled_identifier(html, "Media type", &stream.media_type, limits);
            render_labeled_identifier(
                html,
                "Kind",
                match stream.kind {
                    devicerail_protocol::MediaStreamKind::Screenshot => "screenshot",
                    devicerail_protocol::MediaStreamKind::Video => "video",
                },
                limits,
            );
        }
        TestEventPayload::MediaFrameCaptured { frame } => {
            render_labeled_identifier(html, "Stream", &frame.stream_id.to_string(), limits);
            html.push_str("<p><strong>Frame:</strong> ");
            let _ = write!(html, "{}", frame.frame_index.get());
            if frame.key_frame {
                html.push_str(" <span class=\"status status-succeeded\">Key frame</span>");
            }
            html.push_str("</p>");
            if let Some(duration_ms) = frame.duration_ms {
                html.push_str("<p><strong>Duration:</strong> ");
                let _ = write!(html, "{duration_ms} ms");
                html.push_str("</p>");
            }
            render_evidence(
                html,
                "Frame evidence",
                std::slice::from_ref(&frame.evidence),
                links,
                limits,
            );
        }
        TestEventPayload::MediaStreamEnded {
            stream_id,
            frame_count,
        } => {
            render_labeled_identifier(html, "Stream", &stream_id.to_string(), limits);
            html.push_str("<p><strong>Frames:</strong> ");
            let _ = write!(html, "{frame_count}");
            html.push_str("</p>");
        }
        TestEventPayload::VerdictRecorded { verdict } => {
            html.push_str("<p><span class=\"status status-verdict\">");
            html.push_str(match verdict.status {
                VerdictStatus::Pass => "Pass",
                VerdictStatus::Fail => "Fail",
                VerdictStatus::Unknown => "Unknown",
            });
            html.push_str("</span></p>");
            render_labeled_text(html, "Summary", &verdict.summary, limits);
            render_evidence(html, "Verdict evidence", &verdict.evidence, links, limits);
        }
        TestEventPayload::Error { error } => render_error(html, error, limits),
    }
    html.push_str("</article></li>");
}

fn render_observation(
    html: &mut BoundedHtmlWriter,
    label: &str,
    observation: &Observation,
    links: &dyn RenderLinks,
    limits: &VisualizerLimits,
) {
    html.push_str("<section class=\"observation\"><h4>");
    html.push_str(label);
    html.push_str("</h4><dl><div><dt>Device</dt><dd><bdi translate=\"no\">");
    html.push_str(&escape_text(
        &observation.device_id.0,
        limits.max_text_chars,
    ));
    html.push_str("</bdi></dd></div><div><dt>Captured</dt><dd>");
    let _ = write!(html, "{} ms", observation.captured_at_ms);
    html.push_str("</dd></div><div><dt>Viewport</dt><dd>");
    let _ = write!(
        html,
        "{} × {} @ {}",
        observation.viewport.width, observation.viewport.height, observation.viewport.scale_factor
    );
    html.push_str("</dd></div></dl>");

    if let Some(reference) = &observation.screenshot {
        render_screenshot(html, reference, links);
    } else if let Some(reason) = observation.screenshot_omission {
        html.push_str("<p class=\"omission");
        if reason == ScreenshotOmissionReason::ProtectedAction {
            html.push_str(" protected");
        }
        html.push_str("\"><strong>Screenshot omitted:</strong> ");
        html.push_str(match reason {
            ScreenshotOmissionReason::Policy => "policy",
            ScreenshotOmissionReason::ProtectedAction => "protected action",
        });
        html.push_str(".</p>");
    } else {
        html.push_str("<p class=\"omission\">No screenshot was captured.</p>");
    }

    if !observation.metadata.is_empty() {
        render_json(html, "Metadata", &observation.metadata, limits);
    }
    html.push_str("</section>");
}

fn render_screenshot(html: &mut BoundedHtmlWriter, reference: &AssetRef, links: &dyn RenderLinks) {
    let Some(digest) = canonical_digest(reference) else {
        // This is unreachable after Bundle validation. Retain an explicit
        // omission rather than ever falling back to an attacker URI.
        html.push_str("<p class=\"omission\">Screenshot reference is unavailable.</p>");
        return;
    };
    if reference.media_type == "image/png" && links.can_preview_png(digest) {
        html.push_str("<figure><img class=\"evidence-preview\" loading=\"lazy\" src=\"");
        html.push_str(&links.image_url(digest));
        html.push_str("\" width=\"");
        let _ = write!(html, "{PREVIEW_WIDTH}");
        html.push_str("\" height=\"");
        let _ = write!(html, "{PREVIEW_HEIGHT}");
        html.push_str("\" alt=\"DeviceRail screenshot Evidence preview\"><figcaption>PNG Evidence preview. If the image does not appear, the Viewer rejected it during request-time integrity or safety validation.</figcaption></figure>");
    } else if reference.media_type == "image/png" {
        html.push_str("<p>Preview unavailable because this PNG did not pass bounded static-image validation.</p>");
    } else {
        html.push_str("<p>Preview unavailable for this Evidence media type.</p>");
    }
    html.push_str("<p><a class=\"download\" download href=\"");
    html.push_str(&links.download_url(digest));
    html.push_str("\">Download screenshot Evidence</a></p>");
}

fn render_evidence(
    html: &mut BoundedHtmlWriter,
    label: &str,
    evidence: &[AssetRef],
    links: &dyn RenderLinks,
    limits: &VisualizerLimits,
) {
    if evidence.is_empty() {
        return;
    }
    html.push_str("<section class=\"evidence\"><h4>");
    html.push_str(label);
    html.push_str("</h4><ul>");
    for (index, reference) in evidence
        .iter()
        .take(MAX_EVIDENCE_LINKS_PER_BLOCK)
        .enumerate()
    {
        let Some(digest) = canonical_digest(reference) else {
            continue;
        };
        html.push_str("<li><a download href=\"");
        html.push_str(&links.download_url(digest));
        html.push_str("\">Evidence ");
        let _ = write!(html, "{}", index + 1);
        html.push_str("</a> <span class=\"media-type\"><bdi translate=\"no\">");
        html.push_str(&escape_text(&reference.media_type, limits.max_text_chars));
        html.push_str("</bdi></span></li>");
    }
    html.push_str("</ul>");
    if evidence.len() > MAX_EVIDENCE_LINKS_PER_BLOCK {
        html.push_str("<p class=\"omission\">");
        let _ = write!(
            html,
            "{} additional Evidence references omitted by the Viewer display limit.",
            evidence.len() - MAX_EVIDENCE_LINKS_PER_BLOCK
        );
        html.push_str("</p>");
    }
    html.push_str("</section>");
}

fn render_error(html: &mut BoundedHtmlWriter, error: &ErrorInfo, limits: &VisualizerLimits) {
    html.push_str("<section class=\"error-detail\" aria-label=\"Error detail\">");
    render_labeled_identifier(html, "Code", &error.code, limits);
    render_labeled_text(html, "Message", &error.message, limits);
    html.push_str("<p><strong>Retryable:</strong> ");
    html.push_str(if error.retryable { "yes" } else { "no" });
    html.push_str("</p>");
    if let Some(details) = &error.details {
        render_json(html, "Details", details, limits);
    }
    html.push_str("</section>");
}

fn render_labeled_text(
    html: &mut BoundedHtmlWriter,
    label: &str,
    value: &str,
    limits: &VisualizerLimits,
) {
    html.push_str("<p><strong>");
    html.push_str(label);
    html.push_str(":</strong> ");
    html.push_str(&escape_text(value, limits.max_text_chars));
    html.push_str("</p>");
}

fn render_labeled_identifier(
    html: &mut BoundedHtmlWriter,
    label: &str,
    value: &str,
    limits: &VisualizerLimits,
) {
    html.push_str("<p><strong>");
    html.push_str(label);
    html.push_str(":</strong> <bdi translate=\"no\">");
    html.push_str(&escape_text(value, limits.max_text_chars));
    html.push_str("</bdi></p>");
}

fn render_json<T: Serialize + ?Sized>(
    html: &mut BoundedHtmlWriter,
    label: &str,
    value: &T,
    limits: &VisualizerLimits,
) {
    let rendered = bounded_json(value, limits.max_json_bytes);
    html.push_str("<details><summary>");
    html.push_str(label);
    html.push_str("</summary><pre>");
    html.push_str(&escape_text(&rendered, limits.max_json_bytes));
    html.push_str("</pre></details>");
}

fn event_class(payload: &TestEventPayload) -> &'static str {
    match payload {
        TestEventPayload::SessionStarted | TestEventPayload::SessionEnded { .. } => "event-session",
        TestEventPayload::ObservationCaptured { .. } => "event-observation",
        TestEventPayload::ActionStarted { .. } | TestEventPayload::ActionCompleted { .. } => {
            "event-action"
        }
        TestEventPayload::MediaStreamStarted { .. }
        | TestEventPayload::MediaFrameCaptured { .. }
        | TestEventPayload::MediaStreamEnded { .. } => "event-media",
        TestEventPayload::VerdictRecorded { .. } => "event-verdict",
        TestEventPayload::Error { .. } => "event-error",
    }
}

fn event_title(payload: &TestEventPayload) -> &'static str {
    match payload {
        TestEventPayload::SessionStarted => "Session started",
        TestEventPayload::SessionEnded { .. } => "Session ended",
        TestEventPayload::ObservationCaptured { .. } => "Observation captured",
        TestEventPayload::ActionStarted { .. } => "Action started",
        TestEventPayload::ActionCompleted { .. } => "Action completed",
        TestEventPayload::MediaStreamStarted { .. } => "Media stream started",
        TestEventPayload::MediaFrameCaptured { .. } => "Media frame captured",
        TestEventPayload::MediaStreamEnded { .. } => "Media stream ended",
        TestEventPayload::VerdictRecorded { .. } => "Verdict recorded",
        TestEventPayload::Error { .. } => "Error",
    }
}

fn canonical_digest(reference: &AssetRef) -> Option<&str> {
    let digest = reference.sha256.as_deref()?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(digest)
}

fn validate_capability_base(base: &str) -> Result<(), VisualizerError> {
    if base.is_empty()
        || base.len() > MAX_CAPABILITY_BASE_BYTES
        || !base.starts_with('/')
        || (base.len() > 1 && base.ends_with('/'))
        || base.contains("//")
        || base.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~'))
        })
        || base.split('/').any(|segment| matches!(segment, "." | ".."))
    {
        return Err(VisualizerError::InvalidCapabilityBase);
    }
    Ok(())
}

fn capability_url(base: &str, suffix: &str) -> String {
    if base == "/" {
        format!("/{suffix}")
    } else {
        format!("{base}/{suffix}")
    }
}

trait RenderLinks {
    fn stylesheet_url(&self) -> String;
    fn page_url(&self, kind: PageKind, page: usize) -> String;
    fn image_url(&self, digest: &str) -> String;
    fn download_url(&self, digest: &str) -> String;
    fn can_preview_png(&self, digest: &str) -> bool;
}

struct CapabilityLinks<'a> {
    base: &'a str,
}

impl RenderLinks for CapabilityLinks<'_> {
    fn stylesheet_url(&self) -> String {
        capability_url(self.base, "style.css")
    }

    fn page_url(&self, kind: PageKind, page: usize) -> String {
        format!("{}?kind={}&amp;page={page}", self.base, kind.slug())
    }

    fn image_url(&self, digest: &str) -> String {
        capability_url(self.base, &format!("image/{digest}"))
    }

    fn download_url(&self, digest: &str) -> String {
        capability_url(self.base, &format!("download/{digest}"))
    }

    fn can_preview_png(&self, _digest: &str) -> bool {
        true
    }
}

struct StaticLinks<'a> {
    current: PageQuery,
    previewable_pngs: &'a BTreeSet<String>,
}

impl StaticLinks<'_> {
    fn is_nested_page(&self) -> bool {
        self.current.kind != PageKind::All || self.current.page != 1
    }

    fn asset_path(&self, digest: &str) -> String {
        let extension = if self.previewable_pngs.contains(digest) {
            "png"
        } else {
            "bin"
        };
        if self.is_nested_page() {
            format!("../assets/sha256/{digest}.{extension}")
        } else {
            format!("assets/sha256/{digest}.{extension}")
        }
    }
}

impl RenderLinks for StaticLinks<'_> {
    fn stylesheet_url(&self) -> String {
        if self.is_nested_page() {
            "../style.css".to_owned()
        } else {
            "style.css".to_owned()
        }
    }

    fn page_url(&self, kind: PageKind, page: usize) -> String {
        let target = static_page_path(kind, page);
        if !self.is_nested_page() {
            return target;
        }
        if target == "index.html" {
            "../index.html".to_owned()
        } else {
            target.strip_prefix("pages/").unwrap_or(&target).to_owned()
        }
    }

    fn image_url(&self, digest: &str) -> String {
        self.asset_path(digest)
    }

    fn download_url(&self, digest: &str) -> String {
        self.asset_path(digest)
    }

    fn can_preview_png(&self, digest: &str) -> bool {
        self.previewable_pngs.contains(digest)
    }
}

pub(crate) fn static_page_path(kind: PageKind, page: usize) -> String {
    if kind == PageKind::All && page == 1 {
        "index.html".to_owned()
    } else {
        format!("pages/{}-{page}.html", kind.slug())
    }
}

struct BoundedHtmlWriter {
    value: String,
    limit: usize,
    exceeded: bool,
}

impl BoundedHtmlWriter {
    fn new(limit: usize) -> Self {
        Self {
            value: String::with_capacity(limit.min(8 * 1024)),
            limit,
            exceeded: false,
        }
    }

    fn push_str(&mut self, value: &str) {
        if self
            .value
            .len()
            .checked_add(value.len())
            .is_none_or(|length| length > self.limit)
        {
            self.exceeded = true;
            return;
        }
        if !self.exceeded {
            self.value.push_str(value);
        }
    }

    fn push(&mut self, character: char) {
        if self
            .value
            .len()
            .checked_add(character.len_utf8())
            .is_none_or(|length| length > self.limit)
        {
            self.exceeded = true;
            return;
        }
        if !self.exceeded {
            self.value.push(character);
        }
    }

    const fn exceeded(&self) -> bool {
        self.exceeded
    }

    fn finish(self) -> Result<String, VisualizerError> {
        if self.exceeded {
            Err(VisualizerError::RenderLimitExceeded)
        } else {
            Ok(self.value)
        }
    }
}

impl std::fmt::Write for BoundedHtmlWriter {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.push_str(value);
        if self.exceeded {
            Err(std::fmt::Error)
        } else {
            Ok(())
        }
    }

    fn write_char(&mut self, character: char) -> std::fmt::Result {
        self.push(character);
        if self.exceeded {
            Err(std::fmt::Error)
        } else {
            Ok(())
        }
    }
}

fn escape_text(value: &str, max_chars: usize) -> String {
    let mut escaped = String::with_capacity(value.len().min(max_chars).saturating_add(16));
    let mut chars = value.chars();
    for character in chars.by_ref().take(max_chars) {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            '\t' | '\n' | '\r' => escaped.push(character),
            character
                if character <= '\u{001f}' || ('\u{007f}'..='\u{009f}').contains(&character) =>
            {
                escaped.push('\u{fffd}');
            }
            character if is_bidi_control(character) => escaped.push('\u{fffd}'),
            _ => escaped.push(character),
        }
    }
    if chars.next().is_some() {
        escaped.push('…');
    }
    escaped
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn bounded_json<T: Serialize + ?Sized>(value: &T, max_bytes: usize) -> String {
    const TRUNCATED: &[u8] = b"\n[truncated]";
    const FAILED: &str = "[JSON serialization failed]";

    // Reserve the marker before serialization so even a truncated rendering
    // remains within the caller's exact byte budget.
    let mut writer = BoundedUtf8Writer::new(max_bytes.saturating_sub(TRUNCATED.len()));
    let serialized = serde_json::to_writer(&mut writer, value);
    if serialized.is_err() && writer.truncated {
        let remaining = max_bytes.saturating_sub(writer.bytes.len());
        writer
            .bytes
            .extend_from_slice(&TRUNCATED[..remaining.min(TRUNCATED.len())]);
    } else if serialized.is_err() {
        let mut end = FAILED.len().min(max_bytes);
        while !FAILED.is_char_boundary(end) {
            end -= 1;
        }
        return FAILED[..end].to_owned();
    }
    String::from_utf8(writer.bytes).unwrap_or_else(|_| "[JSON serialization failed]".to_owned())
}

struct BoundedUtf8Writer {
    bytes: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl BoundedUtf8Writer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(16 * 1024)),
            limit,
            truncated: false,
        }
    }
}

impl io::Write for BoundedUtf8Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if bytes.len() <= remaining {
            self.bytes.extend_from_slice(bytes);
            return Ok(bytes.len());
        }

        let mut prefix = remaining.min(bytes.len());
        while prefix > 0 && std::str::from_utf8(&bytes[..prefix]).is_err() {
            prefix -= 1;
        }
        self.bytes.extend_from_slice(&bytes[..prefix]);
        self.truncated = true;
        Err(io::Error::other("JSON presentation limit exceeded"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
