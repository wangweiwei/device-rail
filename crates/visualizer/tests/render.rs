use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use devicerail_core::ExecutionControl;
use devicerail_session_bundle::{BundleManifest, to_canonical_bytes};
use devicerail_visualizer::{
    OfflineVisualizer, PageKind, PageQuery, STYLESHEET, VisualizerError, VisualizerLimits,
};
use serde_json::{Value, json};

const SESSION_ID: &str = "00000000-0000-0000-0000-000000000099";
const PNG_DIGEST: &str = "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881";
const JPEG_DIGEST: &str = "a1fce4363854ff888cff4b8e7875d600c2682390412a8cf79b37d0b11148b0fa";
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempBundle(PathBuf);

impl TempBundle {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempBundle {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct TestAsset {
    digest: &'static str,
    media_type: &'static str,
    bytes: &'static [u8],
}

fn temp_bundle() -> TempBundle {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "devicerail-visualizer-{}-{nonce}-{counter}",
        std::process::id()
    ));
    fs::create_dir(&root).expect("create test Bundle root");
    TempBundle(root)
}

fn event(sequence: u64, payload: Value) -> Value {
    json!({
        "eventId": format!("00000000-0000-0000-0000-{sequence:012x}"),
        "sessionId": SESSION_ID,
        "sequence": sequence,
        "atMs": 100 + sequence,
        "payload": payload
    })
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

fn write_bundle_with_protocol(
    middle_payloads: Vec<Value>,
    mut assets: Vec<TestAsset>,
    protocol_minor: u16,
) -> TempBundle {
    let root = temp_bundle();
    let mut events = vec![event(1, json!({ "type": "sessionStarted" }))];
    for (index, payload) in middle_payloads.into_iter().enumerate() {
        events.push(event(index as u64 + 2, payload));
    }
    let last = events.len() as u64 + 1;
    events.push(event(
        last,
        json!({ "type": "sessionEnded", "outcome": "completed", "reason": null }),
    ));

    assets.sort_by_key(|asset| asset.digest);
    let manifest_value = json!({
        "magic": "devicerail.session-bundle",
        "bundleVersion": 1,
        "eventProtocolVersion": { "major": 1, "minor": protocol_minor },
        "session": {
            "id": SESSION_ID,
            "state": "ended",
            "startedAtMs": 101,
            "endedAtMs": 100 + last,
            "eventCount": last,
            "lastSequence": last
        },
        "events": events,
        "assets": assets.iter().map(|asset| json!({
            "sha256": asset.digest,
            "mediaType": asset.media_type,
            "byteLength": asset.bytes.len(),
            "path": format!("assets/sha256/{}", asset.digest)
        })).collect::<Vec<_>>()
    });
    let manifest: BundleManifest =
        serde_json::from_value(manifest_value).expect("typed test manifest");
    fs::write(
        root.path().join("manifest.json"),
        to_canonical_bytes(&manifest).expect("canonical manifest"),
    )
    .expect("write manifest");
    if !assets.is_empty() {
        fs::create_dir_all(root.path().join("assets/sha256")).expect("create asset tree");
        for asset in assets {
            fs::write(
                root.path().join("assets/sha256").join(asset.digest),
                asset.bytes,
            )
            .expect("write asset");
        }
    }
    root
}

fn write_bundle(middle_payloads: Vec<Value>, assets: Vec<TestAsset>) -> TempBundle {
    write_bundle_with_protocol(middle_payloads, assets, 2)
}

async fn open(root: &TempBundle, limits: VisualizerLimits) -> OfflineVisualizer {
    OfflineVisualizer::open(root.path(), limits, &ExecutionControl::unbounded())
        .await
        .expect("open visualizer")
}

fn observation(
    id_suffix: u128,
    width: u32,
    height: u32,
    screenshot: Option<(&str, &str)>,
    omission: Option<&str>,
    metadata: Value,
) -> Value {
    let mut value = json!({
        "id": format!("00000000-0000-0000-0000-{id_suffix:012x}"),
        "deviceId": "mock-1",
        "capturedAtMs": 200,
        "viewport": { "width": width, "height": height, "scaleFactor": 1.0 },
        "screenshot": screenshot.map(|(digest, media_type)| json!({
            "id": format!("sha256:{digest}"),
            "mediaType": media_type,
            "uri": format!("devicerail://assets/sha256/{digest}"),
            "sha256": digest
        })),
        "metadata": metadata
    });
    if let Some(omission) = omission {
        value
            .as_object_mut()
            .expect("observation object")
            .insert("screenshotOmission".to_owned(), json!(omission));
    }
    value
}

#[tokio::test]
async fn escapes_and_truncates_all_untrusted_presentation_data() {
    let long = format!(
        "<script>boom</script>{}\u{0001}\u{202e}NEVER-RENDER",
        "z".repeat(200)
    );
    let metadata = json!({
        "__proto__": { "polluted": "<img src=x onerror=alert(1)>" },
        "constructor": { "prototype": { "x": long.clone() } }
    });
    let bundle = write_bundle(
        vec![
            json!({
                "type": "observationCaptured",
                "observation": observation(1, 10, 10, None, None, metadata)
            }),
            json!({
                "type": "error",
                "error": {
                    "code": "bad<code>\u{0001}",
                    "message": long,
                    "retryable": false,
                    "details": { "__proto__": "<svg/onload=alert(1)>" }
                }
            }),
            json!({
                "type": "verdictRecorded",
                "verdict": { "status": "fail", "summary": "</p><script>x</script>", "evidence": [] }
            }),
        ],
        vec![],
    );
    let limits = VisualizerLimits {
        max_text_chars: 32,
        max_json_bytes: 80,
        ..VisualizerLimits::default()
    };
    let viewer = open(&bundle, limits).await;
    let html = viewer
        .render_page(PageQuery::default(), "/offline/token")
        .expect("render");

    assert!(!html.contains("<script>"));
    assert!(!html.contains("<img src=x"));
    assert!(!html.contains("NEVER-RENDER"));
    assert!(!html.contains('\u{0001}'));
    assert!(!html.contains('\u{202e}'));
    assert!(html.contains('\u{fffd}'));
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("__proto__"));
    assert!(html.contains("[truncated]"));
    assert!(!html.contains("http://"));
    assert!(!html.contains("https://"));
    assert!(!html.contains("src=\"//"));
    assert!(!html.contains("<script"));
    assert!(!html.contains("<style"));
    assert!(html.contains("Content-Security-Policy"));
    assert!(html.contains("class=\"skip-link\""));
    assert!(html.contains("Unsigned Bundle"));
    assert!(STYLESHEET.contains(":focus-visible"));
}

