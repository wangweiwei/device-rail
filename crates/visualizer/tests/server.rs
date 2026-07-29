use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use devicerail_core::ExecutionControl;
use devicerail_session_bundle::{BundleManifest, to_canonical_bytes};
use devicerail_visualizer::{
    OfflineVisualizer, ServerError, ServerLimits, ViewerServer, VisualizerLimits,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time,
};

const SESSION_ID: &str = "00000000-0000-0000-0000-000000000299";
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
    digest: String,
    media_type: &'static str,
    bytes: Vec<u8>,
}

impl TestAsset {
    fn new(media_type: &'static str, bytes: Vec<u8>) -> Self {
        let digest = hex::encode(Sha256::digest(&bytes));
        Self {
            digest,
            media_type,
            bytes,
        }
    }
}

fn temp_bundle() -> TempBundle {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "devicerail-visualizer-server-{}-{nonce}-{counter}",
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
        "atMs": 1_000 + sequence,
        "payload": payload
    })
}

fn asset_ref(asset: &TestAsset) -> Value {
    json!({
        "id": format!("sha256:{}", asset.digest),
        "mediaType": asset.media_type,
        "uri": format!("devicerail://assets/sha256/{}", asset.digest),
        "sha256": asset.digest
    })
}

fn write_bundle(middle_payloads: Vec<Value>, mut assets: Vec<TestAsset>) -> TempBundle {
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
    assets.sort_by(|left, right| left.digest.cmp(&right.digest));

    let value = json!({
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
        "assets": assets.iter().map(|asset| json!({
            "sha256": asset.digest,
            "mediaType": asset.media_type,
            "byteLength": asset.bytes.len(),
            "path": format!("assets/sha256/{}", asset.digest)
        })).collect::<Vec<_>>()
    });
    let manifest: BundleManifest = serde_json::from_value(value).expect("typed manifest");
    fs::write(
        root.path().join("manifest.json"),
        to_canonical_bytes(&manifest).expect("canonical manifest"),
    )
    .expect("write manifest");
    if !assets.is_empty() {
        fs::create_dir_all(root.path().join("assets/sha256")).expect("asset directory");
        for asset in assets {
            fs::write(
                root.path().join("assets/sha256").join(&asset.digest),
                asset.bytes,
            )
            .expect("asset bytes");
        }
    }
    root
}

fn observation(asset: &TestAsset) -> Value {
    json!({
        "type": "observationCaptured",
        "observation": {
            "id": "00000000-0000-0000-0000-000000000301",
            "deviceId": "mock-1",
            "capturedAtMs": 1_002,
            "viewport": { "width": 2, "height": 2, "scaleFactor": 1.0 },
            "screenshot": asset_ref(asset),
            "metadata": {}
        }
    })
}

fn verdict(asset: &TestAsset) -> Value {
    json!({
        "type": "verdictRecorded",
        "verdict": {
            "status": "unknown",
            "summary": "attachment",
            "evidence": [asset_ref(asset)]
        }
    })
}

fn tiny_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer
            .write_image_data(&[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ])
            .expect("PNG pixels");
        writer.finish().expect("PNG finish");
    }
    bytes
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn dimension_bomb() -> Vec<u8> {
    let mut bytes = tiny_png();
    bytes[16..20].copy_from_slice(&8_193_u32.to_be_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_be_bytes());
    let crc = crc32(&bytes[12..29]);
    bytes[29..33].copy_from_slice(&crc.to_be_bytes());
    bytes
}

async fn start(bundle: &TempBundle, mut limits: ServerLimits) -> ViewerServer {
    limits.port = 0;
    let viewer = OfflineVisualizer::open(
        bundle.path(),
        VisualizerLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("validated Viewer");
    ViewerServer::bind(viewer, limits)
        .await
        .expect("loopback Viewer")
}

async fn raw_request(server: &ViewerServer, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(server.local_addr())
        .await
        .expect("connect Viewer");
    stream.write_all(request).await.expect("write request");
    let mut response = Vec::new();
    if let Err(error) = stream.read_to_end(&mut response).await {
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::ConnectionReset,
            "read response"
        );
    }
    response
}

fn request(server: &ViewerServer, method: &str, target: &str) -> Vec<u8> {
    format!(
        "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        server.local_addr().port()
    )
    .into_bytes()
}

