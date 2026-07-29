use std::{fs, path::Path};

use devicerail_core::{CancellationReason, ExecutionControl, ExecutionController, TimeoutScope};
use devicerail_session_bundle::{BundleManifest, from_canonical_slice, to_canonical_bytes};
use devicerail_visualizer::{
    VisualizerLimits,
    report::{
        REPORT_MAGIC, ReportError, ReportLimits, ReportManifest, export_static_report,
        validate_static_report,
    },
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

struct Asset {
    digest: String,
    media_type: &'static str,
    bytes: Vec<u8>,
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn coordinate_report_file(report: &Path, relative: &str, bytes: &[u8]) {
    fs::write(report.join(relative), bytes).expect("rewrite report file");
    let manifest_path = report.join("report.json");
    let mut manifest: ReportManifest =
        from_canonical_slice(&fs::read(&manifest_path).expect("read report manifest"))
            .expect("canonical report manifest");
    let file = if relative == "style.css" {
        &mut manifest.stylesheet
    } else {
        &mut manifest
            .pages
            .iter_mut()
            .find(|page| page.file.path == relative)
            .expect("manifest page")
            .file
    };
    file.byte_length = bytes.len() as u64;
    file.sha256 = digest(bytes);
    fs::write(
        manifest_path,
        to_canonical_bytes(&manifest).expect("rewrite canonical manifest"),
    )
    .expect("write report manifest");
}

fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer
            .write_image_data(&[0x20, 0x40, 0x60, 0xff])
            .expect("PNG pixels");
    }
    bytes
}

fn event(sequence: u64, payload: Value) -> Value {
    json!({
        "eventId": format!("aaaaaaaa-aaaa-4aaa-8aaa-{sequence:012x}"),
        "sessionId": SESSION_ID,
        "sequence": sequence,
        "atMs": 1_000 + sequence,
        "payload": payload
    })
}

fn write_bundle(parent: &Path, screenshot_bytes: Vec<u8>) -> (std::path::PathBuf, String, String) {
    let root = parent.join("bundle");
    fs::create_dir(&root).expect("Bundle root");
    let png_digest = digest(&screenshot_bytes);
    let active_bytes = b"<script>never execute</script>".to_vec();
    let active_digest = digest(&active_bytes);
    let mut assets = vec![
        Asset {
            digest: png_digest.clone(),
            media_type: "image/png",
            bytes: screenshot_bytes,
        },
        Asset {
            digest: active_digest.clone(),
            media_type: "text/html",
            bytes: active_bytes,
        },
    ];
    assets.sort_by(|left, right| left.digest.cmp(&right.digest));
    let events = vec![
        event(1, json!({ "type": "sessionStarted" })),
        event(
            2,
            json!({
                "type": "observationCaptured",
                "observation": {
                    "id": "22222222-2222-4222-8222-222222222222",
                    "deviceId": "mock-1",
                    "capturedAtMs": 1_002,
                    "viewport": { "width": 1, "height": 1, "scaleFactor": 1.0 },
                    "screenshot": {
                        "id": format!("sha256:{png_digest}"),
                        "mediaType": "image/png",
                        "uri": format!("devicerail://assets/sha256/{png_digest}"),
                        "sha256": png_digest
                    },
                    "metadata": { "source": "report-test" }
                }
            }),
        ),
        event(
            3,
            json!({
                "type": "verdictRecorded",
                "verdict": {
                    "status": "pass",
                    "summary": "Static report complete",
                    "evidence": [{
                        "id": format!("sha256:{active_digest}"),
                        "mediaType": "text/html",
                        "uri": format!("devicerail://assets/sha256/{active_digest}"),
                        "sha256": active_digest
                    }]
                }
            }),
        ),
        event(
            4,
            json!({ "type": "sessionEnded", "outcome": "completed", "reason": "done" }),
        ),
    ];
    let manifest: BundleManifest = serde_json::from_value(json!({
        "magic": "devicerail.session-bundle",
        "bundleVersion": 1,
        "eventProtocolVersion": { "major": 1, "minor": 2 },
        "session": {
            "id": SESSION_ID,
            "state": "ended",
            "startedAtMs": 1_001,
            "endedAtMs": 1_004,
            "eventCount": 4,
            "lastSequence": 4
        },
        "events": events,
        "assets": assets.iter().map(|asset| json!({
            "sha256": asset.digest,
            "mediaType": asset.media_type,
            "byteLength": asset.bytes.len(),
            "path": format!("assets/sha256/{}", asset.digest)
        })).collect::<Vec<_>>()
    }))
    .expect("typed manifest");
    fs::write(
        root.join("manifest.json"),
        to_canonical_bytes(&manifest).expect("canonical manifest"),
    )
    .expect("manifest");
    fs::create_dir_all(root.join("assets/sha256")).expect("asset tree");
    for asset in assets {
        fs::write(root.join("assets/sha256").join(asset.digest), asset.bytes).expect("asset");
    }
    (root, png_digest, active_digest)
}