#[tokio::test]
async fn timeline_is_sequence_ordered_and_server_paginated_at_fifty() {
    let payloads = (0..118)
        .map(|index| {
            json!({
                "type": "verdictRecorded",
                "verdict": { "status": "unknown", "summary": format!("v-{index}"), "evidence": [] }
            })
        })
        .collect();
    let bundle = write_bundle(payloads, vec![]);
    let viewer = open(&bundle, VisualizerLimits::default()).await;

    let page_two = viewer
        .render_page(PageQuery::new(PageKind::All, 2), "/v/cap")
        .expect("page two");
    assert_eq!(page_two.matches("data-sequence=").count(), 50);
    assert!(page_two.contains("data-sequence=\"51\""));
    assert!(page_two.contains("data-sequence=\"100\""));
    assert!(!page_two.contains("data-sequence=\"50\""));
    assert!(page_two.find("data-sequence=\"51\"") < page_two.find("data-sequence=\"100\""));

    let page_three = viewer
        .render_page(PageQuery::new(PageKind::All, 3), "/v/cap")
        .expect("page three");
    assert_eq!(page_three.matches("data-sequence=").count(), 20);
    assert!(matches!(
        viewer.render_page(PageQuery::new(PageKind::All, 0), "/v/cap"),
        Err(VisualizerError::InvalidPage)
    ));
    assert!(matches!(
        viewer.render_page(PageQuery::new(PageKind::All, 4), "/v/cap"),
        Err(VisualizerError::PageOutOfRange { .. })
    ));
}