fn status(response: &[u8]) -> u16 {
    let line_end = response
        .windows(2)
        .position(|window| window == b"\r\n")
        .expect("status line");
    std::str::from_utf8(&response[..line_end])
        .expect("ASCII status")
        .split(' ')
        .nth(1)
        .expect("status code")
        .parse()
        .expect("numeric status")
}

fn response_text(response: &[u8]) -> &str {
    std::str::from_utf8(response).expect("text response")
}

fn response_body(response: &[u8]) -> &[u8] {
    let index = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response headers");
    &response[index + 4..]
}

#[tokio::test]
async fn serves_script_free_pages_and_css_on_rotating_loopback_capabilities() {
    let bundle = write_bundle(vec![], vec![]);
    let mut first = start(&bundle, ServerLimits::default()).await;
    let mut second = start(&bundle, ServerLimits::default()).await;

    assert!(first.local_addr().ip().is_ipv4());
    assert!(first.local_addr().ip().is_loopback());
    assert_ne!(first.base_path(), second.base_path());
    assert_eq!(first.base_path().len(), 3 + 64);
    assert!(
        first.base_path()[3..]
            .bytes()
            .all(|byte| { byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) })
    );

    let page_request = request(&first, "GET", first.base_path());
    let page = raw_request(&first, &page_request).await;
    let page_text = response_text(&page);
    assert_eq!(status(&page), 200);
    assert!(page_text.contains("Content-Type: text/html; charset=utf-8"));
    assert!(page_text.contains("Content-Security-Policy: default-src 'none'"));
    assert!(page_text.contains("script-src 'none'"));
    assert!(page_text.contains("connect-src 'none'"));
    assert!(page_text.contains("X-Content-Type-Options: nosniff"));
    assert!(page_text.contains("Cross-Origin-Resource-Policy: same-origin"));
    assert!(page_text.contains("Referrer-Policy: no-referrer"));
    assert!(page_text.contains("Unsigned Bundle"));
    assert!(!page_text.contains("Access-Control-Allow-Origin"));
    assert!(!page_text.contains("Set-Cookie"));
    assert!(!response_text(response_body(&page)).contains("<script"));
    assert!(!response_text(response_body(&page)).contains("http://"));
    assert!(!response_text(response_body(&page)).contains("https://"));

    let style_path = format!("{}/style.css", first.base_path());
    let style_request = request(&first, "GET", &style_path);
    let style = raw_request(&first, &style_request).await;
    assert_eq!(status(&style), 200);
    assert!(response_text(&style).contains("Content-Type: text/css; charset=utf-8"));
    let css = response_text(response_body(&style));
    assert!(!css.contains("@import"));
    assert!(!css.contains("url("));
    assert!(!css.contains("http://"));
    assert!(!css.contains("https://"));

    first.shutdown().await.expect("first shutdown");
    first.shutdown().await.expect("idempotent shutdown");
    assert!(TcpStream::connect(first.local_addr()).await.is_err());
    second.shutdown().await.expect("second shutdown");
}