fn write_large_filtered_bundle(parent: &Path, cycles: u64) -> std::path::PathBuf {
    let root = parent.join("large-bundle");
    fs::create_dir(&root).expect("Bundle root");
    let mut events = vec![event(1, json!({ "type": "sessionStarted" }))];
    for index in 0..cycles {
        let call_id = format!("bbbbbbbb-bbbb-4bbb-8bbb-{:012x}", 0x1000 + index);
        for payload in [
            json!({
                "type": "observationCaptured",
                "observation": {
                    "id": format!("cccccccc-cccc-4ccc-8ccc-{:012x}", 0x2000 + index),
                    "deviceId": "mock-1",
                    "capturedAtMs": 2_000 + index,
                    "viewport": { "width": 1, "height": 1, "scaleFactor": 1.0 },
                    "screenshot": null,
                    "metadata": { "cycle": index }
                }
            }),
            json!({
                "type": "error",
                "error": { "code": format!("error-{index}"), "message": "failed", "retryable": false, "details": null }
            }),
            json!({
                "type": "verdictRecorded",
                "verdict": { "status": "unknown", "summary": format!("verdict-{index}"), "evidence": [] }
            }),
            json!({
                "type": "actionStarted",
                "call": { "id": call_id, "name": format!("action-{index}"), "arguments": {} }
            }),
            json!({
                "type": "actionCompleted",
                "callId": call_id,
                "outcome": {
                    "outcome": "failed",
                    "error": { "code": "action_failed", "message": "failed", "retryable": false, "details": null }
                }
            }),
        ] {
            events.push(event(events.len() as u64 + 1, payload));
        }
    }
    let last = events.len() as u64 + 1;
    events.push(event(
        last,
        json!({ "type": "sessionEnded", "outcome": "completed", "reason": "done" }),
    ));
    let manifest: BundleManifest = serde_json::from_value(json!({
        "magic": "devicerail.session-bundle",
        "bundleVersion": 1,
        "eventProtocolVersion": { "major": 1, "minor": 2 },
        "session": {
            "id": SESSION_ID,
            "state": "ended",
            "startedAtMs": 1_001,
            "endedAtMs": 1_000 + last,
            "eventCount": last,
            "lastSequence": last
        },
        "events": events,
        "assets": []
    }))
    .expect("typed large manifest");
    fs::write(
        root.join("manifest.json"),
        to_canonical_bytes(&manifest).expect("canonical large manifest"),
    )
    .expect("large manifest");
    root
}

fn rendered_sequences(html: &str) -> Vec<u64> {
    html.split("data-sequence=\"")
        .skip(1)
        .map(|tail| {
            tail.split('"')
                .next()
                .expect("sequence attribute value")
                .parse()
                .expect("numeric event sequence")
        })
        .collect()
}

