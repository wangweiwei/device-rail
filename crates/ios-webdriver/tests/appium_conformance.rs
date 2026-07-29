use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceRuntime, DriverError, DriverResult, EvidenceInput, EvidenceMetadata,
    EvidenceOutput, EvidenceResult, EvidenceStore, ExecutionControl, GcPolicy, GcReport,
    MemoryEventStore, OperationContext, PutEvidence, ReleaseReport, SessionEventStore,
    Sha256Digest, StartSession, StoredEvidence, now_ms,
};
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_ios_webdriver::{
    AppiumButton, AppiumContext, AppiumDrag, AppiumElement, AppiumIosDriver, AppiumLocatorStrategy,
    AppiumSession, AppiumSessionRequest, AppiumStatus, AppiumTransport, IosDeviceConfig,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionExecution, AssetRef, SessionId, UiContextKind, UiRect,
    Viewport,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

struct FakeAppium {
    connected: Mutex<bool>,
    context: Mutex<AppiumContext>,
    element_operations: Mutex<Vec<String>>,
    screenshot_operations: Mutex<Vec<&'static str>>,
}

impl FakeAppium {
    fn new() -> Self {
        Self {
            connected: Mutex::new(false),
            context: Mutex::new(AppiumContext::native()),
            element_operations: Mutex::new(Vec::new()),
            screenshot_operations: Mutex::new(Vec::new()),
        }
    }

    fn require_session(&self) -> DriverResult<()> {
        if *self.connected.lock().expect("connection lock") {
            Ok(())
        } else {
            Err(DriverError::Platform {
                code: "appium_invalid_session".to_owned(),
                retryable: false,
            })
        }
    }

    fn record(&self, operation: impl Into<String>) {
        self.element_operations
            .lock()
            .expect("operation lock")
            .push(operation.into());
    }
}

#[async_trait]
impl AppiumTransport for FakeAppium {
    async fn status(&self, _: &ExecutionControl) -> DriverResult<AppiumStatus> {
        Ok(AppiumStatus {
            ready: true,
            message: Some("ready".to_owned()),
            version: Some("2.0".to_owned()),
            os_version: Some("26.4".to_owned()),
        })
    }

    async fn create_session(
        &self,
        _: &AppiumSessionRequest,
        _: &ExecutionControl,
    ) -> DriverResult<AppiumSession> {
        *self.connected.lock().expect("connection lock") = true;
        *self.context.lock().expect("context lock") = AppiumContext::native();
        AppiumSession::parse("appium-conformance-session")
    }

    async fn delete_session(&self, _: &AppiumSession, _: &ExecutionControl) -> DriverResult<()> {
        *self.connected.lock().expect("connection lock") = false;
        Ok(())
    }

    async fn contexts(
        &self,
        _: &AppiumSession,
        _: &ExecutionControl,
    ) -> DriverResult<Vec<AppiumContext>> {
        self.require_session()?;
        Ok(vec![
            AppiumContext::native(),
            AppiumContext::parse("WEBVIEW_devicerail")?,
        ])
    }

    async fn current_context(
        &self,
        _: &AppiumSession,
        _: &ExecutionControl,
    ) -> DriverResult<AppiumContext> {
        self.require_session()?;
        Ok(self.context.lock().expect("context lock").clone())
    }

    async fn switch_context(
        &self,
        _: &AppiumSession,
        context: &AppiumContext,
        _: &ExecutionControl,
    ) -> DriverResult<()> {
        self.require_session()?;
        *self.context.lock().expect("context lock") = context.clone();
        Ok(())
    }

    async fn native_source_json(
        &self,
        _: &AppiumSession,
        _: &ExecutionControl,
    ) -> DriverResult<Value> {
        self.require_session()?;
        Ok(json!({
            "type": "XCUIElementTypeApplication",
            "name": "DeviceRail Fixture",
            "enabled": true,
            "children": [
                {
                    "type": "XCUIElementTypeTextField",
                    "label": "Query",
                    "identifier": "query-field",
                    "value": "",
                    "rect": {"x": 10, "y": 20, "width": 180, "height": 40},
                    "enabled": true,
                    "hittable": true,
                    "children": []
                },
                {
                    "type": "XCUIElementTypeButton",
                    "label": "Search",
                    "identifier": "search-button",
                    "rect": {"x": 200, "y": 20, "width": 80, "height": 40},
                    "enabled": true,
                    "hittable": true,
                    "children": []
                }
            ]
        }))
    }

    async fn page_source(&self, _: &AppiumSession, _: &ExecutionControl) -> DriverResult<String> {
        self.require_session()?;
        Ok("<html></html>".to_owned())
    }

    async fn viewport(&self, _: &AppiumSession, _: &ExecutionControl) -> DriverResult<Viewport> {
        self.require_session()?;
        Ok(Viewport {
            width: 320,
            height: 640,
            scale_factor: 1.0,
        })
    }

    async fn screenshot_png(
        &self,
        _: &AppiumSession,
        _: &ExecutionControl,
    ) -> DriverResult<Vec<u8>> {
        self.require_session()?;
        self.screenshot_operations
            .lock()
            .expect("screenshot operation lock")
            .push("display");
        Ok(fixture_png())
    }

    async fn web_viewport_screenshot_png(
        &self,
        _: &AppiumSession,
        _: &ExecutionControl,
    ) -> DriverResult<Vec<u8>> {
        self.require_session()?;
        self.screenshot_operations
            .lock()
            .expect("screenshot operation lock")
            .push("webViewport");
        Ok(fixture_png())
    }

    async fn execute_script(
        &self,
        _: &AppiumSession,
        script: &str,
        arguments: &[Value],
        _: &ExecutionControl,
    ) -> DriverResult<Value> {
        self.require_session()?;
        if script.contains("document.querySelectorAll") {
            let selector = arguments
                .first()
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Ok(if selector == "#query" {
                json!({"invalid": false, "count": 1, "path": "0.0"})
            } else {
                json!({"invalid": false, "count": 0, "path": null})
            });
        }
        Ok(json!({
            "truncated": false,
            "href": "https://example.test/",
            "timeOrigin": "1",
            "documentToken": "document-1",
            "nodes": [
                {
                    "path": "0",
                    "parentPath": null,
                    "xpath": "/html[1]",
                    "role": "html",
                    "name": null,
                    "value": null,
                    "identifier": null,
                    "text": null,
                    "bounds": {"x": 0, "y": 0, "width": 320, "height": 640},
                    "enabled": true,
                    "hittable": true
                },
                {
                    "path": "0.0",
                    "parentPath": "0",
                    "xpath": "/html[1]/input[1]",
                    "role": "textbox",
                    "name": "Query",
                    "value": "",
                    "identifier": "query",
                    "text": null,
                    "bounds": {"x": 10, "y": 20, "width": 180, "height": 40},
                    "enabled": true,
                    "hittable": true
                }
            ]
        }))
    }

    async fn find_element(
        &self,
        _: &AppiumSession,
        _: AppiumLocatorStrategy,
        value: &str,
        _: &ExecutionControl,
    ) -> DriverResult<AppiumElement> {
        self.require_session()?;
        AppiumElement::parse(format!("element-{value}"))
    }

    async fn element_rect(
        &self,
        _: &AppiumSession,
        _: &AppiumElement,
        _: &ExecutionControl,
    ) -> DriverResult<UiRect> {
        self.require_session()?;
        Ok(UiRect {
            x: 10.0,
            y: 20.0,
            width: 80.0,
            height: 40.0,
        })
    }

    async fn element_attribute(
        &self,
        _: &AppiumSession,
        _: &AppiumElement,
        _: &str,
        _: &ExecutionControl,
    ) -> DriverResult<Option<Value>> {
        self.require_session()?;
        Ok(None)
    }

    async fn element_displayed(
        &self,
        _: &AppiumSession,
        _: &AppiumElement,
        _: &ExecutionControl,
    ) -> DriverResult<bool> {
        self.require_session()?;
        Ok(true)
    }

    async fn element_enabled(
        &self,
        _: &AppiumSession,
        _: &AppiumElement,
        _: &ExecutionControl,
    ) -> DriverResult<bool> {
        self.require_session()?;
        Ok(true)
    }

    async fn click_element(
        &self,
        _: &AppiumSession,
        element: &AppiumElement,
        _: &ExecutionControl,
    ) -> DriverResult<()> {
        self.require_session()?;
        self.record(format!("click:{}", element.as_str()));
        Ok(())
    }

    async fn clear_element(
        &self,
        _: &AppiumSession,
        element: &AppiumElement,
        _: &ExecutionControl,
    ) -> DriverResult<()> {
        self.require_session()?;
        self.record(format!("clear:{}", element.as_str()));
        Ok(())
    }

    async fn set_element_value(
        &self,
        _: &AppiumSession,
        element: &AppiumElement,
        value: &str,
        _: &ExecutionControl,
    ) -> DriverResult<()> {
        self.require_session()?;
        self.record(format!("value:{}:{value}", element.as_str()));
        Ok(())
    }

    async fn tap_coordinate(
        &self,
        _: &AppiumSession,
        _: u32,
        _: u32,
        _: &ExecutionControl,
    ) -> DriverResult<()> {
        self.require_session()?;
        self.record("coordinateTap");
        Ok(())
    }

    async fn drag(
        &self,
        _: &AppiumSession,
        _: AppiumDrag,
        _: &ExecutionControl,
    ) -> DriverResult<()> {
        self.require_session()?;
        self.record("drag");
        Ok(())
    }

    async fn send_keys(
        &self,
        _: &AppiumSession,
        text: &str,
        _: &ExecutionControl,
    ) -> DriverResult<()> {
        self.require_session()?;
        self.record(format!("keys:{text}"));
        Ok(())
    }

    async fn press_button(
        &self,
        _: &AppiumSession,
        _: AppiumButton,
        _: &ExecutionControl,
    ) -> DriverResult<()> {
        self.require_session()?;
        self.record("pressButton");
        Ok(())
    }
}

fn fixture_driver() -> AppiumIosDriver {
    AppiumIosDriver::new(
        IosDeviceConfig::new(
            format!("appium-conformance-{}", Uuid::new_v4()),
            "Appium iOS conformance device",
            Some("26.4".to_owned()),
        )
        .expect("device config"),
        Arc::new(FakeAppium::new()),
        AppiumSessionRequest::new("fake-ios-udid").expect("session request"),
    )
}

fn conformance_call(action: &ActionDefinition) -> Result<ActionCall, String> {
    let arguments = match action.name.as_str() {
        "tap" => json!({"x": 1, "y": 1}),
        "inputText" => json!({"text": "DeviceRail"}),
        "keyPress" => json!({"key": "enter"}),
        "swipe" => json!({
            "startX": 1, "startY": 1, "endX": 2, "endY": 2, "durationMs": 100
        }),
        "scroll" => json!({"deltaX": 0, "deltaY": 1}),
        "findElement" => json!({"selector": {"identifier": "query-field"}}),
        "tapElement" => json!({
            "target": {"kind": "selector", "selector": {"identifier": "search-button"}}
        }),
        "clearElement" => json!({
            "target": {"kind": "selector", "selector": {"identifier": "query-field"}}
        }),
        "setElementValue" => json!({
            "target": {"kind": "selector", "selector": {"identifier": "query-field"}},
            "value": "abc"
        }),
        "waitForElement" => json!({
            "selector": {"identifier": "search-button"}, "condition": "present"
        }),
        name => return Err(format!("no Appium conformance fixture for `{name}`")),
    };
    Ok(ActionCall {
        id: Uuid::new_v4(),
        name: action.name.clone(),
        arguments,
    })
}

fn fixture_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 320, 640);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG header");
    let pixels = vec![0x66; 320 * 640 * 4];
    writer.write_image_data(&pixels).expect("PNG image");
    writer.finish().expect("PNG finish");
    bytes
}