#[tokio::test]
async fn rejects_wrong_authority_methods_queries_and_http_smuggling_shapes() {
    let bundle = write_bundle(vec![], vec![]);
    let limits = ServerLimits {
        max_header_bytes: 512,
        request_timeout_ms: 250,
        ..ServerLimits::default()
    };
    let mut server = start(&bundle, limits).await;

    let wrong_host = format!(
        "GET {} HTTP/1.1\r\nHost: localhost:{}\r\n\r\n",
        server.base_path(),
        server.local_addr().port()
    );
    assert_eq!(
        status(&raw_request(&server, wrong_host.as_bytes()).await),
        404
    );

    let wrong_token = "/v/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for method in ["GET", "POST"] {
        let request = request(&server, method, wrong_token);
        assert_eq!(status(&raw_request(&server, &request).await), 404);
    }
    for method in ["POST", "HEAD", "OPTIONS", "TRACE", "CONNECT"] {
        let request = request(&server, method, server.base_path());
        let response = raw_request(&server, &request).await;
        assert_eq!(status(&response), 405);
        assert!(response_text(&response).contains("Allow: GET"));
        assert!(!response_text(&response).contains("Access-Control-Allow-Origin"));
    }

    let valid_query = format!("{}?kind=all&page=1", server.base_path());
    let valid_request = request(&server, "GET", &valid_query);
    assert_eq!(status(&raw_request(&server, &valid_request).await), 200);
    for query in [
        "page=0",
        "page=01",
        "page=-1",
        "page=1&page=1",
        "unknown=1",
        "kind=other",
        "page=184467440737095516160",
    ] {
        let target = format!("{}?{query}", server.base_path());
        let request = request(&server, "GET", &target);
        assert_eq!(
            status(&raw_request(&server, &request).await),
            400,
            "{query}"
        );
    }
    let page_two = format!("{}?page=2", server.base_path());
    let page_two_request = request(&server, "GET", &page_two);
    assert_eq!(status(&raw_request(&server, &page_two_request).await), 404);
    for malformed_target in [
        format!("{}//style.css", server.base_path()),
        format!("{}/../style.css", server.base_path()),
        format!("{}%2fstyle.css", server.base_path()),
        format!("{}\\style.css", server.base_path()),
    ] {
        let malformed_request = request(&server, "GET", &malformed_target);
        assert_eq!(
            status(&raw_request(&server, &malformed_request).await),
            400,
            "{malformed_target}"
        );
    }

    let duplicate_host = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nHost: 127.0.0.1:{}\r\n\r\n",
        server.base_path(),
        server.local_addr().port(),
        server.local_addr().port()
    );
    assert_eq!(
        status(&raw_request(&server, duplicate_host.as_bytes()).await),
        400
    );
    for header in [
        "Content-Length: 0",
        "Transfer-Encoding: chunked",
        "Upgrade: websocket",
    ] {
        let malformed = format!(
            "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{header}\r\n\r\n",
            server.base_path(),
            server.local_addr().port()
        );
        assert_eq!(
            status(&raw_request(&server, malformed.as_bytes()).await),
            400
        );
    }
    let absolute = format!(
        "GET http://127.0.0.1:{}{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        server.local_addr().port(),
        server.base_path(),
        server.local_addr().port()
    );
    assert_eq!(
        status(&raw_request(&server, absolute.as_bytes()).await),
        400
    );
    let pipeline = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\nGET / HTTP/1.1\r\n\r\n",
        server.base_path(),
        server.local_addr().port()
    );
    assert_eq!(
        status(&raw_request(&server, pipeline.as_bytes()).await),
        400
    );
    let oversized = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Fill: {}\r\n\r\n",
        server.base_path(),
        server.local_addr().port(),
        "x".repeat(600)
    );
    let oversized_response = raw_request(&server, oversized.as_bytes()).await;
    assert!(oversized_response.is_empty() || status(&oversized_response) == 431);

    server.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn rehashes_assets_rejects_active_inline_content_and_validates_complete_pngs() {
    let png = TestAsset::new("image/png", tiny_png());
    let html = TestAsset::new(
        "text/html",
        b"<script>top.location='https://evil.invalid'</script>".to_vec(),
    );
    let bundle = write_bundle(
        vec![observation(&png), verdict(&html)],
        vec![png.clone(), html.clone()],
    );
    let mut server = start(&bundle, ServerLimits::default()).await;

    let image_path = format!("{}/image/{}", server.base_path(), png.digest);
    let image_request = request(&server, "GET", &image_path);
    let image = raw_request(&server, &image_request).await;
    assert_eq!(status(&image), 200);
    assert!(
        response_text(&image[..image.len() - png.bytes.len()]).contains("Content-Type: image/png")
    );
    assert_eq!(response_body(&image), png.bytes);

    let active_inline = format!("{}/image/{}", server.base_path(), html.digest);
    let active_request = request(&server, "GET", &active_inline);
    assert_eq!(status(&raw_request(&server, &active_request).await), 404);
    let download_path = format!("{}/download/{}", server.base_path(), html.digest);
    let download_request = request(&server, "GET", &download_path);
    let download = raw_request(&server, &download_request).await;
    assert_eq!(status(&download), 200);
    assert!(
        response_text(&download[..download.len() - html.bytes.len()])
            .contains("Content-Type: application/octet-stream")
    );
    assert!(
        response_text(&download[..download.len() - html.bytes.len()])
            .contains("Content-Disposition: attachment;")
    );
    assert_eq!(response_body(&download), html.bytes);

    let asset_path = bundle.path().join("assets/sha256").join(&png.digest);
    let mut tampered = png.bytes.clone();
    tampered[40] ^= 0x01;
    fs::write(&asset_path, tampered).expect("same-size tamper");
    assert_eq!(status(&raw_request(&server, &image_request).await), 422);
    fs::write(&asset_path, &png.bytes).expect("restore asset");
    assert_eq!(status(&raw_request(&server, &image_request).await), 200);
    fs::write(&asset_path, &png.bytes[..png.bytes.len() - 1]).expect("truncate asset");
    assert_eq!(status(&raw_request(&server, &image_request).await), 422);
    server.shutdown().await.expect("shutdown");

    let fake = TestAsset::new("image/png", b"not a PNG despite its media type".to_vec());
    let fake_bundle = write_bundle(vec![observation(&fake)], vec![fake.clone()]);
    let mut fake_server = start(&fake_bundle, ServerLimits::default()).await;
    let fake_path = format!("{}/image/{}", fake_server.base_path(), fake.digest);
    let fake_request = request(&fake_server, "GET", &fake_path);
    assert_eq!(status(&raw_request(&fake_server, &fake_request).await), 422);
    fake_server.shutdown().await.expect("fake shutdown");

    let bomb = TestAsset::new("image/png", dimension_bomb());
    let bomb_bundle = write_bundle(vec![observation(&bomb)], vec![bomb.clone()]);
    let mut bomb_server = start(&bomb_bundle, ServerLimits::default()).await;
    let bomb_path = format!("{}/image/{}", bomb_server.base_path(), bomb.digest);
    let bomb_request = request(&bomb_server, "GET", &bomb_path);
    assert_eq!(status(&raw_request(&bomb_server, &bomb_request).await), 413);
    bomb_server.shutdown().await.expect("bomb shutdown");
}