#[tokio::test]
async fn indexed_filters_preserve_large_timeline_order_across_dynamic_and_static_pages() {
    const CYCLES: u64 = 70;
    let mut payloads = Vec::with_capacity(CYCLES as usize * 5);
    for index in 0..CYCLES {
        let call_id = format!("00000000-0000-0000-0000-{:012x}", 0x1000 + index);
        payloads.extend([
            json!({
                "type": "observationCaptured",
                "observation": observation(0x2000 + index as u128, 10, 10, None, None, json!({ "cycle": index }))
            }),
            json!({ "type": "error", "error": error(&format!("error-{index}")) }),
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
                "outcome": { "outcome": "failed", "error": error(&format!("action-{index}")) }
            }),
        ]);
    }
    let bundle = write_bundle(payloads, vec![]);
    let limits = VisualizerLimits {
        events_per_page: 17,
        ..VisualizerLimits::default()
    };
    let viewer = open(&bundle, limits).await;

    assert_eq!(viewer.page_count(PageKind::All), 21);
    assert_eq!(viewer.page_count(PageKind::Observations), 5);
    assert_eq!(viewer.page_count(PageKind::Actions), 9);
    assert_eq!(viewer.page_count(PageKind::Errors), 5);
    assert_eq!(viewer.page_count(PageKind::Verdicts), 5);

    let action_sequences = (68_u64..85)
        .map(|offset| 5 + (offset / 2) * 5 + offset % 2)
        .collect::<Vec<_>>();
    let cases = [
        (
            PageQuery::new(PageKind::All, 11),
            (171_u64..=187).collect::<Vec<_>>(),
        ),
        (
            PageQuery::new(PageKind::Observations, 3),
            (34_u64..51).map(|offset| 2 + offset * 5).collect(),
        ),
        (PageQuery::new(PageKind::Actions, 5), action_sequences),
        (
            PageQuery::new(PageKind::Errors, 4),
            (51_u64..68).map(|offset| 3 + offset * 5).collect(),
        ),
        (
            PageQuery::new(PageKind::Verdicts, 2),
            (17_u64..34).map(|offset| 4 + offset * 5).collect(),
        ),
    ];

    for (query, expected) in cases {
        let dynamic = viewer
            .render_page(query, "/offline/indexed")
            .expect("render indexed dynamic page");
        let static_page = viewer
            .render_static_page(query, &BTreeSet::new())
            .expect("render indexed static page");
        assert_eq!(rendered_sequences(&dynamic), expected);
        assert_eq!(rendered_sequences(&static_page), expected);
    }
}

#[tokio::test]
async fn observations_filter_contains_media_frames_but_not_stream_boundaries() {
    let stream_id = "00000000-0000-4000-8000-000000000025";
    let bundle = write_bundle_with_protocol(
        vec![
            json!({
                "type": "mediaStreamStarted",
                "stream": {
                    "id": stream_id,
                    "kind": "screenshot",
                    "mediaType": "image/png"
                }
            }),
            json!({
                "type": "mediaFrameCaptured",
                "frame": {
                    "streamId": stream_id,
                    "frameIndex": 1,
                    "keyFrame": true,
                    "evidence": {
                        "id": format!("sha256:{PNG_DIGEST}"),
                        "mediaType": "image/png",
                        "uri": format!("devicerail://assets/sha256/{PNG_DIGEST}"),
                        "sha256": PNG_DIGEST
                    }
                }
            }),
            json!({
                "type": "mediaStreamEnded",
                "streamId": stream_id,
                "frameCount": 1
            }),
        ],
        vec![TestAsset {
            digest: PNG_DIGEST,
            media_type: "image/png",
            bytes: b"x",
        }],
        4,
    );
    let viewer = open(&bundle, VisualizerLimits::default()).await;

    let all = viewer
        .render_page(PageQuery::new(PageKind::All, 1), "/offline/media-filter")
        .expect("render all media lifecycle events");
    assert_eq!(rendered_sequences(&all), vec![1, 2, 3, 4, 5]);

    for filtered in [
        viewer
            .render_page(
                PageQuery::new(PageKind::Observations, 1),
                "/offline/media-filter",
            )
            .expect("render dynamic observations"),
        viewer
            .render_static_page(PageQuery::new(PageKind::Observations, 1), &BTreeSet::new())
            .expect("render static observations"),
    ] {
        assert_eq!(rendered_sequences(&filtered), vec![3]);
        assert!(filtered.contains("Media frame captured"));
        assert!(!filtered.contains("Media stream started"));
        assert!(!filtered.contains("Media stream ended"));
    }
}

fn error(code: &str) -> Value {
    json!({ "code": code, "message": format!("{code} message"), "retryable": false, "details": null })
}