struct TemporaryEvidenceStore {
    inner: FileEvidenceStore,
    _root: TempDir,
}

impl TemporaryEvidenceStore {
    fn create() -> Arc<dyn EvidenceStore> {
        let root = tempfile::tempdir().expect("temporary Evidence root");
        let inner = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
            .expect("Evidence Store");
        Arc::new(Self { inner, _root: root })
    }
}

#[async_trait]
impl EvidenceStore for TemporaryEvidenceStore {
    async fn put(
        &self,
        request: PutEvidence,
        input: EvidenceInput,
    ) -> EvidenceResult<StoredEvidence> {
        self.inner.put(request, input).await
    }

    async fn attach(
        &self,
        session_id: &SessionId,
        asset: &AssetRef,
    ) -> EvidenceResult<StoredEvidence> {
        self.inner.attach(session_id, asset).await
    }

    async fn verify_session_reference(
        &self,
        session_id: &SessionId,
        asset: &AssetRef,
    ) -> EvidenceResult<EvidenceMetadata> {
        self.inner.verify_session_reference(session_id, asset).await
    }

    async fn open(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
        self.inner.open(digest).await
    }

    async fn metadata(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
        self.inner.metadata(digest).await
    }

    async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
        self.inner.referenced_sessions().await
    }

    async fn release_session(
        &self,
        session_id: &SessionId,
        released_at_ms: u64,
    ) -> EvidenceResult<ReleaseReport> {
        self.inner.release_session(session_id, released_at_ms).await
    }

    async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
        self.inner.gc(policy).await
    }
}