#[tokio::test]
async fn shows_the_checked_in_protected_omission_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../session-bundle/tests/fixtures/protected-omission");
    let viewer = OfflineVisualizer::open(
        root,
        VisualizerLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("protected fixture");
    let mut server = ViewerServer::bind(viewer, ServerLimits::default())
        .await
        .expect("protected server");
    let page_request = request(&server, "GET", server.base_path());
    let page = raw_request(&server, &page_request).await;
    assert_eq!(status(&page), 200);
    let body = response_text(response_body(&page));
    assert!(body.contains("Protected action"));
    assert!(body.contains("Screenshot omitted:</strong> protected action"));
    assert!(body.contains(">protected-action</bdi>"));
    assert!(!body.contains("String(&quot;"));
    assert!(!body.contains("<img "));
    server.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn shutdown_interrupts_an_idle_connection_within_the_grace_period() {
    let bundle = write_bundle(vec![], vec![]);
    let limits = ServerLimits {
        request_timeout_ms: 5_000,
        shutdown_grace_ms: 500,
        ..ServerLimits::default()
    };
    let mut server = start(&bundle, limits).await;
    let idle = TcpStream::connect(server.local_addr())
        .await
        .expect("idle connection");
    time::timeout(Duration::from_millis(750), server.shutdown())
        .await
        .expect("bounded shutdown")
        .expect("clean shutdown");
    drop(idle);
    assert!(TcpStream::connect(server.local_addr()).await.is_err());
}

#[tokio::test]
async fn rejects_limits_that_would_widen_the_server_attack_surface() {
    let bundle = write_bundle(vec![], vec![]);
    let viewer = OfflineVisualizer::open(
        bundle.path(),
        VisualizerLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("validated Viewer");
    let limits = ServerLimits {
        max_render_requests: 3,
        ..ServerLimits::default()
    };
    assert!(matches!(
        ViewerServer::bind(viewer, limits).await,
        Err(ServerError::InvalidLimits)
    ));
}

#[tokio::test]
async fn applies_the_server_html_budget_during_rendering() {
    let bundle = write_bundle(vec![], vec![]);
    let limits = ServerLimits {
        max_html_bytes: 1_024,
        ..ServerLimits::default()
    };
    let mut server = start(&bundle, limits).await;
    let page_request = request(&server, "GET", server.base_path());
    assert_eq!(status(&raw_request(&server, &page_request).await), 413);
    server.shutdown().await.expect("shutdown");
}