#[tokio::test]
async fn renders_all_action_terminals_and_preserves_protected_omission() {
    let ids = [
        "00000000-0000-0000-0000-000000000101",
        "00000000-0000-0000-0000-000000000102",
        "00000000-0000-0000-0000-000000000103",
        "00000000-0000-0000-0000-000000000104",
    ];
    let secret = "DO-NOT-RENDER-SECRET";
    let mut payloads = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        payloads.push(json!({
            "type": "actionStarted",
            "call": {
                "id": id,
                "name": format!("action-{index}"),
                "arguments": if index == 0 { Value::Null } else { json!({ "index": index }) },
                "argumentsRedacted": index == 0
            }
        }));
    }
    let protected_observation = observation(10, 10, 20, None, Some("protectedAction"), json!({}));
    payloads.extend([
        json!({
            "type": "actionCompleted", "callId": ids[0],
            "outcome": { "outcome": "succeeded", "result": {
                "callId": ids[0], "startedAtMs": 1, "finishedAtMs": 2,
                "output": { "ok": true }, "before": protected_observation,
                "after": observation(11, 10, 20, None, Some("protectedAction"), json!({})),
                "evidence": []
            }}
        }),
        json!({ "type": "actionCompleted", "callId": ids[1], "outcome": { "outcome": "failed", "error": error("failed") } }),
        json!({ "type": "actionCompleted", "callId": ids[2], "outcome": { "outcome": "cancelled", "error": error("cancelled") } }),
        json!({ "type": "actionCompleted", "callId": ids[3], "outcome": { "outcome": "timedOut", "error": error("timeout"), "timeoutMs": 500 } }),
    ]);
    let bundle = write_bundle(payloads, vec![]);
    let viewer = open(&bundle, VisualizerLimits::default()).await;
    assert!(!format!("{viewer:?}").contains(bundle.path().to_string_lossy().as_ref()));
    let html = viewer
        .render_page(PageQuery::new(PageKind::Actions, 1), "/offline/cap")
        .expect("render actions");

    assert!(html.contains(">Succeeded<"));
    assert!(html.contains(">Failed<"));
    assert!(html.contains(">Cancelled<"));
    assert!(html.contains(">Timed out<"));
    assert!(html.contains("arguments were deliberately omitted"));
    assert_eq!(
        html.matches("Screenshot omitted:</strong> protected action")
            .count(),
        2
    );
    assert!(!html.contains(secret));
}

#[tokio::test]
async fn displays_javascript_safe_integer_boundaries_exactly() {
    const MAX_SAFE: u64 = 9_007_199_254_740_991;
    let call_id = "00000000-0000-0000-0000-000000000201";
    let bundle = write_bundle(
        vec![
            json!({
                "type": "actionStarted",
                "call": { "id": call_id, "name": "boundary", "arguments": {} }
            }),
            json!({
                "type": "actionCompleted",
                "callId": call_id,
                "outcome": {
                    "outcome": "timedOut",
                    "error": error("boundary_timeout"),
                    "timeoutMs": MAX_SAFE
                }
            }),
        ],
        vec![],
    );
    let viewer = open(&bundle, VisualizerLimits::default()).await;
    let html = viewer
        .render_page(PageQuery::new(PageKind::Actions, 1), "/offline/token")
        .expect("safe integer boundary");

    assert!(html.contains("9007199254740991 ms"));
    assert!(!html.contains("9.007199"));
}

#[tokio::test]
async fn image_preview_requires_exact_png_and_uses_only_digest_capabilities() {
    let bundle = write_bundle(
        vec![
            json!({ "type": "observationCaptured", "observation": observation(20, u32::MAX, 0, Some((PNG_DIGEST, "image/png")), None, json!({})) }),
            json!({ "type": "observationCaptured", "observation": observation(21, 1, 8_192, Some((JPEG_DIGEST, "image/jpeg")), None, json!({})) }),
        ],
        vec![
            TestAsset {
                digest: PNG_DIGEST,
                media_type: "image/png",
                bytes: b"x",
            },
            TestAsset {
                digest: JPEG_DIGEST,
                media_type: "image/jpeg",
                bytes: b"y",
            },
        ],
    );
    let viewer = open(&bundle, VisualizerLimits::default()).await;
    let html = viewer
        .render_page(PageQuery::new(PageKind::Observations, 1), "/offline/token")
        .expect("render observations");

    assert_eq!(html.matches("<img ").count(), 1);
    assert!(html.contains(&format!("/offline/token/image/{PNG_DIGEST}")));
    assert!(!html.contains(&format!("/offline/token/image/{JPEG_DIGEST}")));
    assert!(html.contains(&format!("/offline/token/download/{PNG_DIGEST}")));
    assert!(html.contains(&format!("/offline/token/download/{JPEG_DIGEST}")));
    assert!(html.contains("width=\"960\" height=\"540\""));
    assert!(html.contains("class=\"evidence-preview\""));
    assert!(html.contains("Viewer rejected it during request-time integrity or safety validation"));
    assert!(STYLESHEET.contains("height: clamp(12rem, 50vw, 40rem)"));
    assert!(STYLESHEET.contains("object-fit: contain"));
    assert!(STYLESHEET.contains("background: var(--status-bg)"));
    assert!(STYLESHEET.contains("--protected: #deb7ff"));
    assert!(STYLESHEET.contains("outline: 3px solid var(--focus)"));
    assert!(!html.contains("devicerail://"));
    assert!(!html.contains("assets/sha256"));
    assert!(!html.contains(bundle.path().to_string_lossy().as_ref()));
    assert_eq!(
        viewer
            .asset_by_digest(PNG_DIGEST)
            .map(|asset| asset.media_type.as_str()),
        Some("image/png")
    );
    assert!(viewer.asset_by_digest("../manifest.json").is_none());
}

