use std::{future::pending, io::Cursor, sync::Arc};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceOperationResult, DriverError, DriverOperationContext, DriverResult,
    ExecutionControl, ScreenshotPolicy, now_ms,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, AssetRef, DeviceId, DeviceInfo,
    Observation, Platform, ScreenshotOmissionReason, Viewport,
};
use png::{DecodeOptions, Decoder, Limits};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::{sync::Mutex, time};
use url::Url;
use uuid::Uuid;

use crate::bridge::{
    BridgeConfig, BrowserKind, DiscoveredPage, PlaywrightBridge, SystemPlaywrightBridge,
};

const MAX_SELECTOR_CHARS: usize = 2_048;
const MAX_TEXT_CHARS: usize = 16 * 1_024;
/// The helper cleans and caps readValueNearLabel values to 4096 code points;
/// the response gate holds it to that promise.
const MAX_VALUE_CHARS: usize = 4_096;
const MAX_TEXT_BYTES: usize = 64 * 1_024;
const MAX_URL_CHARS: usize = 2_048;
const MAX_URL_BYTES: usize = 8 * 1_024;
const MAX_KEY_CHARS: usize = 128;
const MAX_SCROLL_DELTA: i64 = 1_000_000;
const MAX_SCREENSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SCREENSHOT_DIMENSION: u32 = 8_192;
const MAX_SCREENSHOT_PIXELS: u64 = 16_000_000;
const MAX_SCREENSHOT_DECODED_BYTES: usize = 64 * 1024 * 1024;

struct DriverState {
    connected: bool,
    page: DiscoveredPage,
}

pub struct PlaywrightDriver {
    id: DeviceId,
    name: String,
    browser: BrowserKind,
    bridge: Arc<dyn PlaywrightBridge>,
    state: Mutex<DriverState>,
}

impl std::fmt::Debug for PlaywrightDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlaywrightDriver")
            .field("id", &self.id)
            .field("browser", &self.browser)
            .finish_non_exhaustive()
    }
}

impl PlaywrightDriver {
    pub fn new(
        id: DeviceId,
        name: String,
        browser: BrowserKind,
        bridge: Arc<dyn PlaywrightBridge>,
        page: DiscoveredPage,
    ) -> DriverResult<Self> {
        validate_page(&page)?;
        if id.0.trim().is_empty() || name.trim().is_empty() {
            return Err(DriverError::Protocol(
                "Playwright device identity is invalid".to_owned(),
            ));
        }
        Ok(Self {
            id,
            name,
            browser,
            bridge,
            state: Mutex::new(DriverState {
                connected: false,
                page,
            }),
        })
    }

    pub async fn device_info(&self) -> DeviceInfo {
        let state = self.state.lock().await;
        self.info(state.connected)
    }

    async fn capture(
        &self,
        state: &mut DriverState,
        context: &DriverOperationContext,
        omission: Option<ScreenshotOmissionReason>,
        redact_metadata: bool,
    ) -> DeviceOperationResult<Observation> {
        let control = context.control();
        let (next, screenshot) = if omission.is_some() {
            (self.bridge.probe(&state.page, control).await?, None)
        } else {
            let (next, bytes) = self.bridge.observe(&state.page, control).await?;
            validate_png(&bytes)?;
            (next, Some(bytes))
        };
        validate_page(&next)?;
        state.page = next.clone();
        self.observation_from_page(context, &next, screenshot, omission, redact_metadata)
            .await
    }

    async fn observation_from_page(
        &self,
        context: &DriverOperationContext,
        page: &DiscoveredPage,
        screenshot_bytes: Option<Vec<u8>>,
        omission: Option<ScreenshotOmissionReason>,
        redact_metadata: bool,
    ) -> DeviceOperationResult<Observation> {
        let screenshot = match screenshot_bytes {
            Some(bytes) => {
                let size = bytes.len() as u64;
                let stored = context
                    .evidence()
                    .put_with_declared_size("image/png", size, Box::pin(Cursor::new(bytes)))
                    .await?;
                Some(stored.asset_ref())
            }
            None => None,
        };
        let mut metadata = Map::new();
        metadata.insert("browser".to_owned(), json!(browser_slug(self.browser)));
        metadata.insert("contextIndex".to_owned(), json!(page.context_index));
        metadata.insert("pageIndex".to_owned(), json!(page.page_index));
        if !redact_metadata {
            metadata.insert("url".to_owned(), json!(page.url));
            metadata.insert("title".to_owned(), json!(page.title));
        }
        Ok(Observation {
            id: Uuid::new_v4(),
            device_id: self.id.clone(),
            captured_at_ms: now_ms(),
            viewport: Viewport {
                width: page.viewport.width,
                height: page.viewport.height,
                scale_factor: page.viewport.scale_factor,
            },
            screenshot,
            screenshot_omission: omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata,
        })
    }

