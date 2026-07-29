use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devicerail_core::{DriverError, DriverResult, ExecutionControl, run_bounded_blocking};
use devicerail_protocol::{UiRect, Viewport};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use crate::{
    HttpEndpointConfig,
    control::{platform, run_controlled},
};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 256 * 1024;
const MAX_REQUEST_JSON_DEPTH: usize = 64;
const MAX_REQUEST_JSON_NODES: usize = 32 * 1024;
const MAX_JSON_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SCREENSHOT_BODY_BYTES: usize = 48 * 1024 * 1024;
const MAX_SCREENSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SESSION_ID_BYTES: usize = 512;
const MAX_ELEMENT_ID_BYTES: usize = 4_096;
const MAX_CONTEXT_NAME_BYTES: usize = 4_096;
const MAX_CONTEXTS: usize = 256;
const MAX_CAPABILITY_NAME_BYTES: usize = 256;
const MAX_DEVICE_TOKEN_BYTES: usize = 512;
const MAX_DEVICE_NAME_CHARS: usize = 1_024;
const MAX_PLATFORM_VERSION_CHARS: usize = 256;
const MAX_BUNDLE_ID_BYTES: usize = 512;
const MAX_NEW_COMMAND_TIMEOUT_SECONDS: u64 = 3_600;
const APPIUM_SESSION_CREATE_TIMEOUT_MS: u64 = 300_000;
pub(crate) const MAX_LOCATOR_CHARS: usize = 16 * 1024;
const MAX_LOCATOR_BYTES: usize = 64 * 1024;
const MAX_TEXT_CHARS: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_ATTRIBUTE_NAME_BYTES: usize = 256;
const MAX_ATTRIBUTE_VALUE_CHARS: usize = 64 * 1024;
const MAX_SCRIPT_CHARS: usize = 64 * 1024;
const MAX_SCRIPT_BYTES: usize = 256 * 1024;
const MAX_SCRIPT_ARGUMENTS: usize = 128;
const MAX_STATUS_MESSAGE_CHARS: usize = 4_096;
const MAX_VERSION_CHARS: usize = 256;
const MAX_RECT_ABSOLUTE_VALUE: f64 = 10_000_000.0;
const MAX_VIEWPORT_DIMENSION: u32 = 16_384;
const MAX_POINTER_COORDINATE: u32 = 1_000_000;
const MAX_DRAG_DURATION_MS: u32 = 60_000;
const W3C_ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppiumStatus {
    pub ready: bool,
    pub message: Option<String>,
    pub version: Option<String>,
    pub os_version: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AppiumSession {
    id: String,
}

impl AppiumSession {
    pub fn parse(id: impl Into<String>) -> DriverResult<Self> {
        let id = id.into();
        validate_identifier(&id, MAX_SESSION_ID_BYTES, "invalid Appium session id")?;
        Ok(Self { id })
    }

    pub fn as_str(&self) -> &str {
        &self.id
    }
}

impl std::fmt::Debug for AppiumSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppiumSession")
            .field("id", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AppiumContext {
    name: String,
}

impl AppiumContext {
    pub fn parse(name: impl Into<String>) -> DriverResult<Self> {
        let name = name.into();
        validate_identifier(&name, MAX_CONTEXT_NAME_BYTES, "invalid Appium context name")?;
        Ok(Self { name })
    }

    pub fn native() -> Self {
        Self {
            name: "NATIVE_APP".to_owned(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }

    pub fn is_native(&self) -> bool {
        self.name.eq_ignore_ascii_case("NATIVE_APP")
    }
}

impl std::fmt::Debug for AppiumContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppiumContext")
            .field("name", &"[REDACTED]")
            .field("native", &self.is_native())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AppiumElement {
    id: String,
}

impl AppiumElement {
    pub fn parse(id: impl Into<String>) -> DriverResult<Self> {
        let id = id.into();
        validate_identifier(&id, MAX_ELEMENT_ID_BYTES, "invalid Appium element id")?;
        Ok(Self { id })
    }

    pub fn as_str(&self) -> &str {
        &self.id
    }
}

impl std::fmt::Debug for AppiumElement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppiumElement")
            .field("id", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppiumLocatorStrategy {
    AccessibilityId,
    ClassName,
    CssSelector,
    Id,
    IosClassChain,
    IosPredicate,
    LinkText,
    PartialLinkText,
    TagName,
    XPath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppiumButton {
    Home,
    VolumeUp,
    VolumeDown,
}

impl AppiumButton {
    const fn as_wire(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::VolumeUp => "volumeUp",
            Self::VolumeDown => "volumeDown",
        }
    }
}

impl AppiumLocatorStrategy {
    const fn as_wire(self) -> &'static str {
        match self {
            Self::AccessibilityId => "accessibility id",
            Self::ClassName => "class name",
            Self::CssSelector => "css selector",
            Self::Id => "id",
            Self::IosClassChain => "-ios class chain",
            Self::IosPredicate => "-ios predicate string",
            Self::LinkText => "link text",
            Self::PartialLinkText => "partial link text",
            Self::TagName => "tag name",
            Self::XPath => "xpath",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppiumDrag {
    start_x: u32,
    start_y: u32,
    end_x: u32,
    end_y: u32,
    duration_ms: u32,
}

impl AppiumDrag {
    pub fn new(
        start_x: u32,
        start_y: u32,
        end_x: u32,
        end_y: u32,
        duration_ms: u32,
    ) -> DriverResult<Self> {
        validate_coordinates(&[(start_x, start_y), (end_x, end_y)])?;
        if duration_ms == 0 || duration_ms > MAX_DRAG_DURATION_MS {
            return Err(DriverError::Protocol(
                "invalid Appium drag duration".to_owned(),
            ));
        }
        Ok(Self {
            start_x,
            start_y,
            end_x,
            end_y,
            duration_ms,
        })
    }
}

/// Validated W3C capabilities for a single Appium XCUITest session.
///
/// The three mandatory capabilities cannot be replaced through
/// [`Self::with_capability`]. This keeps the transport pinned to iOS/XCUITest
/// while still permitting bounded Appium-specific tuning.
#[derive(Clone, PartialEq)]
pub struct AppiumSessionRequest {
    always_match: Map<String, Value>,
}

impl AppiumSessionRequest {
    pub fn new(device_udid: impl Into<String>) -> DriverResult<Self> {
        let device_udid = device_udid.into();
        validate_identifier(
            &device_udid,
            MAX_DEVICE_TOKEN_BYTES,
            "invalid Appium device UDID",
        )?;
        let mut always_match = Map::new();
        always_match.insert("platformName".to_owned(), Value::String("iOS".to_owned()));
        always_match.insert(
            "appium:automationName".to_owned(),
            Value::String("XCUITest".to_owned()),
        );
        always_match.insert("appium:udid".to_owned(), Value::String(device_udid));
        always_match.insert(
            "appium:includeSafariInWebviews".to_owned(),
            Value::Bool(true),
        );
        let request = Self { always_match };
        request.validate_size()?;
        Ok(request)
    }

    pub fn safari(device_udid: impl Into<String>) -> DriverResult<Self> {
        let mut request = Self::new(device_udid)?;
        request
            .always_match
            .insert("browserName".to_owned(), Value::String("Safari".to_owned()));
        request.validate_size()?;
        Ok(request)
    }

    pub fn bundle(
        device_udid: impl Into<String>,
        bundle_id: impl Into<String>,
    ) -> DriverResult<Self> {
        let bundle_id = bundle_id.into();
        validate_identifier(
            &bundle_id,
            MAX_BUNDLE_ID_BYTES,
            "invalid Appium application bundle id",
        )?;
        let mut request = Self::new(device_udid)?;
        request
            .always_match
            .insert("appium:bundleId".to_owned(), Value::String(bundle_id));
        request.validate_size()?;
        Ok(request)
    }

    pub fn with_capability(mut self, name: impl Into<String>, value: Value) -> DriverResult<Self> {
        let name = name.into();
        validate_capability_name(&name)?;
        if matches!(
            name.as_str(),
            "platformName"
                | "appium:automationName"
                | "appium:udid"
                | "browserName"
                | "appium:bundleId"
                | "appium:deviceName"
                | "appium:platformVersion"
                | "appium:webDriverAgentUrl"
                | "appium:newCommandTimeout"
                | "appium:options"
        ) {
            return Err(DriverError::Protocol(
                "reserved Appium capability must use a typed session constructor".to_owned(),
            ));
        }
        if value.is_null() {
            return Err(DriverError::Protocol(
                "Appium capability value must not be null".to_owned(),
            ));
        }
        validate_json_value(&value, MAX_REQUEST_JSON_NODES)?;
        self.always_match.insert(name, value);
        self.validate_size()?;
        Ok(self)
    }

    pub fn device_name(mut self, name: impl Into<String>) -> DriverResult<Self> {
        let name = name.into();
        validate_bounded_text(&name, MAX_DEVICE_NAME_CHARS, "invalid Appium device name")?;
        self.always_match
            .insert("appium:deviceName".to_owned(), Value::String(name));
        self.validate_size()?;
        Ok(self)
    }

    pub fn with_device_name(self, name: impl Into<String>) -> DriverResult<Self> {
        self.device_name(name)
    }

    pub fn platform_version(mut self, version: impl Into<String>) -> DriverResult<Self> {
        let version = version.into();
        validate_bounded_text(
            &version,
            MAX_PLATFORM_VERSION_CHARS,
            "invalid Appium platform version",
        )?;
        self.always_match
            .insert("appium:platformVersion".to_owned(), Value::String(version));
        self.validate_size()?;
        Ok(self)
    }

    pub fn with_platform_version(self, version: impl Into<String>) -> DriverResult<Self> {
        self.platform_version(version)
    }

    pub fn new_command_timeout_seconds(mut self, seconds: u64) -> DriverResult<Self> {
        if seconds == 0 || seconds > MAX_NEW_COMMAND_TIMEOUT_SECONDS {
            return Err(DriverError::Protocol(
                "Appium new-command timeout must be between 1 and 3600 seconds".to_owned(),
            ));
        }
        self.always_match.insert(
            "appium:newCommandTimeout".to_owned(),
            Value::Number(seconds.into()),
        );
        self.validate_size()?;
        Ok(self)
    }

    pub fn with_new_command_timeout_seconds(self, seconds: u64) -> DriverResult<Self> {
        self.new_command_timeout_seconds(seconds)
    }

    pub fn web_driver_agent_url(mut self, endpoint: &HttpEndpointConfig) -> DriverResult<Self> {
        if !endpoint.is_numeric_loopback() {
            return Err(DriverError::Protocol(
                "Appium WebDriverAgent URL must use a numeric loopback address".to_owned(),
            ));
        }
        let url = format!("http://{}{}", endpoint.authority(), endpoint.request_path());
        self.always_match
            .insert("appium:webDriverAgentUrl".to_owned(), Value::String(url));
        self.validate_size()?;
        Ok(self)
    }

    pub fn with_web_driver_agent_endpoint(
        self,
        endpoint: HttpEndpointConfig,
    ) -> DriverResult<Self> {
        self.web_driver_agent_url(&endpoint)
    }

    fn validate_size(&self) -> DriverResult<()> {
        let body = self.body();
        let encoded = serde_json::to_vec(&body).map_err(|_| {
            DriverError::Protocol("could not serialize Appium capabilities".to_owned())
        })?;
        if encoded.len() > MAX_REQUEST_BODY_BYTES {
            return Err(DriverError::Protocol(
                "Appium capabilities exceed the request limit".to_owned(),
            ));
        }
        Ok(())
    }

    fn body(&self) -> Value {
        json!({
            "capabilities": {
                "alwaysMatch": self.always_match,
                "firstMatch": [{}]
            }
        })
    }
}

impl std::fmt::Debug for AppiumSessionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppiumSessionRequest")
            .field("capability_count", &self.always_match.len())
            .field("values", &"[REDACTED]")
            .finish()
    }
}

/// Bounded Appium command surface used by the iOS Driver.
///
/// There is deliberately no generic HTTP path method. The trait includes the
/// standard W3C element operations plus Appium's context and script extensions
/// required to implement native accessibility and web DOM channels. The stock
/// Driver invokes only fixed scripts; generic script execution is an explicit
/// trusted-embedder API and is not exposed on DeviceRail's wire protocol.
/// Mutating implementations must return `Cancelled` or `TimedOut` only before
/// the request starts sending. Once bytes may have reached Appium, an
/// interrupted mutation must return the non-retryable
/// `appium_command_outcome_unknown` platform error.
#[async_trait]
pub trait AppiumTransport: Send + Sync {
    async fn status(&self, control: &ExecutionControl) -> DriverResult<AppiumStatus>;
    async fn create_session(
        &self,
        request: &AppiumSessionRequest,
        control: &ExecutionControl,
    ) -> DriverResult<AppiumSession>;
    async fn delete_session(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
    async fn contexts(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<AppiumContext>>;
    async fn current_context(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<AppiumContext>;
    async fn switch_context(
        &self,
        session: &AppiumSession,
        context: &AppiumContext,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
    async fn native_source_json(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Value>;
    async fn page_source(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<String>;
    async fn viewport(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Viewport>;
    async fn screenshot_png(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<u8>>;
    /// Capture only the current WebKit viewport.
    ///
    /// Appium's standard screenshot command returns the complete iOS display,
    /// including Safari chrome. That image cannot be mapped to DOM CSS bounds
    /// with a single scale factor. This typed operation is deliberately
    /// separate so Web contexts cannot accidentally consume a full-display
    /// screenshot as if it were a CSS viewport capture.
    async fn web_viewport_screenshot_png(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<u8>>;
    async fn execute_script(
        &self,
        session: &AppiumSession,
        script: &str,
        arguments: &[Value],
        control: &ExecutionControl,
    ) -> DriverResult<Value>;
    async fn find_element(
        &self,
        session: &AppiumSession,
        strategy: AppiumLocatorStrategy,
        value: &str,
        control: &ExecutionControl,
    ) -> DriverResult<AppiumElement>;
    async fn element_rect(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<UiRect>;
    async fn element_attribute(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        name: &str,
        control: &ExecutionControl,
    ) -> DriverResult<Option<Value>>;
    async fn element_displayed(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<bool>;
    async fn element_enabled(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<bool>;
    async fn click_element(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
    async fn clear_element(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
    async fn set_element_value(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        value: &str,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
    async fn tap_coordinate(
        &self,
        session: &AppiumSession,
        x: u32,
        y: u32,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
    async fn drag(
        &self,
        session: &AppiumSession,
        gesture: AppiumDrag,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
    async fn send_keys(
        &self,
        session: &AppiumSession,
        text: &str,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
    async fn press_button(
        &self,
        session: &AppiumSession,
        button: AppiumButton,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
}

#[derive(Clone, Debug)]
pub struct SystemAppiumTransport {
    endpoint: HttpEndpointConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestTimeoutPolicy {
    Endpoint,
    SessionCreation,
}

struct RequestOptions<'a> {
    timeout_policy: RequestTimeoutPolicy,
    request_started: Option<&'a AtomicBool>,
}

impl SystemAppiumTransport {
    pub fn new(endpoint: HttpEndpointConfig) -> Self {
        Self { endpoint }
    }

    pub fn endpoint(&self) -> &HttpEndpointConfig {
        &self.endpoint
    }

    const fn request_timeout_ms(&self, policy: RequestTimeoutPolicy) -> u64 {
        match policy {
            RequestTimeoutPolicy::Endpoint => self.endpoint.request_timeout_ms(),
            RequestTimeoutPolicy::SessionCreation => APPIUM_SESSION_CREATE_TIMEOUT_MS,
        }
    }

    async fn json_request(
        &self,
        method: Method,
        suffix: &str,
        body: Option<Value>,
        max_response_bytes: usize,
        control: &ExecutionControl,
    ) -> DriverResult<Value> {
        self.json_request_with_progress(
            method,
            suffix,
            body,
            max_response_bytes,
            control,
            RequestOptions {
                timeout_policy: RequestTimeoutPolicy::Endpoint,
                request_started: None,
            },
        )
        .await
    }

    async fn json_request_with_progress(
        &self,
        method: Method,
        suffix: &str,
        body: Option<Value>,
        max_response_bytes: usize,
        control: &ExecutionControl,
        options: RequestOptions<'_>,
    ) -> DriverResult<Value> {
        let route = self.endpoint.route(suffix)?;
        let body = body
            .map(|value| {
                validate_json_value(&value, MAX_REQUEST_JSON_NODES)?;
                serde_json::to_vec(&value).map_err(|_| {
                    DriverError::Internal("could not serialize Appium request".to_owned())
                })
            })
            .transpose()?;
        if body
            .as_ref()
            .is_some_and(|bytes| bytes.len() > MAX_REQUEST_BODY_BYTES)
        {
            return Err(platform("appium_request_too_large", false));
        }
        let response = run_controlled(
            control,
            self.request_timeout_ms(options.timeout_policy),
            "appium_transport_timeout",
            request_http(
                &self.endpoint,
                method,
                &route,
                body.as_deref(),
                max_response_bytes,
                options.request_started,
            ),
        )
        .await?;
        let root: Value = serde_json::from_slice(&response.body).map_err(|_| {
            if (200..300).contains(&response.status) {
                platform("appium_invalid_json", false)
            } else {
                platform("appium_http_status", response.status >= 500)
            }
        })?;
        if let Some(error) = webdriver_error(&root, response.status) {
            return Err(error);
        }
        if !(200..300).contains(&response.status) {
            return Err(platform("appium_http_status", response.status >= 500));
        }
        if !root.is_object() {
            return Err(platform("appium_invalid_response", false));
        }
        Ok(root)
    }

    async fn mutation_json_request(
        &self,
        method: Method,
        suffix: &str,
        body: Option<Value>,
        max_response_bytes: usize,
        control: &ExecutionControl,
    ) -> DriverResult<Value> {
        self.mutation_json_request_with_timeout(
            method,
            suffix,
            body,
            max_response_bytes,
            RequestTimeoutPolicy::Endpoint,
            control,
        )
        .await
    }

    async fn mutation_json_request_with_timeout(
        &self,
        method: Method,
        suffix: &str,
        body: Option<Value>,
        max_response_bytes: usize,
        timeout_policy: RequestTimeoutPolicy,
        control: &ExecutionControl,
    ) -> DriverResult<Value> {
        let request_started = AtomicBool::new(false);
        self.json_request_with_progress(
            method,
            suffix,
            body,
            max_response_bytes,
            control,
            RequestOptions {
                timeout_policy,
                request_started: Some(&request_started),
            },
        )
        .await
        .map_err(|error| {
            map_ambiguous_mutation_error(error, request_started.load(Ordering::Acquire))
        })
    }

    async fn null_command(
        &self,
        method: Method,
        route: &str,
        body: Option<Value>,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        let root = self
            .mutation_json_request(method, route, body, MAX_JSON_BODY_BYTES, control)
            .await?;
        if root.get("value") != Some(&Value::Null) {
            return Err(command_outcome_unknown());
        }
        Ok(())
    }

    fn session_route(&self, session: &AppiumSession, suffix: &str) -> DriverResult<String> {
        let id = encode_path_segment(
            session.as_str(),
            MAX_SESSION_ID_BYTES,
            "invalid Appium session id",
        )?;
        Ok(format!("/session/{id}{suffix}"))
    }

    fn element_route(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        suffix: &str,
    ) -> DriverResult<String> {
        let session = encode_path_segment(
            session.as_str(),
            MAX_SESSION_ID_BYTES,
            "invalid Appium session id",
        )?;
        let element = encode_path_segment(
            element.as_str(),
            MAX_ELEMENT_ID_BYTES,
            "invalid Appium element id",
        )?;
        Ok(format!("/session/{session}/element/{element}{suffix}"))
    }

    fn take_value(mut root: Value) -> DriverResult<Value> {
        root.get_mut("value")
            .map(Value::take)
            .ok_or_else(|| platform("appium_invalid_response", false))
    }

    async fn decode_screenshot_value(
        root: Value,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<u8>> {
        let encoded = Self::take_value(root)?
            .as_str()
            .filter(|encoded| !encoded.is_empty())
            .ok_or_else(|| platform("appium_invalid_screenshot", false))?
            .to_owned();
        run_bounded_blocking(
            control,
            move || {
                let bytes = BASE64
                    .decode(encoded)
                    .map_err(|_| platform("appium_invalid_screenshot", false))?;
                if bytes.len() > MAX_SCREENSHOT_BYTES
                    || !bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
                {
                    return Err(platform("appium_invalid_screenshot", false));
                }
                Ok(bytes)
            },
            || platform("appium_invalid_screenshot", false),
        )
        .await
    }
}

#[async_trait]
impl AppiumTransport for SystemAppiumTransport {
    async fn status(&self, control: &ExecutionControl) -> DriverResult<AppiumStatus> {
        let root = self
            .json_request(Method::Get, "/status", None, MAX_JSON_BODY_BYTES, control)
            .await?;
        let value = root
            .get("value")
            .and_then(Value::as_object)
            .ok_or_else(|| platform("appium_invalid_status", false))?;
        let ready = value
            .get("ready")
            .and_then(Value::as_bool)
            .ok_or_else(|| platform("appium_invalid_status", false))?;
        let message = bounded_optional_string(
            value.get("message"),
            MAX_STATUS_MESSAGE_CHARS,
            "appium_invalid_status",
        )?;
        let version = bounded_optional_string(
            value.get("build").and_then(|build| build.get("version")),
            MAX_VERSION_CHARS,
            "appium_invalid_status",
        )?;
        let os_version = bounded_optional_string(
            value.get("os").and_then(|os| os.get("version")),
            MAX_VERSION_CHARS,
            "appium_invalid_status",
        )?;
        Ok(AppiumStatus {
            ready,
            message,
            version,
            os_version,
        })
    }

    async fn create_session(
        &self,
        request: &AppiumSessionRequest,
        control: &ExecutionControl,
    ) -> DriverResult<AppiumSession> {
        request.validate_size()?;
        let root = self
            .mutation_json_request_with_timeout(
                Method::Post,
                "/session",
                Some(request.body()),
                MAX_JSON_BODY_BYTES,
                RequestTimeoutPolicy::SessionCreation,
                control,
            )
            .await?;
        let id = root
            .get("value")
            .and_then(|value| value.get("sessionId"))
            .or_else(|| root.get("sessionId"))
            .and_then(Value::as_str)
            .ok_or_else(command_outcome_unknown)?;
        AppiumSession::parse(id).map_err(|_| command_outcome_unknown())
    }

    async fn delete_session(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        self.null_command(
            Method::Delete,
            &self.session_route(session, "")?,
            None,
            control,
        )
        .await
    }

    async fn contexts(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<AppiumContext>> {
        let root = self
            .json_request(
                Method::Get,
                &self.session_route(session, "/contexts")?,
                None,
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        let values = root
            .get("value")
            .and_then(Value::as_array)
            .filter(|values| !values.is_empty() && values.len() <= MAX_CONTEXTS)
            .ok_or_else(|| platform("appium_invalid_contexts", false))?;
        let mut names = BTreeSet::new();
        let mut contexts = Vec::with_capacity(values.len());
        for value in values {
            let name = value
                .as_str()
                .ok_or_else(|| platform("appium_invalid_contexts", false))?;
            let context = AppiumContext::parse(name)
                .map_err(|_| platform("appium_invalid_contexts", false))?;
            if !names.insert(context.as_str().to_owned()) {
                return Err(platform("appium_invalid_contexts", false));
            }
            contexts.push(context);
        }
        Ok(contexts)
    }

    async fn current_context(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<AppiumContext> {
        let root = self
            .json_request(
                Method::Get,
                &self.session_route(session, "/context")?,
                None,
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        let name = root
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| platform("appium_invalid_context", false))?;
        AppiumContext::parse(name).map_err(|_| platform("appium_invalid_context", false))
    }

    async fn switch_context(
        &self,
        session: &AppiumSession,
        context: &AppiumContext,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        self.null_command(
            Method::Post,
            &self.session_route(session, "/context")?,
            Some(json!({ "name": context.as_str() })),
            control,
        )
        .await
    }

    async fn native_source_json(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Value> {
        let value = self
            .execute_script(
                session,
                "mobile: source",
                &[json!({ "format": "json" })],
                control,
            )
            .await?;
        let source = match value {
            Value::String(source) => {
                if source.len() > MAX_SOURCE_BYTES {
                    return Err(platform("appium_source_too_large", false));
                }
                serde_json::from_str(&source)
                    .map_err(|_| platform("appium_invalid_native_source", false))?
            }
            Value::Object(_) | Value::Array(_) => value,
            _ => return Err(platform("appium_invalid_native_source", false)),
        };
        let encoded = serde_json::to_vec(&source)
            .map_err(|_| platform("appium_invalid_native_source", false))?;
        if encoded.len() > MAX_SOURCE_BYTES
            || !matches!(&source, Value::Object(_) | Value::Array(_))
        {
            return Err(platform("appium_invalid_native_source", false));
        }
        Ok(source)
    }

    async fn page_source(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<String> {
        let root = self
            .json_request(
                Method::Get,
                &self.session_route(session, "/source")?,
                None,
                MAX_SOURCE_BYTES.saturating_mul(2),
                control,
            )
            .await?;
        let source = root
            .get("value")
            .and_then(Value::as_str)
            .filter(|source| source.len() <= MAX_SOURCE_BYTES)
            .ok_or_else(|| platform("appium_invalid_source", false))?;
        Ok(source.to_owned())
    }

    async fn viewport(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Viewport> {
        let root = self
            .json_request(
                Method::Get,
                &self.session_route(session, "/window/rect")?,
                None,
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        let value = root
            .get("value")
            .and_then(Value::as_object)
            .ok_or_else(|| platform("appium_invalid_viewport", false))?;
        let width = json_u32(value.get("width"), "appium_invalid_viewport")?;
        let height = json_u32(value.get("height"), "appium_invalid_viewport")?;
        if width == 0
            || height == 0
            || width > MAX_VIEWPORT_DIMENSION
            || height > MAX_VIEWPORT_DIMENSION
        {
            return Err(platform("appium_invalid_viewport", false));
        }
        Ok(Viewport {
            width,
            height,
            scale_factor: 1.0,
        })
    }

    async fn screenshot_png(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<u8>> {
        let root = self
            .json_request(
                Method::Get,
                &self.session_route(session, "/screenshot")?,
                None,
                MAX_SCREENSHOT_BODY_BYTES,
                control,
            )
            .await?;
        Self::decode_screenshot_value(root, control).await
    }

    async fn web_viewport_screenshot_png(
        &self,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<u8>> {
        // XCUITest's typed extension delegates to WebKit's viewport capture in
        // a web context. Keep the fixed script inside the transport instead of
        // exposing arbitrary script execution on DeviceRail's wire protocol.
        let root = self
            .json_request(
                Method::Post,
                &self.session_route(session, "/execute/sync")?,
                Some(json!({
                    "script": "mobile: viewportScreenshot",
                    "args": []
                })),
                MAX_SCREENSHOT_BODY_BYTES,
                control,
            )
            .await?;
        Self::decode_screenshot_value(root, control).await
    }

    async fn execute_script(
        &self,
        session: &AppiumSession,
        script: &str,
        arguments: &[Value],
        control: &ExecutionControl,
    ) -> DriverResult<Value> {
        if script.trim().is_empty()
            || script.len() > MAX_SCRIPT_BYTES
            || script.chars().count() > MAX_SCRIPT_CHARS
            || arguments.len() > MAX_SCRIPT_ARGUMENTS
        {
            return Err(DriverError::Protocol(
                "invalid Appium script request".to_owned(),
            ));
        }
        let mut argument_nodes = 0_usize;
        for argument in arguments {
            let remaining = MAX_REQUEST_JSON_NODES
                .checked_sub(argument_nodes)
                .ok_or_else(|| {
                    DriverError::Protocol(
                        "Appium request JSON exceeds its complexity limit".to_owned(),
                    )
                })?;
            argument_nodes = argument_nodes
                .checked_add(validate_json_value(argument, remaining)?)
                .ok_or_else(|| {
                    DriverError::Protocol(
                        "Appium request JSON exceeds its complexity limit".to_owned(),
                    )
                })?;
        }
        let root = self
            .json_request(
                Method::Post,
                &self.session_route(session, "/execute/sync")?,
                Some(json!({ "script": script, "args": arguments })),
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        Self::take_value(root)
    }

    async fn find_element(
        &self,
        session: &AppiumSession,
        strategy: AppiumLocatorStrategy,
        value: &str,
        control: &ExecutionControl,
    ) -> DriverResult<AppiumElement> {
        validate_locator(value)?;
        let root = self
            .json_request(
                Method::Post,
                &self.session_route(session, "/element")?,
                Some(json!({ "using": strategy.as_wire(), "value": value })),
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        let value = root
            .get("value")
            .and_then(Value::as_object)
            .ok_or_else(|| platform("appium_invalid_element", false))?;
        let id = value
            .get(W3C_ELEMENT_KEY)
            .or_else(|| value.get("ELEMENT"))
            .and_then(Value::as_str)
            .ok_or_else(|| platform("appium_invalid_element", false))?;
        AppiumElement::parse(id).map_err(|_| platform("appium_invalid_element", false))
    }

    async fn element_rect(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<UiRect> {
        let root = self
            .json_request(
                Method::Get,
                &self.element_route(session, element, "/rect")?,
                None,
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        let value = root
            .get("value")
            .and_then(Value::as_object)
            .ok_or_else(|| platform("appium_invalid_element_rect", false))?;
        let rect = UiRect {
            x: json_f64(value.get("x"), "appium_invalid_element_rect")?,
            y: json_f64(value.get("y"), "appium_invalid_element_rect")?,
            width: json_f64(value.get("width"), "appium_invalid_element_rect")?,
            height: json_f64(value.get("height"), "appium_invalid_element_rect")?,
        };
        if !rect.is_valid()
            || rect.x.abs() > MAX_RECT_ABSOLUTE_VALUE
            || rect.y.abs() > MAX_RECT_ABSOLUTE_VALUE
            || rect.width > MAX_RECT_ABSOLUTE_VALUE
            || rect.height > MAX_RECT_ABSOLUTE_VALUE
        {
            return Err(platform("appium_invalid_element_rect", false));
        }
        Ok(rect)
    }

    async fn element_attribute(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        name: &str,
        control: &ExecutionControl,
    ) -> DriverResult<Option<Value>> {
        let name = encode_path_segment(
            name,
            MAX_ATTRIBUTE_NAME_BYTES,
            "invalid Appium attribute name",
        )?;
        let route = self.element_route(session, element, &format!("/attribute/{name}"))?;
        let root = self
            .json_request(Method::Get, &route, None, MAX_JSON_BODY_BYTES, control)
            .await?;
        let value = Self::take_value(root)?;
        match value {
            Value::Null => Ok(None),
            Value::String(ref text) if text.chars().count() <= MAX_ATTRIBUTE_VALUE_CHARS => {
                Ok(Some(value))
            }
            Value::Bool(_) | Value::Number(_) => Ok(Some(value)),
            _ => Err(platform("appium_invalid_element_attribute", false)),
        }
    }

    async fn element_displayed(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<bool> {
        self.element_bool(session, element, "/displayed", control)
            .await
    }

    async fn element_enabled(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<bool> {
        self.element_bool(session, element, "/enabled", control)
            .await
    }

    async fn click_element(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        self.null_command(
            Method::Post,
            &self.element_route(session, element, "/click")?,
            Some(json!({})),
            control,
        )
        .await
    }

    async fn clear_element(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        self.null_command(
            Method::Post,
            &self.element_route(session, element, "/clear")?,
            Some(json!({})),
            control,
        )
        .await
    }

    async fn set_element_value(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        value: &str,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        validate_text(value)?;
        self.null_command(
            Method::Post,
            &self.element_route(session, element, "/value")?,
            Some(json!({ "text": value })),
            control,
        )
        .await
    }

    async fn tap_coordinate(
        &self,
        session: &AppiumSession,
        x: u32,
        y: u32,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        validate_coordinates(&[(x, y)])?;
        self.null_command(
            Method::Post,
            &self.session_route(session, "/actions")?,
            Some(json!({
                "actions": [{
                    "type": "pointer",
                    "id": "devicerail-finger",
                    "parameters": { "pointerType": "touch" },
                    "actions": [
                        { "type": "pointerMove", "duration": 0, "origin": "viewport", "x": x, "y": y },
                        { "type": "pointerDown", "button": 0 },
                        { "type": "pause", "duration": 50 },
                        { "type": "pointerUp", "button": 0 }
                    ]
                }]
            })),
            control,
        )
        .await
    }

    async fn drag(
        &self,
        session: &AppiumSession,
        gesture: AppiumDrag,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        let AppiumDrag {
            start_x,
            start_y,
            end_x,
            end_y,
            duration_ms,
        } = gesture;
        self.null_command(
            Method::Post,
            &self.session_route(session, "/actions")?,
            Some(json!({
                "actions": [{
                    "type": "pointer",
                    "id": "devicerail-finger",
                    "parameters": { "pointerType": "touch" },
                    "actions": [
                        { "type": "pointerMove", "duration": 0, "origin": "viewport", "x": start_x, "y": start_y },
                        { "type": "pointerDown", "button": 0 },
                        { "type": "pause", "duration": 50 },
                        { "type": "pointerMove", "duration": duration_ms, "origin": "viewport", "x": end_x, "y": end_y },
                        { "type": "pointerUp", "button": 0 }
                    ]
                }]
            })),
            control,
        )
        .await
    }

    async fn send_keys(
        &self,
        session: &AppiumSession,
        text: &str,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        validate_text(text)?;
        let root = self
            .json_request(
                Method::Get,
                &self.session_route(session, "/element/active")?,
                None,
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        let element = parse_element_response(&root)?;
        self.set_element_value(session, &element, text, control)
            .await
    }

    async fn press_button(
        &self,
        session: &AppiumSession,
        button: AppiumButton,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        self.null_command(
            Method::Post,
            &self.session_route(session, "/execute/sync")?,
            Some(json!({
                "script": "mobile: pressButton",
                "args": [{ "name": button.as_wire() }]
            })),
            control,
        )
        .await
    }
}

impl SystemAppiumTransport {
    async fn element_bool(
        &self,
        session: &AppiumSession,
        element: &AppiumElement,
        suffix: &str,
        control: &ExecutionControl,
    ) -> DriverResult<bool> {
        let root = self
            .json_request(
                Method::Get,
                &self.element_route(session, element, suffix)?,
                None,
                MAX_JSON_BODY_BYTES,
                control,
            )
            .await?;
        root.get("value")
            .and_then(Value::as_bool)
            .ok_or_else(|| platform("appium_invalid_element_state", false))
    }
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

async fn request_http(
    endpoint: &HttpEndpointConfig,
    method: Method,
    route: &str,
    body: Option<&[u8]>,
    max_response_bytes: usize,
    request_started: Option<&AtomicBool>,
) -> DriverResult<HttpResponse> {
    let mut stream = TcpStream::connect((endpoint.host(), endpoint.port()))
        .await
        .map_err(|_| platform("appium_connect_failed", true))?;
    let body = body.unwrap_or_default();
    let request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        method.as_str(),
        route,
        endpoint.authority(),
        body.len()
    );
    if let Some(request_started) = request_started {
        // Once the first socket write is attempted, dropping this future can no
        // longer prove that Appium did not receive and execute the command.
        request_started.store(true, Ordering::Release);
    }
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| platform("appium_write_failed", true))?;
    if !body.is_empty() {
        stream
            .write_all(body)
            .await
            .map_err(|_| platform("appium_write_failed", true))?;
    }
    read_http_response(&mut stream, max_response_bytes).await
}

async fn read_http_response(
    stream: &mut TcpStream,
    max_body_bytes: usize,
) -> DriverResult<HttpResponse> {
    let (status, headers, mut body) = read_http_head(stream).await?;
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        let encoded_limit = max_body_bytes
            .checked_mul(2)
            .and_then(|value| value.checked_add(MAX_HEADER_BYTES))
            .ok_or_else(|| platform("http_body_too_large", false))?;
        read_to_end_bounded(stream, &mut body, encoded_limit).await?;
        body = decode_chunked(&body, max_body_bytes)?;
    } else if let Some(content_length) = headers.get("content-length") {
        let length = content_length
            .parse::<usize>()
            .ok()
            .filter(|length| *length <= max_body_bytes)
            .ok_or_else(|| platform("http_invalid_content_length", false))?;
        while body.len() < length {
            read_more_bounded(stream, &mut body, length).await?;
        }
        body.truncate(length);
    } else {
        read_to_end_bounded(stream, &mut body, max_body_bytes).await?;
    }
    if body.len() > max_body_bytes {
        return Err(platform("http_body_too_large", false));
    }
    Ok(HttpResponse { status, body })
}

async fn read_http_head(
    stream: &mut TcpStream,
) -> DriverResult<(u16, BTreeMap<String, String>, Vec<u8>)> {
    let mut bytes = Vec::with_capacity(4 * 1024);
    let header_end = loop {
        if let Some(index) = find_subslice(&bytes, b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err(platform("http_headers_too_large", false));
        }
        let remaining = (MAX_HEADER_BYTES - bytes.len()).min(4 * 1024);
        let mut chunk = vec![0_u8; remaining];
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|_| platform("appium_read_failed", true))?;
        if read == 0 {
            return Err(platform("http_truncated_headers", true));
        }
        bytes.extend_from_slice(&chunk[..read]);
    };
    let head = std::str::from_utf8(&bytes[..header_end - 4])
        .map_err(|_| platform("http_invalid_headers", false))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| platform("http_invalid_headers", false))?;
    let mut status_fields = status_line.split_whitespace();
    let version = status_fields.next().unwrap_or_default();
    let status = status_fields
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| (100..=599).contains(value))
        .ok_or_else(|| platform("http_invalid_headers", false))?;
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(platform("http_invalid_headers", false));
    }
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| platform("http_invalid_headers", false))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        if name.is_empty() || headers.insert(name, value).is_some() {
            return Err(platform("http_invalid_headers", false));
        }
    }
    Ok((status, headers, bytes[header_end..].to_vec()))
}

async fn read_more_bounded(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    max_bytes: usize,
) -> DriverResult<()> {
    if bytes.len() >= max_bytes {
        return Err(platform("http_body_too_large", false));
    }
    let remaining = (max_bytes - bytes.len()).min(16 * 1024);
    let mut chunk = vec![0_u8; remaining];
    let read = stream
        .read(&mut chunk)
        .await
        .map_err(|_| platform("appium_read_failed", true))?;
    if read == 0 {
        return Err(platform("http_truncated_body", true));
    }
    bytes.extend_from_slice(&chunk[..read]);
    Ok(())
}

async fn read_to_end_bounded(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    max_bytes: usize,
) -> DriverResult<()> {
    loop {
        if bytes.len() > max_bytes {
            return Err(platform("http_body_too_large", false));
        }
        if bytes.len() == max_bytes {
            let mut probe = [0_u8; 1];
            let read = stream
                .read(&mut probe)
                .await
                .map_err(|_| platform("appium_read_failed", true))?;
            return if read == 0 {
                Ok(())
            } else {
                Err(platform("http_body_too_large", false))
            };
        }
        let remaining = (max_bytes - bytes.len()).min(16 * 1024);
        let mut chunk = vec![0_u8; remaining];
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|_| platform("appium_read_failed", true))?;
        if read == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

fn decode_chunked(bytes: &[u8], max_body_bytes: usize) -> DriverResult<Vec<u8>> {
    let mut cursor = 0_usize;
    let mut output = Vec::new();
    loop {
        let line_end = find_subslice(
            bytes
                .get(cursor..)
                .ok_or_else(|| platform("http_invalid_chunked_body", false))?,
            b"\r\n",
        )
        .map(|index| cursor + index)
        .ok_or_else(|| platform("http_invalid_chunked_body", false))?;
        let size_text = std::str::from_utf8(&bytes[cursor..line_end])
            .ok()
            .and_then(|line| line.split(';').next())
            .ok_or_else(|| platform("http_invalid_chunked_body", false))?;
        let size = usize::from_str_radix(size_text.trim(), 16)
            .map_err(|_| platform("http_invalid_chunked_body", false))?;
        cursor = line_end + 2;
        if size == 0 {
            return Ok(output);
        }
        let end = cursor
            .checked_add(size)
            .filter(|end| end.checked_add(2).is_some_and(|tail| tail <= bytes.len()))
            .ok_or_else(|| platform("http_invalid_chunked_body", false))?;
        if &bytes[end..end + 2] != b"\r\n"
            || output
                .len()
                .checked_add(size)
                .is_none_or(|length| length > max_body_bytes)
        {
            return Err(platform("http_invalid_chunked_body", false));
        }
        output.extend_from_slice(&bytes[cursor..end]);
        cursor = end + 2;
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        Some(0)
    } else {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}

fn webdriver_error(root: &Value, status: u16) -> Option<DriverError> {
    let error = root
        .get("value")
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)?;
    Some(match error {
        "no such element" => DriverError::ElementNotFound,
        "stale element reference" => DriverError::ElementStale,
        "element not interactable" | "element click intercepted" => {
            DriverError::ElementNotInteractable
        }
        "no such context" | "no such window" => DriverError::UiContextNotFound,
        "invalid argument" | "invalid selector" => {
            DriverError::Protocol("Appium rejected an invalid WebDriver argument".to_owned())
        }
        "invalid session id" => platform("appium_invalid_session", false),
        "session not created" => platform("appium_session_not_created", true),
        "timeout" | "script timeout" => platform("appium_webdriver_timeout", true),
        "unknown command" | "unsupported operation" => {
            platform("appium_unsupported_command", false)
        }
        _ => platform("appium_webdriver_error", status >= 500),
    })
}

fn map_ambiguous_mutation_error(error: DriverError, request_started: bool) -> DriverError {
    if !request_started {
        return error;
    }
    match &error {
        DriverError::Cancelled | DriverError::TimedOut => command_outcome_unknown(),
        DriverError::Platform { code, .. }
            if matches!(
                code.as_str(),
                "appium_write_failed"
                    | "appium_read_failed"
                    | "appium_transport_timeout"
                    | "appium_invalid_json"
                    | "appium_invalid_response"
                    | "http_truncated_headers"
                    | "http_truncated_body"
                    | "http_headers_too_large"
                    | "http_invalid_headers"
                    | "http_invalid_content_length"
                    | "http_invalid_chunked_body"
                    | "http_body_too_large"
            ) =>
        {
            command_outcome_unknown()
        }
        _ => error,
    }
}

fn command_outcome_unknown() -> DriverError {
    platform("appium_command_outcome_unknown", false)
}

fn validate_identifier(value: &str, max_bytes: usize, message: &'static str) -> DriverResult<()> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(DriverError::Protocol(message.to_owned()));
    }
    Ok(())
}

fn validate_bounded_text(value: &str, max_chars: usize, message: &'static str) -> DriverResult<()> {
    if value.trim().is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
    {
        return Err(DriverError::Protocol(message.to_owned()));
    }
    Ok(())
}

fn validate_capability_name(name: &str) -> DriverResult<()> {
    if name.is_empty()
        || name.len() > MAX_CAPABILITY_NAME_BYTES
        || name.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
        })
    {
        return Err(DriverError::Protocol(
            "invalid Appium capability name".to_owned(),
        ));
    }
    Ok(())
}

fn validate_locator(value: &str) -> DriverResult<()> {
    if value.trim().is_empty()
        || value.len() > MAX_LOCATOR_BYTES
        || value.chars().count() > MAX_LOCATOR_CHARS
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\t' | '\n' | '\r'))
    {
        return Err(DriverError::Protocol(
            "invalid Appium element locator".to_owned(),
        ));
    }
    Ok(())
}

fn validate_text(value: &str) -> DriverResult<()> {
    if value.len() > MAX_TEXT_BYTES || value.chars().count() > MAX_TEXT_CHARS {
        return Err(DriverError::Protocol(
            "Appium element value exceeds the input limit".to_owned(),
        ));
    }
    Ok(())
}

fn validate_coordinates(points: &[(u32, u32)]) -> DriverResult<()> {
    if points
        .iter()
        .any(|(x, y)| *x > MAX_POINTER_COORDINATE || *y > MAX_POINTER_COORDINATE)
    {
        return Err(DriverError::Protocol(
            "Appium pointer coordinate exceeds the transport limit".to_owned(),
        ));
    }
    Ok(())
}

fn validate_json_value(value: &Value, max_nodes: usize) -> DriverResult<usize> {
    if max_nodes == 0 {
        return Err(DriverError::Protocol(
            "Appium request JSON exceeds its complexity limit".to_owned(),
        ));
    }
    let mut stack = vec![(value, 0_usize)];
    let mut nodes = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        nodes = nodes.checked_add(1).ok_or_else(|| {
            DriverError::Protocol("Appium request JSON exceeds its complexity limit".to_owned())
        })?;
        if nodes > max_nodes || depth > MAX_REQUEST_JSON_DEPTH {
            return Err(DriverError::Protocol(
                "Appium request JSON exceeds its complexity limit".to_owned(),
            ));
        }
        match value {
            Value::Array(values) => {
                for child in values {
                    push_json_child(&mut stack, nodes, max_nodes, child, depth + 1)?;
                }
            }
            Value::Object(values) => {
                for child in values.values() {
                    push_json_child(&mut stack, nodes, max_nodes, child, depth + 1)?;
                }
            }
            _ => {}
        }
    }
    Ok(nodes)
}

fn push_json_child<'a>(
    stack: &mut Vec<(&'a Value, usize)>,
    processed_nodes: usize,
    max_nodes: usize,
    child: &'a Value,
    depth: usize,
) -> DriverResult<()> {
    if processed_nodes
        .checked_add(stack.len())
        .is_none_or(|pending| pending >= max_nodes)
    {
        return Err(DriverError::Protocol(
            "Appium request JSON exceeds its complexity limit".to_owned(),
        ));
    }
    stack.push((child, depth));
    Ok(())
}

fn encode_path_segment(
    value: &str,
    max_bytes: usize,
    message: &'static str,
) -> DriverResult<String> {
    validate_identifier(value, max_bytes, message)?;
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    Ok(encoded)
}

fn bounded_optional_string(
    value: Option<&Value>,
    max_chars: usize,
    error_code: &'static str,
) -> DriverResult<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if value.chars().count() <= max_chars && !value.chars().any(char::is_control) =>
        {
            Ok(Some(value.clone()))
        }
        _ => Err(platform(error_code, false)),
    }
}

fn json_f64(value: Option<&Value>, error_code: &'static str) -> DriverResult<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| platform(error_code, false))
}

fn json_u32(value: Option<&Value>, error_code: &'static str) -> DriverResult<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| platform(error_code, false))
}

fn parse_element_response(root: &Value) -> DriverResult<AppiumElement> {
    let value = root
        .get("value")
        .and_then(Value::as_object)
        .ok_or_else(|| platform("appium_invalid_element", false))?;
    let id = value
        .get(W3C_ELEMENT_KEY)
        .or_else(|| value.get("ELEMENT"))
        .and_then(Value::as_str)
        .ok_or_else(|| platform("appium_invalid_element", false))?;
    AppiumElement::parse(id).map_err(|_| platform("appium_invalid_element", false))
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc, time::Duration};

    use devicerail_core::{
        CancellationReason, DeviceDriver, DriverError, ExecutionControl, ExecutionController,
        TimeoutScope,
    };
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::{
        AppiumButton, AppiumContext, AppiumLocatorStrategy, AppiumSessionRequest, AppiumTransport,
        SystemAppiumTransport,
    };
    use crate::{AppiumIosDriver, HttpEndpointConfig, IosDeviceConfig};

    struct ExpectedResponse {
        method: &'static str,
        route: &'static str,
        body: Option<Value>,
        status: u16,
        response: Value,
    }

    async fn mock_transport(
        responses: Vec<ExpectedResponse>,
    ) -> (SystemAppiumTransport, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let mut responses = VecDeque::from(responses);
            while let Some(expected) = responses.pop_front() {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let (head, body) = read_request(&mut socket).await;
                assert!(
                    head.starts_with(&format!(
                        "{} {} HTTP/1.1\r\n",
                        expected.method, expected.route
                    )),
                    "unexpected request head: {head}"
                );
                let actual_body = if body.is_empty() {
                    None
                } else {
                    Some(serde_json::from_slice::<Value>(&body).expect("JSON request"))
                };
                assert_eq!(actual_body, expected.body);
                write_json_response(&mut socket, expected.status, &expected.response)
                    .await
                    .expect("write response");
            }
        });
        let endpoint =
            HttpEndpointConfig::new(format!("http://{address}/wd/hub")).expect("endpoint");
        (SystemAppiumTransport::new(endpoint), server)
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
        let mut request = Vec::new();
        let header_end = loop {
            if let Some(index) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                break index + 4;
            }
            let mut chunk = [0_u8; 1024];
            let read = socket.read(&mut chunk).await.expect("read request");
            assert_ne!(read, 0, "request ended before headers");
            request.extend_from_slice(&chunk[..read]);
        };
        let head = std::str::from_utf8(&request[..header_end])
            .expect("request headers")
            .to_owned();
        let content_length = head
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length: ")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .expect("content length");
        while request.len() < header_end + content_length {
            let mut chunk = [0_u8; 1024];
            let read = socket.read(&mut chunk).await.expect("read request body");
            assert_ne!(read, 0, "request ended before body");
            request.extend_from_slice(&chunk[..read]);
        }
        (
            head,
            request[header_end..header_end + content_length].to_vec(),
        )
    }

    async fn write_json_response(
        socket: &mut tokio::net::TcpStream,
        status: u16,
        response: &Value,
    ) -> std::io::Result<()> {
        let encoded = serde_json::to_vec(response).expect("response JSON");
        socket
            .write_all(
                format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    encoded.len()
                )
                .as_bytes(),
            )
            .await?;
        socket.write_all(&encoded).await
    }

    #[test]
    fn session_requests_are_xcuitest_scoped_bounded_and_redacted() {
        let request = AppiumSessionRequest::safari("00008101-device")
            .expect("Safari request")
            .with_capability("appium:usePreinstalledWDA", Value::Bool(true))
            .expect("additional capability")
            .with_new_command_timeout_seconds(600)
            .expect("bounded new-command timeout");
        assert_eq!(
            request
                .body()
                .pointer("/capabilities/alwaysMatch/appium:newCommandTimeout"),
            Some(&json!(600))
        );
        let debug = format!("{request:?}");
        assert!(debug.contains("capability_count"));
        assert!(!debug.contains("00008101-device"));
        assert!(
            request
                .clone()
                .with_capability("appium:udid", Value::String("other".to_owned()))
                .is_err()
        );
        assert!(
            request
                .clone()
                .with_capability("appium:newCommandTimeout", json!(600))
                .is_err()
        );
        assert!(
            AppiumSessionRequest::new("phone-1")
                .expect("base request")
                .with_new_command_timeout_seconds(0)
                .is_err()
        );
        assert!(
            AppiumSessionRequest::new("phone-1")
                .expect("base request")
                .with_new_command_timeout_seconds(3_601)
                .is_err()
        );
        assert!(
            request
                .clone()
                .with_capability(
                    "appium:options",
                    json!({
                        "automationName": "OtherAutomation",
                        "udid": "other-device",
                        "processArguments": {
                            "env": {"nested": {"appium:options": {"udid": "third-device"}}}
                        }
                    }),
                )
                .is_err()
        );
        assert!(AppiumSessionRequest::new("\n").is_err());
        assert!(
            request
                .with_capability("invalid capability", Value::Bool(true))
                .is_err()
        );
        let mut deeply_nested = Value::Bool(true);
        for _ in 0..=super::MAX_REQUEST_JSON_DEPTH {
            deeply_nested = Value::Array(vec![deeply_nested]);
        }
        assert!(
            AppiumSessionRequest::new("phone-1")
                .expect("base request")
                .with_capability("appium:settings", deeply_nested)
                .is_err()
        );
    }

    #[tokio::test]
    async fn invalid_session_id_is_replaced_before_the_next_operation() {
        let request = AppiumSessionRequest::new("phone-1")
            .expect("base request")
            .with_new_command_timeout_seconds(600)
            .expect("bounded timeout");
        let request_body = request.body();
        let (transport, server) = mock_transport(vec![
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/status",
                body: None,
                status: 200,
                response: json!({"value":{"ready":true}}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session",
                body: Some(request_body.clone()),
                status: 200,
                response: json!({"value":{"sessionId":"session-1","capabilities":{}}}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session-1/context",
                body: None,
                status: 404,
                response: json!({"value":{"error":"invalid session id","message":"expired"}}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/status",
                body: None,
                status: 200,
                response: json!({"value":{"ready":true}}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session",
                body: Some(request_body),
                status: 200,
                response: json!({"value":{"sessionId":"session-2","capabilities":{}}}),
            },
        ])
        .await;
        let config =
            IosDeviceConfig::new("phone-1", "Recovery phone", None).expect("device config");
        let driver = AppiumIosDriver::new(config, Arc::new(transport), request);
        let (_controller, control) = ExecutionController::new();

        assert!(
            driver
                .connect(&control)
                .await
                .expect("initial connect")
                .connected
        );
        assert!(
            driver
                .connect(&control)
                .await
                .expect("replace expired session")
                .connected
        );
        server.await.expect("mock Appium server");
    }

    #[test]
    fn ambiguous_mutation_transport_failures_are_non_retryable_unknown_outcomes() {
        for code in [
            "appium_write_failed",
            "appium_read_failed",
            "appium_transport_timeout",
            "appium_invalid_json",
            "appium_invalid_response",
            "http_truncated_headers",
            "http_truncated_body",
            "http_headers_too_large",
            "http_invalid_headers",
            "http_invalid_content_length",
            "http_invalid_chunked_body",
            "http_body_too_large",
        ] {
            assert!(matches!(
                super::map_ambiguous_mutation_error(
                    DriverError::Platform {
                        code: code.to_owned(),
                        retryable: true,
                    },
                    true,
                ),
                DriverError::Platform { code, retryable: false }
                    if code == "appium_command_outcome_unknown"
            ));
        }

        assert!(matches!(
            super::map_ambiguous_mutation_error(
                DriverError::Platform {
                    code: "appium_connect_failed".to_owned(),
                    retryable: true,
                },
                false,
            ),
            DriverError::Platform { code, retryable: true }
                if code == "appium_connect_failed"
        ));
        assert!(matches!(
            super::map_ambiguous_mutation_error(DriverError::Cancelled, false),
            DriverError::Cancelled
        ));
        assert!(matches!(
            super::map_ambiguous_mutation_error(DriverError::TimedOut, false),
            DriverError::TimedOut
        ));
        for error in [DriverError::Cancelled, DriverError::TimedOut] {
            assert!(matches!(
                super::map_ambiguous_mutation_error(error, true),
                DriverError::Platform { code, retryable: false }
                    if code == "appium_command_outcome_unknown"
            ));
        }
    }

    #[tokio::test]
    async fn mutation_control_errors_follow_the_socket_send_phase() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let endpoint =
            HttpEndpointConfig::new(format!("http://{address}/wd/hub")).expect("endpoint");
        let transport = SystemAppiumTransport::new(endpoint);
        let session = super::AppiumSession::parse("session-1").expect("session");

        let (pre_controller, pre_control) = ExecutionController::new();
        assert!(pre_controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            transport
                .press_button(&session, AppiumButton::Home, &pre_control)
                .await,
            Err(DriverError::Cancelled)
        ));

        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let _ = read_request(&mut socket).await;
            received_tx.send(()).expect("signal request received");
            std::future::pending::<()>().await;
        });
        let (controller, control) = ExecutionController::new();
        let mutation = transport.press_button(&session, AppiumButton::Home, &control);
        tokio::pin!(mutation);
        tokio::select! {
            received = received_rx => received.expect("request signal"),
            result = &mut mutation => panic!("mutation completed before cancellation: {result:?}"),
        }
        assert!(controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            mutation.await,
            Err(DriverError::Platform { code, retryable: false })
                if code == "appium_command_outcome_unknown"
        ));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn session_creation_has_a_dedicated_timeout_but_status_keeps_endpoint_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let endpoint = HttpEndpointConfig::new(format!("http://{address}/wd/hub"))
            .expect("endpoint")
            .with_request_timeout_ms(50)
            .expect("short endpoint timeout");
        let transport = SystemAppiumTransport::new(endpoint);
        assert_eq!(
            transport.request_timeout_ms(super::RequestTimeoutPolicy::SessionCreation),
            super::APPIUM_SESSION_CREATE_TIMEOUT_MS
        );
        assert_eq!(
            transport.request_timeout_ms(super::RequestTimeoutPolicy::Endpoint),
            50
        );

        let server = tokio::spawn(async move {
            let (mut create_socket, _) = listener.accept().await.expect("accept create");
            let (create_head, _) = read_request(&mut create_socket).await;
            assert!(create_head.starts_with("POST /wd/hub/session HTTP/1.1\r\n"));
            tokio::time::sleep(Duration::from_millis(150)).await;
            write_json_response(
                &mut create_socket,
                200,
                &json!({"value":{"sessionId":"session-created"}}),
            )
            .await
            .expect("write create response");

            let (mut status_socket, _) = listener.accept().await.expect("accept status");
            let (status_head, _) = read_request(&mut status_socket).await;
            assert!(status_head.starts_with("GET /wd/hub/status HTTP/1.1\r\n"));
            std::future::pending::<()>().await;
        });

        let request = AppiumSessionRequest::safari("simulator-udid").expect("request");
        let session = tokio::time::timeout(
            Duration::from_secs(2),
            transport.create_session(&request, &ExecutionControl::unbounded()),
        )
        .await
        .expect("create did not use the endpoint timeout")
        .expect("create session");
        assert_eq!(session.as_str(), "session-created");

        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(1),
                transport.status(&ExecutionControl::unbounded())
            )
            .await
            .expect("status must retain its endpoint timeout"),
            Err(DriverError::Platform { code, retryable: true })
                if code == "appium_transport_timeout"
        ));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn caller_deadline_wins_after_session_request_is_sent() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let endpoint = HttpEndpointConfig::new(format!("http://{address}/wd/hub"))
            .expect("endpoint")
            .with_request_timeout_ms(5_000)
            .expect("endpoint timeout");
        let transport = SystemAppiumTransport::new(endpoint);
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let _ = read_request(&mut socket).await;
            received_tx.send(()).expect("signal request received");
            std::future::pending::<()>().await;
        });

        let (_, control) = ExecutionController::with_timeout(500, TimeoutScope::Request);
        let request = AppiumSessionRequest::safari("simulator-udid").expect("request");
        let creation = transport.create_session(&request, &control);
        tokio::pin!(creation);
        tokio::select! {
            received = received_rx => received.expect("request signal"),
            result = &mut creation => panic!("creation completed before request was received: {result:?}"),
        }
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(1_500), &mut creation)
                .await
                .expect("caller deadline did not win"),
            Err(DriverError::Platform { code, retryable: false })
                if code == "appium_command_outcome_unknown"
        ));
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn chunked_response_decoder_is_strict_and_bounded() {
        assert_eq!(
            super::decode_chunked(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n", 9).expect("decode"),
            b"Wikipedia"
        );
        assert!(super::decode_chunked(b"4\r\nWiki0\r\n\r\n", 32).is_err());
        assert!(super::decode_chunked(b"a\r\n0123456789\r\n0\r\n\r\n", 9).is_err());
    }

    #[tokio::test]
    async fn session_context_source_and_screenshot_commands_use_w3c_routes() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
        let responses = vec![
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/status",
                body: None,
                status: 200,
                response: json!({"value":{"ready":true,"message":"ready","build":{"version":"3.0"}}}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session",
                body: Some(
                    json!({"capabilities":{"alwaysMatch":{"platformName":"iOS","appium:automationName":"XCUITest","appium:udid":"phone-1","appium:includeSafariInWebviews":true,"browserName":"Safari"},"firstMatch":[{}]}}),
                ),
                status: 200,
                response: json!({"value":{"sessionId":"session/1","capabilities":{}}}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session%2F1/contexts",
                body: None,
                status: 200,
                response: json!({"value":["NATIVE_APP","WEBVIEW_1"]}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session%2F1/context",
                body: None,
                status: 200,
                response: json!({"value":"NATIVE_APP"}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session%2F1/context",
                body: Some(json!({"name":"WEBVIEW_1"})),
                status: 200,
                response: json!({"value":null}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session%2F1/execute/sync",
                body: Some(json!({"script":"mobile: source","args":[{"format":"json"}]})),
                status: 200,
                response: json!({"value":"{\"type\":\"Application\",\"children\":[]}"}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session%2F1/source",
                body: None,
                status: 200,
                response: json!({"value":"<html></html>"}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session%2F1/window/rect",
                body: None,
                status: 200,
                response: json!({"value":{"x":0,"y":0,"width":390,"height":844}}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session%2F1/screenshot",
                body: None,
                status: 200,
                response: json!({"value":base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png)}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session%2F1/execute/sync",
                body: Some(json!({"script":"mobile: viewportScreenshot","args":[]})),
                status: 200,
                response: json!({"value":base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png)}),
            },
            ExpectedResponse {
                method: "DELETE",
                route: "/wd/hub/session/session%2F1",
                body: None,
                status: 200,
                response: json!({"value":null}),
            },
        ];
        let (transport, server) = mock_transport(responses).await;
        let control = ExecutionControl::unbounded();
        let status = transport.status(&control).await.expect("status");
        assert!(status.ready);
        assert_eq!(status.version.as_deref(), Some("3.0"));
        let request = AppiumSessionRequest::safari("phone-1").expect("request");
        let session = transport
            .create_session(&request, &control)
            .await
            .expect("session");
        let contexts = transport
            .contexts(&session, &control)
            .await
            .expect("contexts");
        assert_eq!(contexts.len(), 2);
        assert!(contexts[0].is_native());
        assert!(
            transport
                .current_context(&session, &control)
                .await
                .expect("current context")
                .is_native()
        );
        transport
            .switch_context(
                &session,
                &AppiumContext::parse("WEBVIEW_1").expect("context"),
                &control,
            )
            .await
            .expect("switch context");
        assert_eq!(
            transport
                .native_source_json(&session, &control)
                .await
                .expect("native source"),
            json!({"type":"Application","children":[]})
        );
        assert_eq!(
            transport
                .page_source(&session, &control)
                .await
                .expect("page source"),
            "<html></html>"
        );
        assert_eq!(
            transport
                .viewport(&session, &control)
                .await
                .expect("viewport")
                .height,
            844
        );
        assert_eq!(
            transport
                .screenshot_png(&session, &control)
                .await
                .expect("screenshot"),
            png
        );
        assert_eq!(
            transport
                .web_viewport_screenshot_png(&session, &control)
                .await
                .expect("web viewport screenshot"),
            png
        );
        transport
            .delete_session(&session, &control)
            .await
            .expect("delete session");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn element_commands_are_typed_and_bounded() {
        let responses = vec![
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session-1/element",
                body: Some(json!({"using":"css selector","value":"#search"})),
                status: 200,
                response: json!({"value":{"element-6066-11e4-a52e-4f735466cecf":"element/1"}}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session-1/element/element%2F1/rect",
                body: None,
                status: 200,
                response: json!({"value":{"x":10,"y":20,"width":30,"height":40}}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session-1/element/element%2F1/attribute/aria-label",
                body: None,
                status: 200,
                response: json!({"value":"Search"}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session-1/element/element%2F1/displayed",
                body: None,
                status: 200,
                response: json!({"value":true}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session-1/element/element%2F1/enabled",
                body: None,
                status: 200,
                response: json!({"value":true}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session-1/element/element%2F1/click",
                body: Some(json!({})),
                status: 200,
                response: json!({"value":null}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session-1/element/element%2F1/clear",
                body: Some(json!({})),
                status: 200,
                response: json!({"value":null}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session-1/element/element%2F1/value",
                body: Some(json!({"text":"12"})),
                status: 200,
                response: json!({"value":null}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session-1/execute/sync",
                body: Some(json!({"script":"return document.title","args":[]})),
                status: 200,
                response: json!({"value":"Baidu"}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session-1/execute/sync",
                body: Some(json!({"script":"mobile: pressButton","args":[{"name":"home"}]})),
                status: 200,
                response: json!({"value":null}),
            },
        ];
        let (transport, server) = mock_transport(responses).await;
        let session = super::AppiumSession::parse("session-1").expect("session");
        let control = ExecutionControl::unbounded();
        let element = transport
            .find_element(
                &session,
                AppiumLocatorStrategy::CssSelector,
                "#search",
                &control,
            )
            .await
            .expect("element");
        let rect = transport
            .element_rect(&session, &element, &control)
            .await
            .expect("rect");
        assert_eq!(rect.width, 30.0);
        assert_eq!(
            transport
                .element_attribute(&session, &element, "aria-label", &control)
                .await
                .expect("attribute"),
            Some(Value::String("Search".to_owned()))
        );
        assert!(
            transport
                .element_displayed(&session, &element, &control)
                .await
                .expect("displayed")
        );
        assert!(
            transport
                .element_enabled(&session, &element, &control)
                .await
                .expect("enabled")
        );
        transport
            .click_element(&session, &element, &control)
            .await
            .expect("click");
        transport
            .clear_element(&session, &element, &control)
            .await
            .expect("clear");
        transport
            .set_element_value(&session, &element, "12", &control)
            .await
            .expect("value");
        assert_eq!(
            transport
                .execute_script(&session, "return document.title", &[], &control)
                .await
                .expect("script"),
            Value::String("Baidu".to_owned())
        );
        transport
            .press_button(&session, AppiumButton::Home, &control)
            .await
            .expect("press button");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn legacy_coordinate_actions_use_closed_w3c_commands() {
        let responses = vec![
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session-1/actions",
                body: Some(json!({
                    "actions": [{
                        "type": "pointer",
                        "id": "devicerail-finger",
                        "parameters": {"pointerType": "touch"},
                        "actions": [
                            {"type":"pointerMove","duration":0,"origin":"viewport","x":10,"y":20},
                            {"type":"pointerDown","button":0},
                            {"type":"pause","duration":50},
                            {"type":"pointerUp","button":0}
                        ]
                    }]
                })),
                status: 200,
                response: json!({"value":null}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session-1/actions",
                body: Some(json!({
                    "actions": [{
                        "type": "pointer",
                        "id": "devicerail-finger",
                        "parameters": {"pointerType": "touch"},
                        "actions": [
                            {"type":"pointerMove","duration":0,"origin":"viewport","x":10,"y":20},
                            {"type":"pointerDown","button":0},
                            {"type":"pause","duration":50},
                            {"type":"pointerMove","duration":300,"origin":"viewport","x":30,"y":40},
                            {"type":"pointerUp","button":0}
                        ]
                    }]
                })),
                status: 200,
                response: json!({"value":null}),
            },
            ExpectedResponse {
                method: "GET",
                route: "/wd/hub/session/session-1/element/active",
                body: None,
                status: 200,
                response: json!({"value":{"element-6066-11e4-a52e-4f735466cecf":"active-1"}}),
            },
            ExpectedResponse {
                method: "POST",
                route: "/wd/hub/session/session-1/element/active-1/value",
                body: Some(json!({"text":"ab"})),
                status: 200,
                response: json!({"value":null}),
            },
        ];
        let (transport, server) = mock_transport(responses).await;
        let session = super::AppiumSession::parse("session-1").expect("session");
        let control = ExecutionControl::unbounded();
        transport
            .tap_coordinate(&session, 10, 20, &control)
            .await
            .expect("tap");
        transport
            .drag(
                &session,
                super::AppiumDrag::new(10, 20, 30, 40, 300).expect("gesture"),
                &control,
            )
            .await
            .expect("drag");
        transport
            .send_keys(&session, "ab", &control)
            .await
            .expect("keys");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn webdriver_errors_map_to_explicit_driver_errors() {
        let responses = vec![ExpectedResponse {
            method: "POST",
            route: "/wd/hub/session/session-1/element",
            body: Some(json!({"using":"accessibility id","value":"missing"})),
            status: 404,
            response: json!({"value":{"error":"no such element","message":"sensitive details"}}),
        }];
        let (transport, server) = mock_transport(responses).await;
        let error = transport
            .find_element(
                &super::AppiumSession::parse("session-1").expect("session"),
                AppiumLocatorStrategy::AccessibilityId,
                "missing",
                &ExecutionControl::unbounded(),
            )
            .await
            .expect_err("missing element");
        assert!(matches!(error, DriverError::ElementNotFound));
        server.await.expect("server");
    }
}