#[tokio::test]
async fn static_pages_use_only_bounded_relative_report_links() {
    let bundle = write_bundle(
        vec![json!({
            "type": "observationCaptured",
            "observation": observation(
                30,
                10,
                10,
                Some((PNG_DIGEST, "image/png")),
                None,
                json!({})
            )
        })],
        vec![TestAsset {
            digest: PNG_DIGEST,
            media_type: "image/png",
            bytes: b"x",
        }],
    );
    let viewer = open(&bundle, VisualizerLimits::default()).await;
    let previewable = BTreeSet::from([PNG_DIGEST.to_owned()]);

    let index = viewer
        .render_static_page(PageQuery::default(), &previewable)
        .expect("static index");
    assert!(index.contains("href=\"style.css\""));
    assert!(index.contains("href=\"pages/observations-1.html\""));
    assert!(index.contains(&format!("src=\"assets/sha256/{PNG_DIGEST}.png\"")));
    assert!(!index.contains("/image/"));
    assert!(!index.contains("/download/"));

    let nested = viewer
        .render_static_page(PageQuery::new(PageKind::Observations, 1), &previewable)
        .expect("static nested page");
    assert!(nested.contains("href=\"../style.css\""));
    assert!(nested.contains("href=\"../index.html\""));
    assert!(nested.contains(&format!("src=\"../assets/sha256/{PNG_DIGEST}.png\"")));
}

#[tokio::test]
async fn bounds_large_evidence_lists_inside_one_event() {
    let reference = json!({
        "id": format!("sha256:{JPEG_DIGEST}"),
        "mediaType": "image/jpeg",
        "uri": format!("devicerail://assets/sha256/{JPEG_DIGEST}"),
        "sha256": JPEG_DIGEST
    });
    let bundle = write_bundle(
        vec![json!({
            "type": "verdictRecorded",
            "verdict": {
                "status": "unknown",
                "summary": "bounded evidence",
                "evidence": vec![reference; 75]
            }
        })],
        vec![TestAsset {
            digest: JPEG_DIGEST,
            media_type: "image/jpeg",
            bytes: b"y",
        }],
    );
    let viewer = open(&bundle, VisualizerLimits::default()).await;
    let html = viewer
        .render_page(PageQuery::new(PageKind::Verdicts, 1), "/offline/token")
        .expect("bounded evidence list");

    assert_eq!(
        html.matches(&format!("/offline/token/download/{JPEG_DIGEST}"))
            .count(),
        50
    );
    assert!(html.contains("25 additional Evidence references omitted"));
}

#[tokio::test]
async fn stops_html_construction_at_the_configured_byte_budget() {
    let bundle = write_bundle(
        vec![json!({
            "type": "error",
            "error": {
                "code": "oversized_page",
                "message": "<&>\"'".repeat(1_000),
                "retryable": false,
                "details": null
            }
        })],
        vec![],
    );
    let limits = VisualizerLimits {
        max_html_bytes: 1_024,
        ..VisualizerLimits::default()
    };
    let viewer = open(&bundle, limits).await;
    assert!(matches!(
        viewer.render_page(PageQuery::default(), "/offline/token"),
        Err(VisualizerError::RenderLimitExceeded)
    ));
}

#[tokio::test]
async fn empty_filters_are_semantic_and_capability_paths_fail_closed() {
    let bundle = write_bundle(vec![], vec![]);
    let viewer = open(&bundle, VisualizerLimits::default()).await;
    let html = viewer
        .render_page(PageQuery::new(PageKind::Errors, 1), "/offline/token")
        .expect("empty error filter");
    assert!(html.contains("<section class=\"empty-state\">"));
    assert!(html.contains("No matching events"));

    for invalid in [
        "https://example.invalid/cap",
        "//example.invalid/cap",
        "/offline/../secret",
        "/offline/token?x=1",
        "/offline/token/",
    ] {
        assert!(matches!(
            viewer.render_page(PageQuery::default(), invalid),
            Err(VisualizerError::InvalidCapabilityBase)
        ));
    }
}