    fn info(&self, connected: bool) -> DeviceInfo {
        DeviceInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            platform: Platform::Web,
            os_version: Some(format!("playwright-{}", browser_slug(self.browser))),
            connected,
        }
    }
}

#[async_trait]
impl DeviceDriver for PlaywrightDriver {
    fn id(&self) -> &DeviceId {
        &self.id
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        match name {
            "fillSecret" => Some(ActionProtection::Protected),
            "navigate" | "click" | "fill" | "press" | "select" | "scroll" | "waitFor"
            | "waitForSelector" | "clickByText" | "readValueNearLabel" | "elementExists"
            | "textContains" => Some(ActionProtection::Standard),
            _ => None,
        }
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        let mut state = lock_state(&self.state, control).await?;
        if state.connected {
            return Ok(self.info(true));
        }
        let page = self.bridge.probe(&state.page, control).await?;
        validate_page(&page)?;
        state.page = page;
        state.connected = true;
        Ok(self.info(true))
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        let mut state = lock_state(&self.state, control).await?;
        state.connected = false;
        Ok(())
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        ensure_active(control)?;
        Ok(action_definitions())
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        let state = lock_state(&self.state, control).await?;
        let page = self.bridge.probe(&state.page, control).await?;
        validate_page(&page)
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let mut state = lock_state(&self.state, context.control()).await?;
        if !state.connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        self.capture(&mut state, context, omission, false).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let mut state = lock_state(&self.state, context.control()).await?;
        if !state.connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let ActionCall {
            id: call_id,
            name,
            arguments,
        } = call;
        let parsed = ParsedAction::parse(&name, arguments)?;
        let protected = parsed.is_protected();
        let output_kind = parsed.output_kind();
        let omission = if protected {
            Some(ScreenshotOmissionReason::ProtectedAction)
        } else {
            match context.screenshot_policy() {
                ScreenshotPolicy::Capture => None,
                ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
            }
        };
        let before = self
            .capture(&mut state, context, omission, protected)
            .await?;
        let started_at_ms = now_ms();
        let bridge_result = self
            .bridge
            .execute(&state.page, parsed.into_bridge_value(), context.control())
            .await?;
        validate_page(&bridge_result.page)?;
        let output = validate_action_output(output_kind, bridge_result.output, &name)?;
        state.page = bridge_result.page;
        let after = self
            .capture(&mut state, context, omission, protected)
            .await?;
        ensure_active(context.control())?;
        let finished_at_ms = now_ms().max(started_at_ms);
        let evidence = deduplicated_screenshots(&before, &after);
        Ok(ActionResult {
            call_id,
            started_at_ms,
            finished_at_ms,
            output,
            before: Some(before),
            after: Some(after),
            evidence,
            execution: None,
        })
    }
}