devicerail_core::driver_conformance_test!(
    appium_driver_conforms_to_shared_contract,
    fixture_driver,
    conformance_call,
    TemporaryEvidenceStore::create(),
);

#[tokio::test]
async fn web_context_uses_dom_and_w3c_element_semantics_for_all_canonical_actions() {
    let transport = Arc::new(FakeAppium::new());
    let driver = Arc::new(AppiumIosDriver::new(
        IosDeviceConfig::new(
            format!("appium-web-{}", Uuid::new_v4()),
            "Appium Safari fixture",
            Some("26.4".to_owned()),
        )
        .expect("device config"),
        Arc::clone(&transport) as Arc<dyn AppiumTransport>,
        AppiumSessionRequest::safari("fake-ios-udid").expect("Safari session request"),
    ));
    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect Appium Driver");
    let events = Arc::new(MemoryEventStore::default());
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("start Session");
    let runtime = DeviceRuntime::with_evidence(
        Arc::clone(&driver),
        Arc::clone(&events),
        TemporaryEvidenceStore::create(),
    );
    let context = OperationContext::new(session.id, None)
        .with_ui_snapshots_enabled(true)
        .with_semantic_actions_enabled(true);
    let web_selector = json!({
        "context": {"contextKind": "web", "contextId": "WEBVIEW_devicerail"},
        "css": "#query"
    });
    let calls = [
        ("findElement", json!({"selector": web_selector.clone()})),
        (
            "tapElement",
            json!({"target": {"kind": "selector", "selector": web_selector.clone()}}),
        ),
        (
            "clearElement",
            json!({"target": {"kind": "selector", "selector": web_selector.clone()}}),
        ),
        (
            "setElementValue",
            json!({
                "target": {"kind": "selector", "selector": web_selector.clone()},
                "value": "abc"
            }),
        ),
        (
            "waitForElement",
            json!({"selector": web_selector, "condition": "visible"}),
        ),
    ];

    for (name, arguments) in calls {
        let result = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: name.to_owned(),
                    arguments,
                },
            )
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error}"));
        let ActionExecution::WebSemantic { context } = result
            .execution
            .expect("semantic Action execution metadata")
        else {
            panic!("{name} did not use the web semantic channel");
        };
        assert_eq!(context.context_kind, UiContextKind::Web);
        assert_eq!(context.context_id, "WEBVIEW_devicerail");
    }

    assert_eq!(
        transport.context.lock().expect("context lock").as_str(),
        "WEBVIEW_devicerail"
    );
    let operations = transport
        .element_operations
        .lock()
        .expect("operation lock")
        .clone();
    assert!(
        operations
            .iter()
            .any(|operation| operation.starts_with("click:"))
    );
    assert!(
        operations
            .iter()
            .any(|operation| operation.starts_with("clear:"))
    );
    assert!(
        operations
            .iter()
            .any(|operation| operation.ends_with(":abc"))
    );
    let screenshots = transport
        .screenshot_operations
        .lock()
        .expect("screenshot operation lock")
        .clone();
    assert!(screenshots.contains(&"webViewport"));
    assert!(
        !screenshots.contains(&"display"),
        "a Web Context must not consume the full-display Appium screenshot"
    );
    driver
        .disconnect(&ExecutionControl::unbounded())
        .await
        .expect("disconnect Appium Driver");
}
