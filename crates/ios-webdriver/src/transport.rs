use async_trait::async_trait;
use devicerail_core::{DriverResult, ExecutionControl};
use devicerail_protocol::Viewport;

const MAX_SESSION_ID_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WdaStatus {
    pub ready: bool,
    pub os_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WdaSession {
    id: String,
}

impl WdaSession {
    pub fn parse(id: impl Into<String>) -> DriverResult<Self> {
        let id = id.into();
        if id.is_empty()
            || id.len() > MAX_SESSION_ID_BYTES
            || id
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
        {
            return Err(devicerail_core::DriverError::Protocol(
                "WDA returned an invalid session id".to_owned(),
            ));
        }
        Ok(Self { id })
    }

    pub fn as_str(&self) -> &str {
        &self.id
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WdaPage {
    pub source: String,
    pub viewport: Viewport,
}

/// Closed WebDriverAgent action surface used by the iOS Driver.
///
/// The transport intentionally has no generic path/body operation, preventing
/// action arguments from becoming an arbitrary WDA request boundary.
#[derive(Clone, PartialEq)]
pub enum WdaAction {
    Tap {
        x: u32,
        y: u32,
    },
    TypeText(String),
    PressKey(IosKey),
    Drag {
        start_x: u32,
        start_y: u32,
        end_x: u32,
        end_y: u32,
        duration_ms: u32,
    },
}

impl std::fmt::Debug for WdaAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tap { x, y } => formatter
                .debug_struct("Tap")
                .field("x", x)
                .field("y", y)
                .finish(),
            Self::TypeText(value) => formatter
                .debug_struct("TypeText")
                .field("byte_len", &value.len())
                .finish_non_exhaustive(),
            Self::PressKey(key) => formatter.debug_tuple("PressKey").field(key).finish(),
            Self::Drag {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
            } => formatter
                .debug_struct("Drag")
                .field("start_x", start_x)
                .field("start_y", start_y)
                .field("end_x", end_x)
                .field("end_y", end_y)
                .field("duration_ms", duration_ms)
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IosKey {
    Enter,
    Tab,
    Escape,
    Delete,
    Space,
    Home,
    VolumeUp,
    VolumeDown,
}

impl IosKey {
    pub const VALUES: [&'static str; 8] = [
        "enter",
        "tab",
        "escape",
        "delete",
        "space",
        "home",
        "volumeUp",
        "volumeDown",
    ];

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "enter" => Some(Self::Enter),
            "tab" => Some(Self::Tab),
            "escape" => Some(Self::Escape),
            "delete" => Some(Self::Delete),
            "space" => Some(Self::Space),
            "home" => Some(Self::Home),
            "volumeUp" => Some(Self::VolumeUp),
            "volumeDown" => Some(Self::VolumeDown),
            _ => None,
        }
    }
}

#[async_trait]
pub trait WdaTransport: Send + Sync {
    async fn status(&self, control: &ExecutionControl) -> DriverResult<WdaStatus>;
    async fn create_session(&self, control: &ExecutionControl) -> DriverResult<WdaSession>;
    async fn delete_session(
        &self,
        session: &WdaSession,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
    /// Performs a non-mutating, Session-scoped liveness probe.
    ///
    /// Custom transports may override this with a cheaper endpoint. The
    /// default keeps existing implementations source-compatible and uses the
    /// normal inspection boundary to prove that WDA still owns `session`.
    async fn probe_session(
        &self,
        session: &WdaSession,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        self.inspect(session, control).await.map(drop)
    }
    async fn inspect(
        &self,
        session: &WdaSession,
        control: &ExecutionControl,
    ) -> DriverResult<WdaPage>;
    async fn screenshot_png(
        &self,
        session: &WdaSession,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<u8>>;
    async fn perform(
        &self,
        session: &WdaSession,
        action: WdaAction,
        control: &ExecutionControl,
    ) -> DriverResult<()>;
}