#[tokio::test]
async fn exports_and_revalidates_a_relative_script_free_report() {
    let temporary = TempDir::new().expect("temporary");
    let (bundle, png_digest, active_digest) = write_bundle(temporary.path(), png());
    let report = temporary.path().join("report");
    let summary = export_static_report(
        &bundle,
        &report,
        ReportLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("export report");
    assert_eq!(summary.event_count, 4);
    assert_eq!(summary.page_count, 5);
    assert_eq!(summary.asset_count, 2);
    assert_eq!(
        validate_static_report(
            &report,
            ReportLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("validate report"),
        summary
    );

    let index = fs::read_to_string(report.join("index.html")).expect("index");
    assert!(index.contains("class=\"skip-link\""));
    assert!(index.contains("Content-Security-Policy"));
    assert!(index.contains(&format!("assets/sha256/{png_digest}.png")));
    assert!(index.contains(&format!("assets/sha256/{active_digest}.bin")));
    assert!(!index.contains("devicerail://"));
    assert!(!index.contains("<script>"));
    assert!(!index.contains(bundle.to_string_lossy().as_ref()));

    let bytes = fs::read(report.join("report.json")).expect("report manifest");
    let manifest: ReportManifest = from_canonical_slice(&bytes).expect("canonical report");
    assert_eq!(manifest.magic, REPORT_MAGIC);
    assert!(manifest.assets.iter().any(|asset| asset.previewable));
    assert!(
        manifest
            .assets
            .iter()
            .any(|asset| asset.media_type == "text/html" && asset.path.ends_with(".bin"))
    );
}

#[tokio::test]
async fn large_multi_filter_report_preserves_indexed_deep_page_order() {
    let temporary = TempDir::new().expect("temporary");
    let bundle = write_large_filtered_bundle(temporary.path(), 70);
    let report = temporary.path().join("large-report");
    let limits = ReportLimits {
        visualizer: VisualizerLimits {
            events_per_page: 17,
            ..VisualizerLimits::default()
        },
        ..ReportLimits::default()
    };
    let summary = export_static_report(&bundle, &report, limits, &ExecutionControl::unbounded())
        .await
        .expect("export indexed report");
    assert_eq!(summary.event_count, 352);
    assert_eq!(summary.page_count, 45);

    let manifest: ReportManifest = from_canonical_slice(
        &fs::read(report.join("report.json")).expect("read large report manifest"),
    )
    .expect("canonical large report manifest");
    assert_eq!(manifest.pages.len(), 45);
    let actions =
        fs::read_to_string(report.join("pages/actions-5.html")).expect("read deep actions page");
    let expected_actions = (68_u64..85)
        .map(|offset| 5 + (offset / 2) * 5 + offset % 2)
        .collect::<Vec<_>>();
    assert_eq!(rendered_sequences(&actions), expected_actions);
    let observations = fs::read_to_string(report.join("pages/observations-3.html"))
        .expect("read deep observations page");
    assert_eq!(
        rendered_sequences(&observations),
        (34_u64..51)
            .map(|offset| 2 + offset * 5)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        validate_static_report(&report, limits, &ExecutionControl::unbounded())
            .await
            .expect("validate indexed report"),
        summary
    );
}

#[tokio::test]
async fn target_conflicts_and_tampering_fail_without_clobbering() {
    let temporary = TempDir::new().expect("temporary");
    let (bundle, _, _) = write_bundle(temporary.path(), png());
    let existing = temporary.path().join("existing");
    fs::create_dir(&existing).expect("existing target");
    fs::write(existing.join("marker"), b"keep").expect("marker");
    assert!(matches!(
        export_static_report(
            &bundle,
            &existing,
            ReportLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(ReportError::TargetExists)
    ));
    assert_eq!(fs::read(existing.join("marker")).expect("marker"), b"keep");

    let report = temporary.path().join("report");
    export_static_report(
        &bundle,
        &report,
        ReportLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("export");
    let original_index = fs::read(report.join("index.html")).expect("original index");
    fs::write(report.join("index.html"), b"tampered").expect("tamper");
    assert!(matches!(
        validate_static_report(
            &report,
            ReportLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(ReportError::IntegrityMismatch)
    ));
    fs::write(report.join("index.html"), original_index).expect("restore index");
    fs::write(report.join("unexpected"), b"extra").expect("extra node");
    assert!(matches!(
        validate_static_report(
            &report,
            ReportLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(ReportError::InvalidTree)
    ));
}

#[tokio::test]
async fn coordinated_manifest_and_active_html_tampering_still_fails_closed() {
    let temporary = TempDir::new().expect("temporary");
    let (bundle, _, _) = write_bundle(temporary.path(), png());

    for (name, transform) in [
        (
            "script",
            Box::new(|html: String| html.replace("</body>", "<script>alert(1)</script></body>"))
                as Box<dyn FnOnce(String) -> String>,
        ),
        (
            "external-link",
            Box::new(|html: String| {
                html.replace(
                    "</body>",
                    "<a href=\"https://example.invalid/\">external</a></body>",
                )
            }),
        ),
        (
            "weakened-csp",
            Box::new(|html: String| html.replace("script-src 'none'", "script-src 'self'")),
        ),
    ] {
        let report = temporary.path().join(format!("report-{name}"));
        export_static_report(
            &bundle,
            &report,
            ReportLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("export report");
        let original = fs::read_to_string(report.join("index.html")).expect("report HTML");
        let modified = transform(original);
        coordinate_report_file(&report, "index.html", modified.as_bytes());
        assert!(matches!(
            validate_static_report(
                &report,
                ReportLimits::default(),
                &ExecutionControl::unbounded(),
            )
            .await,
            Err(ReportError::UnsafeHtml)
        ));
    }

    let report = temporary.path().join("report-css");
    export_static_report(
        &bundle,
        &report,
        ReportLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("export report");
    let mut css = fs::read(report.join("style.css")).expect("stylesheet");
    css.extend_from_slice(b"\n@import url(https://example.invalid/evil.css);\n");
    coordinate_report_file(&report, "style.css", &css);
    assert!(matches!(
        validate_static_report(
            &report,
            ReportLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(ReportError::InvalidManifest | ReportError::UnsafeStylesheet)
    ));
}

#[tokio::test]
async fn invalid_png_and_precancellation_publish_nothing() {
    let invalid = TempDir::new().expect("temporary");
    let (bundle, _, _) = write_bundle(invalid.path(), b"not a PNG".to_vec());
    let target = invalid.path().join("report");
    assert!(matches!(
        export_static_report(
            &bundle,
            &target,
            ReportLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(ReportError::UnsafePng)
    ));
    assert!(!target.exists());

    let inside_source = bundle.join("report");
    assert!(matches!(
        export_static_report(
            &bundle,
            &inside_source,
            ReportLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(ReportError::InvalidTarget)
    ));
    assert!(!inside_source.exists());

    let cancelled = TempDir::new().expect("temporary");
    let (bundle, _, _) = write_bundle(cancelled.path(), png());
    let target = cancelled.path().join("report");
    let (controller, control) = ExecutionController::new();
    controller.cancel(CancellationReason::Requested);
    assert!(matches!(
        export_static_report(&bundle, &target, ReportLimits::default(), &control).await,
        Err(ReportError::Cancelled {
            reason: CancellationReason::Requested
        })
    ));
    assert!(!target.exists());

    let timed = TempDir::new().expect("temporary");
    let (bundle, _, _) = write_bundle(timed.path(), png());
    let target = timed.path().join("report");
    let (_, control) = ExecutionController::with_timeout(0, TimeoutScope::Request);
    assert!(matches!(
        export_static_report(&bundle, &target, ReportLimits::default(), &control).await,
        Err(ReportError::TimedOut {
            scope: TimeoutScope::Request,
            timeout_ms: 0
        })
    ));
    assert!(!target.exists());
}
