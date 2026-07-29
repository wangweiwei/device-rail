use std::{fs, path::PathBuf};

use devicerail_core::ExecutionControl;
use devicerail_visualizer::{OfflineVisualizer, PageKind, PageQuery, VisualizerLimits};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PresentationExpectations {
    schema_version: u16,
    events: Vec<ExpectedEvent>,
    filters: ExpectedFilters,
    protected_omission: ExpectedProtectedOmission,
    error: ExpectedError,
    verdict: ExpectedVerdict,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExpectedEvent {
    sequence: u64,
    category: String,
    title: String,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExpectedFilters {
    all: Vec<u64>,
    observations: Vec<u64>,
    actions: Vec<u64>,
    errors: Vec<u64>,
    verdicts: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExpectedProtectedOmission {
    started_sequence: u64,
    completed_sequence: u64,
    screenshot_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExpectedError {
    sequence: u64,
    code: String,
    message: String,
    retryable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExpectedVerdict {
    sequence: u64,
    status: String,
    summary: String,
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../visualizer/fixtures/presentation-semantics")
}

fn article(html: &str, sequence: u64) -> &str {
    let id = format!("id=\"event-{sequence}\"");
    let id_offset = html.find(&id).expect("expected event id is rendered");
    let start = html[..id_offset]
        .rfind("<article ")
        .expect("event id belongs to an article");
    let relative_end = html[id_offset..]
        .find("</article>")
        .expect("event article is closed");
    &html[start..id_offset + relative_end + "</article>".len()]
}

fn rendered_sequences(html: &str) -> Vec<u64> {
    let marker = "data-sequence=\"";
    let mut remaining = html;
    let mut sequences = Vec::new();
    while let Some(offset) = remaining.find(marker) {
        remaining = &remaining[offset + marker.len()..];
        let end = remaining.find('"').expect("sequence attribute is closed");
        sequences.push(
            remaining[..end]
                .parse::<u64>()
                .expect("sequence attribute is an integer"),
        );
        remaining = &remaining[end + 1..];
    }
    sequences
}

#[tokio::test]
async fn shared_fixture_matches_rust_presentation_semantics() {
    let root = fixture_root();
    let manifest = fs::read_to_string(root.join("manifest.json")).expect("read fixture manifest");
    let expectations_source = fs::read_to_string(root.with_extension("expectations.json"))
        .expect("read fixture expectations");
    let expectations: PresentationExpectations =
        serde_json::from_str(&expectations_source).expect("typed fixture expectations");
    assert_eq!(expectations.schema_version, 1);

    for source in [&manifest, &expectations_source] {
        for forbidden in [
            "\"uri\"",
            "devicerail://",
            "file://",
            "assets/sha256/",
            "http://",
            "https://",
        ] {
            assert!(
                !source.contains(forbidden),
                "cross-language presentation fixture must not expose Evidence URI material: {forbidden}"
            );
        }
    }

    let viewer = OfflineVisualizer::open(
        &root,
        VisualizerLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("fixture is a canonical validated Session Bundle");
    assert_eq!(
        viewer.validated().session_export.events.len(),
        expectations.events.len(),
        "every validated event has one presentation expectation"
    );

    let all = viewer
        .render_page(PageQuery::new(PageKind::All, 1), "/fixture/parity")
        .expect("render complete fixture");
    assert_eq!(rendered_sequences(&all), expectations.filters.all);
    assert!(!all.contains("devicerail://"));
    assert!(!all.contains("file://"));

    for expected in &expectations.events {
        let rendered = article(&all, expected.sequence);
        assert!(
            rendered.contains(&format!("class=\"event event-{}\"", expected.category)),
            "event {} category diverged from the shared fixture",
            expected.sequence
        );
        assert!(
            rendered.contains(&format!("<h3>{}</h3>", expected.title)),
            "event {} title diverged from the shared fixture",
            expected.sequence
        );
        if let Some(status) = &expected.status {
            assert!(
                rendered.contains(&format!(">{status}<")),
                "event {} status diverged from the shared fixture",
                expected.sequence
            );
        }
    }

    let protected_start = article(&all, expectations.protected_omission.started_sequence);
    assert!(protected_start.contains("arguments were deliberately omitted"));
    assert!(!protected_start.contains("<summary>Arguments</summary>"));
    let protected_complete = article(&all, expectations.protected_omission.completed_sequence);
    assert_eq!(
        protected_complete
            .matches("Screenshot omitted:</strong> protected action")
            .count(),
        expectations.protected_omission.screenshot_count
    );

    let error = article(&all, expectations.error.sequence);
    assert!(error.contains(&expectations.error.code));
    assert!(error.contains(&expectations.error.message));
    assert!(error.contains(if expectations.error.retryable {
        "<strong>Retryable:</strong> yes"
    } else {
        "<strong>Retryable:</strong> no"
    }));

    let verdict = article(&all, expectations.verdict.sequence);
    assert!(verdict.contains(&format!(">{}<", expectations.verdict.status)));
    assert!(verdict.contains(&expectations.verdict.summary));

    for (kind, expected) in [
        (PageKind::Observations, &expectations.filters.observations),
        (PageKind::Actions, &expectations.filters.actions),
        (PageKind::Errors, &expectations.filters.errors),
        (PageKind::Verdicts, &expectations.filters.verdicts),
    ] {
        let filtered = viewer
            .render_page(PageQuery::new(kind, 1), "/fixture/parity")
            .expect("render fixture filter");
        assert_eq!(
            rendered_sequences(&filtered),
            *expected,
            "{} filter diverged from the shared fixture",
            kind.slug()
        );
    }
}