pub async fn discover_playwright_drivers(
    config: BridgeConfig,
    control: &ExecutionControl,
) -> DriverResult<Vec<Arc<PlaywrightDriver>>> {
    let browser = config.browser();
    let endpoint_hash = config.endpoint_fingerprint();
    let bridge = Arc::new(SystemPlaywrightBridge::new(config));
    let pages = bridge.discover(control).await?;
    let mut drivers = Vec::with_capacity(pages.len());
    for page in pages {
        validate_page(&page)?;
        let id = DeviceId::new(format!(
            "playwright-{}-{}-c{}-p{}",
            browser_slug(browser),
            &endpoint_hash[..16],
            page.context_index,
            page.page_index
        ));
        let name = format!(
            "Playwright {} page c{}/p{}",
            browser_slug(browser),
            page.context_index,
            page.page_index
        );
        drivers.push(Arc::new(PlaywrightDriver::new(
            id,
            name,
            browser,
            Arc::clone(&bridge) as Arc<dyn PlaywrightBridge>,
            page,
        )?));
    }
    Ok(drivers)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NavigateArgs {
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClickArgs {
    selector: String,
    #[serde(default = "default_button")]
    button: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FillArgs {
    selector: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PressArgs {
    selector: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectArgs {
    selector: String,
    value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScrollArgs {
    delta_x: i64,
    delta_y: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitForArgs {
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ElementExistsArgs {
    selector: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextContainsArgs {
    selector: String,
    text: String,
    #[serde(default = "default_true")]
    case_sensitive: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitForSelectorArgs {
    selector: String,
    #[serde(default = "default_selector_state")]
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClickByTextArgs {
    text: String,
    #[serde(default = "default_true")]
    exact: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadValueNearLabelArgs {
    label: String,
    #[serde(default = "default_direction")]
    direction: String,
    #[serde(default = "default_true")]
    exact: bool,
}

#[derive(Clone, Copy)]
enum ActionOutputKind {
    Acknowledgement,
    ElementExists,
    TextContains,
    Value,
}

enum ParsedAction {
    Navigate(NavigateArgs),
    Click(ClickArgs),
    Fill(FillArgs, bool),
    Press(PressArgs),
    Select(SelectArgs),
    Scroll(ScrollArgs),
    WaitFor(WaitForArgs),
    WaitForSelector(WaitForSelectorArgs),
    ClickByText(ClickByTextArgs),
    ReadValueNearLabel(ReadValueNearLabelArgs),
    ElementExists(ElementExistsArgs),
    TextContains(TextContainsArgs),
}

impl ParsedAction {
    fn parse(name: &str, arguments: Value) -> DriverResult<Self> {
        let invalid = || DriverError::InvalidArguments {
            action: name.to_owned(),
            message: "arguments do not match the advertised schema".to_owned(),
        };
        let parsed = match name {
            "navigate" => {
                let value: NavigateArgs =
                    serde_json::from_value(arguments).map_err(|_| invalid())?;
                validate_url(&value.url).then_some(()).ok_or_else(invalid)?;
                Self::Navigate(value)
            }
            "click" => {
                let value: ClickArgs = serde_json::from_value(arguments).map_err(|_| invalid())?;
                validate_selector(&value.selector)
                    .then_some(())
                    .ok_or_else(invalid)?;
                matches!(value.button.as_str(), "left" | "middle" | "right")
                    .then_some(())
                    .ok_or_else(invalid)?;
                Self::Click(value)
            }
            "fill" | "fillSecret" => {
                let value: FillArgs = serde_json::from_value(arguments).map_err(|_| invalid())?;
                validate_selector(&value.selector)
                    .then_some(())
                    .ok_or_else(invalid)?;
                validate_text(&value.text)
                    .then_some(())
                    .ok_or_else(invalid)?;
                Self::Fill(value, name == "fillSecret")
            }
            "press" => {
                let value: PressArgs = serde_json::from_value(arguments).map_err(|_| invalid())?;
                validate_selector(&value.selector)
                    .then_some(())
                    .ok_or_else(invalid)?;
                validate_key(&value.key).then_some(()).ok_or_else(invalid)?;
                Self::Press(value)
            }
            "select" => {
                let value: SelectArgs = serde_json::from_value(arguments).map_err(|_| invalid())?;
                validate_selector(&value.selector)
                    .then_some(())
                    .ok_or_else(invalid)?;
                validate_text(&value.value)
                    .then_some(())
                    .ok_or_else(invalid)?;
                Self::Select(value)
            }
            "scroll" => {
                let value: ScrollArgs = serde_json::from_value(arguments).map_err(|_| invalid())?;
                ((value.delta_x != 0 || value.delta_y != 0)
                    && value.delta_x.abs() <= MAX_SCROLL_DELTA
                    && value.delta_y.abs() <= MAX_SCROLL_DELTA)
                    .then_some(())
                    .ok_or_else(invalid)?;
                Self::Scroll(value)
            }
            "waitFor" => {
                let value: WaitForArgs =
                    serde_json::from_value(arguments).map_err(|_| invalid())?;
                matches!(
                    value.state.as_str(),
                    "load" | "domcontentloaded" | "networkidle"
                )
                .then_some(())
                .ok_or_else(invalid)?;
                Self::WaitFor(value)
            }
            "elementExists" => {
                let value: ElementExistsArgs =
                    serde_json::from_value(arguments).map_err(|_| invalid())?;
                validate_selector(&value.selector)
                    .then_some(())
                    .ok_or_else(invalid)?;
                Self::ElementExists(value)
            }
            "textContains" => {
                let value: TextContainsArgs =
                    serde_json::from_value(arguments).map_err(|_| invalid())?;
                validate_selector(&value.selector)
                    .then_some(())
                    .ok_or_else(invalid)?;
                validate_text(&value.text)
                    .then_some(())
                    .ok_or_else(invalid)?;
                Self::TextContains(value)
            }
            "waitForSelector" => {
                let value: WaitForSelectorArgs =
                    serde_json::from_value(arguments).map_err(|_| invalid())?;
                validate_selector(&value.selector)
                    .then_some(())
                    .ok_or_else(invalid)?;
                matches!(
                    value.state.as_str(),
                    "attached" | "detached" | "visible" | "hidden"
                )
                .then_some(())
                .ok_or_else(invalid)?;
                Self::WaitForSelector(value)
            }
            "clickByText" => {
                let value: ClickByTextArgs =
                    serde_json::from_value(arguments).map_err(|_| invalid())?;
                (!value.text.is_empty() && validate_text(&value.text))
                    .then_some(())
                    .ok_or_else(invalid)?;
                Self::ClickByText(value)
            }
            "readValueNearLabel" => {
                let value: ReadValueNearLabelArgs =
                    serde_json::from_value(arguments).map_err(|_| invalid())?;
                (!value.label.is_empty() && validate_text(&value.label))
                    .then_some(())
                    .ok_or_else(invalid)?;
                matches!(value.direction.as_str(), "right" | "below")
                    .then_some(())
                    .ok_or_else(invalid)?;
                Self::ReadValueNearLabel(value)
            }
            _ => return Err(DriverError::UnknownAction(name.to_owned())),
        };
        Ok(parsed)
    }

    fn is_protected(&self) -> bool {
        matches!(self, Self::Fill(_, true))
    }

    fn output_kind(&self) -> ActionOutputKind {
        match self {
            Self::ElementExists(_) => ActionOutputKind::ElementExists,
            Self::TextContains(_) => ActionOutputKind::TextContains,
            Self::ReadValueNearLabel(_) => ActionOutputKind::Value,
            _ => ActionOutputKind::Acknowledgement,
        }
    }

    fn into_bridge_value(self) -> Value {
        match self {
            Self::Navigate(value) => json!({ "type": "navigate", "url": value.url }),
            Self::Click(value) => json!({
                "type": "click",
                "selector": value.selector,
                "button": value.button
            }),
            Self::Fill(value, protected) => json!({
                "type": if protected { "fillSecret" } else { "fill" },
                "selector": value.selector,
                "text": value.text
            }),
            Self::Press(value) => json!({
                "type": "press",
                "selector": value.selector,
                "key": value.key
            }),
            Self::Select(value) => json!({
                "type": "select",
                "selector": value.selector,
                "value": value.value
            }),
            Self::Scroll(value) => json!({
                "type": "scroll",
                "deltaX": value.delta_x,
                "deltaY": value.delta_y
            }),
            Self::WaitFor(value) => json!({ "type": "waitFor", "state": value.state }),
            Self::WaitForSelector(value) => json!({
                "type": "waitForSelector",
                "selector": value.selector,
                "state": value.state
            }),
            Self::ClickByText(value) => json!({
                "type": "clickByText",
                "text": value.text,
                "exact": value.exact
            }),
            Self::ReadValueNearLabel(value) => json!({
                "type": "readValueNearLabel",
                "label": value.label,
                "direction": value.direction,
                "exact": value.exact
            }),
            Self::ElementExists(value) => {
                json!({ "type": "elementExists", "selector": value.selector })
            }
            Self::TextContains(value) => json!({
                "type": "textContains",
                "selector": value.selector,
                "text": value.text,
                "caseSensitive": value.case_sensitive
            }),
        }
    }
}

fn validate_action_output(
    kind: ActionOutputKind,
    output: Option<Value>,
    action: &str,
) -> DriverResult<Value> {
    match (kind, output) {
        (ActionOutputKind::Acknowledgement, None) => {
            Ok(json!({ "accepted": true, "action": action }))
        }
        (ActionOutputKind::ElementExists, Some(Value::Object(value)))
            if value.len() == 1 && value.get("exists").is_some_and(Value::is_boolean) =>
        {
            Ok(Value::Object(value))
        }
        (ActionOutputKind::TextContains, Some(Value::Object(value)))
            if value.len() == 1 && value.get("contains").is_some_and(Value::is_boolean) =>
        {
            Ok(Value::Object(value))
        }
        (ActionOutputKind::Value, Some(Value::Object(value)))
            if value.len() == 1
                && value.get("value").is_some_and(|read| {
                    read.as_str().is_some_and(|text| {
                        !text.is_empty()
                            && text.chars().count() <= MAX_VALUE_CHARS
                            && validate_text(text)
                    })
                }) =>
        {
            Ok(Value::Object(value))
        }
        _ => Err(platform("bridge_invalid_response", false)),
    }
}

fn action_definitions() -> Vec<ActionDefinition> {
    vec![
        definition(
            "navigate",
            "Navigate to an HTTP or HTTPS URL.",
            schema_navigate(),
            ActionProtection::Standard,
        ),
        definition(
            "click",
            "Click one element selected by strict CSS.",
            schema_click(),
            ActionProtection::Standard,
        ),
        definition(
            "fill",
            "Fill one form control selected by strict CSS.",
            schema_fill(),
            ActionProtection::Standard,
        ),
        definition(
            "fillSecret",
            "Fill a protected value without screenshots or durable arguments.",
            schema_fill(),
            ActionProtection::Protected,
        ),
        definition(
            "press",
            "Press one bounded Playwright key on a strict CSS target.",
            schema_press(),
            ActionProtection::Standard,
        ),
        definition(
            "select",
            "Select one option value on a strict CSS target.",
            schema_select(),
            ActionProtection::Standard,
        ),
        definition(
            "scroll",
            "Scroll the page by bounded integer deltas.",
            schema_scroll(),
            ActionProtection::Standard,
        ),
        definition(
            "waitFor",
            "Wait for one closed page load state.",
            schema_wait(),
            ActionProtection::Standard,
        ),
        definition(
            "waitForSelector",
            "Wait until a strict CSS target reaches one closed element state (default: visible).",
            schema_wait_for_selector(),
            ActionProtection::Standard,
        ),
        definition(
            "clickByText",
            "Click the single element matching bounded visible text (exact by default; ambiguity fails).",
            schema_click_by_text(),
            ActionProtection::Standard,
        ),
        definition(
            "readValueNearLabel",
            "Read the value laid out next to a unique label (right or below), resolved by geometry at run time.",
            schema_read_value_near_label(),
            ActionProtection::Standard,
        ),
        definition(
            "elementExists",
            "Return whether at least one element matches a bounded CSS selector.",
            schema_element_exists(),
            ActionProtection::Standard,
        ),
        definition(
            "textContains",
            "Return whether exactly one CSS target contains bounded text.",
            schema_text_contains(),
            ActionProtection::Standard,
        ),
    ]
}

fn definition(
    name: &str,
    description: &str,
    input_schema: Value,
    protection: ActionProtection,
) -> ActionDefinition {
    ActionDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        protection,
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

fn selector_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": MAX_SELECTOR_CHARS,
        "pattern": "^[ -~]+$"
    })
}

fn text_schema() -> Value {
    json!({ "type": "string", "maxLength": MAX_TEXT_CHARS })
}

fn schema_navigate() -> Value {
    object_schema(
        json!({ "url": {
            "type": "string", "minLength": 1, "maxLength": MAX_URL_CHARS,
            "pattern": "^https?://"
        }}),
        &["url"],
    )
}

fn schema_click() -> Value {
    object_schema(
        json!({
            "selector": selector_schema(),
            "button": { "type": "string", "enum": ["left", "middle", "right"], "default": "left" }
        }),
        &["selector"],
    )
}

fn schema_fill() -> Value {
    object_schema(
        json!({ "selector": selector_schema(), "text": text_schema() }),
        &["selector", "text"],
    )
}

fn schema_press() -> Value {
    object_schema(
        json!({
            "selector": selector_schema(),
            "key": { "type": "string", "minLength": 1, "maxLength": MAX_KEY_CHARS, "pattern": "^[ -~]+$" }
        }),
        &["selector", "key"],
    )
}

fn schema_select() -> Value {
    object_schema(
        json!({ "selector": selector_schema(), "value": text_schema() }),
        &["selector", "value"],
    )
}

fn schema_scroll() -> Value {
    let mut schema = object_schema(
        json!({
            "deltaX": { "type": "integer", "minimum": -MAX_SCROLL_DELTA, "maximum": MAX_SCROLL_DELTA },
            "deltaY": { "type": "integer", "minimum": -MAX_SCROLL_DELTA, "maximum": MAX_SCROLL_DELTA }
        }),
        &["deltaX", "deltaY"],
    );
    schema.as_object_mut().expect("object schema").insert(
        "not".to_owned(),
        json!({ "properties": { "deltaX": { "const": 0 }, "deltaY": { "const": 0 } }, "required": ["deltaX", "deltaY"] }),
    );
    schema
}

fn schema_wait() -> Value {
    object_schema(
        json!({ "state": {
            "type": "string", "enum": ["load", "domcontentloaded", "networkidle"]
        }}),
        &["state"],
    )
}

fn schema_element_exists() -> Value {
    object_schema(json!({ "selector": selector_schema() }), &["selector"])
}

fn schema_wait_for_selector() -> Value {
    object_schema(
        json!({
            "selector": selector_schema(),
            "state": {
                "type": "string",
                "enum": ["attached", "detached", "visible", "hidden"],
                "default": "visible"
            }
        }),
        &["selector"],
    )
}

fn schema_click_by_text() -> Value {
    object_schema(
        json!({
            "text": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS },
            "exact": { "type": "boolean", "default": true }
        }),
        &["text"],
    )
}

fn schema_read_value_near_label() -> Value {
    object_schema(
        json!({
            "label": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS },
            "direction": { "type": "string", "enum": ["right", "below"], "default": "right" },
            "exact": { "type": "boolean", "default": true }
        }),
        &["label"],
    )
}

fn schema_text_contains() -> Value {
    object_schema(
        json!({
            "selector": selector_schema(),
            "text": text_schema(),
            "caseSensitive": { "type": "boolean", "default": true }
        }),
        &["selector", "text"],
    )
}

fn validate_selector(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_SELECTOR_CHARS
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        && !value.contains(">>")
        && !value.starts_with("css=")
}

fn validate_text(value: &str) -> bool {
    value.chars().count() <= MAX_TEXT_CHARS
        && value.len() <= MAX_TEXT_BYTES
        && !value.chars().any(|character| {
            matches!(character, '\0'..='\u{0008}' | '\u{000b}' | '\u{000c}' | '\u{000e}'..='\u{001f}' | '\u{007f}'..='\u{009f}')
        })
}

fn validate_key(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_KEY_CHARS
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn validate_url(value: &str) -> bool {
    value.chars().count() <= MAX_URL_CHARS
        && value.len() <= MAX_URL_BYTES
        && Url::parse(value).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
        })
}

fn default_button() -> String {
    "left".to_owned()
}

fn default_true() -> bool {
    true
}

fn default_selector_state() -> String {
    "visible".to_owned()
}

fn default_direction() -> String {
    "right".to_owned()
}

fn validate_page(page: &DiscoveredPage) -> DriverResult<()> {
    let fingerprint = page.fingerprint.as_bytes();
    if page.context_index > 1_023
        || page.page_index > 1_023
        || fingerprint.len() != 64
        || !fingerprint
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        || page.url.len() > MAX_URL_BYTES
        || page.title.len() > 4 * 1_024
        || page.viewport.width == 0
        || page.viewport.height == 0
        || page.viewport.width > MAX_SCREENSHOT_DIMENSION
        || page.viewport.height > MAX_SCREENSHOT_DIMENSION
        || !page.viewport.scale_factor.is_finite()
        || page.viewport.scale_factor <= 0.0
        || page.viewport.scale_factor > 16.0
    {
        return Err(DriverError::Protocol(
            "Playwright bridge returned an invalid page".to_owned(),
        ));
    }
    Ok(())
}

fn validate_png(bytes: &[u8]) -> DriverResult<()> {
    if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(platform("invalid_screenshot", false));
    }
    let mut options = DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(true);
    options.set_ignore_iccp_chunk(true);
    let mut decoder = Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(Limits {
        bytes: MAX_SCREENSHOT_DECODED_BYTES,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|_| platform("invalid_screenshot", false))?;
    let info = reader.info();
    let pixels = u64::from(info.width)
        .checked_mul(u64::from(info.height))
        .ok_or_else(|| platform("invalid_screenshot", false))?;
    if info.width == 0
        || info.height == 0
        || info.width > MAX_SCREENSHOT_DIMENSION
        || info.height > MAX_SCREENSHOT_DIMENSION
        || pixels > MAX_SCREENSHOT_PIXELS
        || reader
            .output_buffer_size()
            .is_none_or(|size| size > MAX_SCREENSHOT_DECODED_BYTES)
    {
        return Err(platform("invalid_screenshot", false));
    }
    while reader
        .next_row()
        .map_err(|_| platform("invalid_screenshot", false))?
        .is_some()
    {}
    reader
        .finish()
        .map_err(|_| platform("invalid_screenshot", false))?;
    Ok(())
}

async fn lock_state<'a>(
    state: &'a Mutex<DriverState>,
    control: &ExecutionControl,
) -> DriverResult<tokio::sync::MutexGuard<'a, DriverState>> {
    ensure_active(control)?;
    let deadline = async {
        match control.remaining() {
            Some(remaining) => time::sleep(remaining).await,
            None => pending::<()>().await,
        }
    };
    tokio::select! {
        guard = state.lock() => Ok(guard),
        _ = control.cancelled() => Err(DriverError::Cancelled),
        _ = deadline => Err(DriverError::TimedOut),
    }
}

fn deduplicated_screenshots(before: &Observation, after: &Observation) -> Vec<AssetRef> {
    let mut evidence = Vec::with_capacity(2);
    for asset in before.screenshot.iter().chain(after.screenshot.iter()) {
        if !evidence.contains(asset) {
            evidence.push(asset.clone());
        }
    }
    evidence
}

fn ensure_active(control: &ExecutionControl) -> DriverResult<()> {
    if control.is_cancelled() {
        Err(DriverError::Cancelled)
    } else if control.is_expired() {
        Err(DriverError::TimedOut)
    } else {
        Ok(())
    }
}

fn browser_slug(browser: BrowserKind) -> &'static str {
    match browser {
        BrowserKind::Chromium => "chromium",
        BrowserKind::Firefox => "firefox",
        BrowserKind::Webkit => "webkit",
    }
}

fn platform(code: &str, retryable: bool) -> DriverError {
    DriverError::Platform {
        code: code.to_owned(),
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use async_trait::async_trait;
    use devicerail_core::{
        DeviceDriver, DeviceRuntime, EvidenceInput, EvidenceMetadata, EvidenceOutput,
        EvidenceResult, EvidenceStore, ExecutionControl, GcPolicy, GcReport, MemoryEventStore,
        OperationContext, PutEvidence, ReleaseReport, SessionEventStore, Sha256Digest,
        StartSession, StoredEvidence, now_ms,
    };
    use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
    use devicerail_protocol::{
        ActionCall, ActionDefinition, AssetRef, ScreenshotOmissionReason, SessionId,
    };
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::{BrowserKind, DiscoveredPage, PlaywrightBridge, PlaywrightDriver};
    use crate::bridge::{BridgeActionResult, BridgeViewport};

    struct FakeBridge {
        page: StdMutex<DiscoveredPage>,
        sequence: StdMutex<u64>,
    }

    impl FakeBridge {
        fn new() -> Self {
            Self {
                page: StdMutex::new(page(0, "https://example.test/", "Example")),
                sequence: StdMutex::new(0),
            }
        }

        fn require(
            &self,
            requested: &DiscoveredPage,
        ) -> devicerail_core::DriverResult<DiscoveredPage> {
            let current = self.page.lock().expect("fake page lock");
            if requested.context_index != current.context_index
                || requested.page_index != current.page_index
                || requested.fingerprint != current.fingerprint
            {
                return Err(devicerail_core::DriverError::Platform {
                    code: "page_identity_changed".to_owned(),
                    retryable: false,
                });
            }
            Ok(current.clone())
        }
    }

    #[async_trait]
    impl PlaywrightBridge for FakeBridge {
        async fn discover(
            &self,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<Vec<DiscoveredPage>> {
            Ok(vec![self.page.lock().expect("fake page lock").clone()])
        }

        async fn probe(
            &self,
            requested: &DiscoveredPage,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<DiscoveredPage> {
            self.require(requested)
        }

        async fn observe(
            &self,
            requested: &DiscoveredPage,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<(DiscoveredPage, Vec<u8>)> {
            Ok((self.require(requested)?, fixture_png()))
        }

        async fn execute(
            &self,
            requested: &DiscoveredPage,
            action: Value,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<BridgeActionResult> {
            self.require(requested)?;
            let output = match action.get("type").and_then(Value::as_str) {
                Some("elementExists") => Some(json!({ "exists": true })),
                Some("textContains") => Some(json!({ "contains": true })),
                Some("readValueNearLabel") => Some(json!({ "value": "$76.55B" })),
                _ => None,
            };
            let mut sequence = self.sequence.lock().expect("fake sequence lock");
            *sequence += 1;
            let mut current = self.page.lock().expect("fake page lock");
            if action.get("type") == Some(&Value::String("navigate".to_owned())) {
                current.url = action["url"].as_str().expect("navigation URL").to_owned();
            }
            let protected = action.get("type") == Some(&Value::String("fillSecret".to_owned()));
            if protected {
                current.title = action["text"].as_str().expect("secret text").to_owned();
            }
            current.fingerprint = format!("{:064x}", *sequence + 1);
            let mut response = current.clone();
            if protected {
                response.url.clear();
                response.title.clear();
            }
            Ok(BridgeActionResult {
                page: response,
                output,
            })
        }
    }

    fn page(sequence: u64, url: &str, title: &str) -> DiscoveredPage {
        DiscoveredPage {
            context_index: 0,
            page_index: 0,
            fingerprint: format!("{:064x}", sequence + 1),
            url: url.to_owned(),
            title: title.to_owned(),
            viewport: BridgeViewport {
                width: 1,
                height: 1,
                scale_factor: 1.0,
            },
        }
    }

    fn fixture_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer
            .write_image_data(&[0x22, 0x44, 0x66, 0xff])
            .expect("PNG image");
        writer.finish().expect("PNG finish");
        bytes
    }

    fn fixture_driver() -> PlaywrightDriver {
        PlaywrightDriver::new(
            devicerail_protocol::DeviceId::new(format!("playwright-test-{}", Uuid::new_v4())),
            "Playwright test page".to_owned(),
            BrowserKind::Chromium,
            Arc::new(FakeBridge::new()),
            page(0, "https://example.test/", "Example"),
        )
        .expect("fixture Driver")
    }

    fn conformance_call(action: &ActionDefinition) -> Result<ActionCall, String> {
        let arguments = match action.name.as_str() {
            "navigate" => json!({ "url": "https://example.test/next" }),
            "click" => json!({ "selector": "#button" }),
            "fill" => json!({ "selector": "#input", "text": "DeviceRail" }),
            "fillSecret" => json!({ "selector": "#secret", "text": "CONFORMANCE_SECRET" }),
            "press" => json!({ "selector": "#input", "key": "Enter" }),
            "select" => json!({ "selector": "#select", "value": "one" }),
            "scroll" => json!({ "deltaX": 0, "deltaY": 100 }),
            "waitFor" => json!({ "state": "load" }),
            "waitForSelector" => json!({ "selector": "#panel", "state": "visible" }),
            "clickByText" => json!({ "text": "Example" }),
            "readValueNearLabel" => json!({ "label": "Total", "direction": "right" }),
            "elementExists" => json!({ "selector": "#result" }),
            "textContains" => json!({ "selector": "body", "text": "Example" }),
            name => return Err(format!("no Playwright conformance fixture for `{name}`")),
        };
        Ok(ActionCall {
            id: Uuid::new_v4(),
            name: action.name.clone(),
            arguments,
        })
    }

    /// Keeps the disposable root alive until Store file handles are closed.
    struct TemporaryEvidenceStore {
        inner: FileEvidenceStore,
        _root: TempDir,
    }

    impl TemporaryEvidenceStore {
        fn create() -> Arc<dyn EvidenceStore> {
            let root = tempfile::tempdir().expect("temporary Evidence Store root");
            let inner = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("temporary Evidence Store");
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
        conforms_to_shared_driver_contract,
        fixture_driver,
        conformance_call,
        TemporaryEvidenceStore::create(),
    );

    #[tokio::test]
    async fn isolated_conformance_enforces_json_schema() {
        let failure = devicerail_core::conformance::run_driver_conformance_with_evidence(
            fixture_driver,
            |action: &ActionDefinition| {
                let mut call = conformance_call(action)?;
                if action.name == "navigate" {
                    call.arguments = json!({ "url": 7 });
                }
                Ok(call)
            },
            TemporaryEvidenceStore::create(),
        )
        .await
        .expect_err("JSON Schema conformance must reject an invalid factory call");

        assert_eq!(failure.check, "action_factory");
        assert!(
            failure
                .message
                .contains("factory arguments for `navigate` do not satisfy inputSchema"),
            "unexpected conformance failure: {failure}"
        );
    }

    #[tokio::test]
    async fn protected_fill_omits_screenshots_metadata_and_durable_arguments() {
        const SECRET: &str = "DO-NOT-PERSIST-PLAYWRIGHT";
        let driver = Arc::new(fixture_driver());
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::with_evidence(
            Arc::clone(&driver),
            Arc::clone(&events),
            TemporaryEvidenceStore::create(),
        );
        runtime
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("connect");
        let start = StartSession::new(None, Some(driver.id().clone()), now_ms());
        let context = OperationContext::new(start.session_id.clone(), None);
        events.start_session(start).await.expect("start session");
        let result = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "fillSecret".to_owned(),
                    arguments: json!({ "selector": "#secret", "text": SECRET }),
                },
            )
            .await
            .expect("protected action");

        assert!(result.evidence.is_empty());
        for observation in [result.before.as_ref(), result.after.as_ref()] {
            let observation = observation.expect("protected observation");
            assert!(observation.screenshot.is_none());
            assert_eq!(
                observation.screenshot_omission,
                Some(ScreenshotOmissionReason::ProtectedAction)
            );
            assert!(!observation.metadata.contains_key("url"));
            assert!(!observation.metadata.contains_key("title"));
        }
        let durable = serde_json::to_string(
            &events
                .list_after(&context.session_id, None)
                .await
                .expect("events"),
        )
        .expect("serialize events");
        assert!(!durable.contains(SECRET));
    }

    #[tokio::test]
    async fn read_only_queries_return_strict_machine_checkable_outputs() {
        let driver = Arc::new(fixture_driver());
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::with_evidence(
            Arc::clone(&driver),
            Arc::clone(&events),
            TemporaryEvidenceStore::create(),
        );
        runtime
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("connect");
        let start = StartSession::new(None, Some(driver.id().clone()), now_ms());
        let context = OperationContext::new(start.session_id.clone(), None);
        events.start_session(start).await.expect("start session");

        let exists = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "elementExists".to_owned(),
                    arguments: json!({ "selector": "#result" }),
                },
            )
            .await
            .expect("elementExists");
        assert_eq!(exists.output, json!({ "exists": true }));

        let contains = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "textContains".to_owned(),
                    arguments: json!({
                        "selector": "body",
                        "text": "example",
                        "caseSensitive": false
                    }),
                },
            )
            .await
            .expect("textContains");
        assert_eq!(contains.output, json!({ "contains": true }));

        // the Value output kind flows end-to-end and survives the output gate
        let read = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "readValueNearLabel".to_owned(),
                    arguments: json!({ "label": "完全稀释市值", "direction": "right" }),
                },
            )
            .await
            .expect("readValueNearLabel");
        assert_eq!(read.output, json!({ "value": "$76.55B" }));
    }
}
