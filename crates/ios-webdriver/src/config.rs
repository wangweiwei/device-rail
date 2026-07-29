use devicerail_core::{DriverError, DriverResult};
use devicerail_protocol::DeviceId;
use std::net::IpAddr;

use url::{Host, Url};

const MAX_ENDPOINT_BYTES: usize = 4_096;
const MAX_DEVICE_TOKEN_BYTES: usize = 512;
const MAX_DEVICE_NAME_BYTES: usize = 1_024;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const MAX_REQUEST_TIMEOUT_MS: u64 = 5 * 60_000;

/// A validated clear-text HTTP endpoint for WDA or its MJPEG side channel.
///
/// WDA normally listens on a local USB tunnel. TLS and credentials are
/// intentionally rejected here instead of being implemented incompletely;
/// callers needing TLS can inject their own [`WdaTransport`](crate::WdaTransport)
/// or [`MjpegFrameSource`](crate::MjpegFrameSource).
#[derive(Clone, PartialEq, Eq)]
pub struct HttpEndpointConfig {
    host: String,
    authority: String,
    port: u16,
    base_path: String,
    request_timeout_ms: u64,
}

impl std::fmt::Debug for HttpEndpointConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpEndpointConfig")
            .field("endpoint", &"[REDACTED]")
            .field("request_timeout_ms", &self.request_timeout_ms)
            .finish()
    }
}

impl HttpEndpointConfig {
    pub fn new(endpoint: impl AsRef<str>) -> DriverResult<Self> {
        let endpoint = endpoint.as_ref();
        let parsed = Url::parse(endpoint).map_err(|_| invalid_endpoint())?;
        if endpoint.is_empty()
            || endpoint.len() > MAX_ENDPOINT_BYTES
            || parsed.scheme() != "http"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(invalid_endpoint());
        }
        let host = parsed.host().ok_or_else(invalid_endpoint)?;
        let port = parsed
            .port_or_known_default()
            .ok_or_else(invalid_endpoint)?;
        let (host, host_for_header) = match host {
            Host::Domain(value) => (value.to_owned(), value.to_owned()),
            Host::Ipv4(value) => (value.to_string(), value.to_string()),
            Host::Ipv6(value) => (value.to_string(), format!("[{value}]")),
        };
        let default_port = port == 80;
        let authority = if default_port {
            host_for_header
        } else {
            format!("{host_for_header}:{port}")
        };
        let base_path = normalize_base_path(parsed.path())?;
        Ok(Self {
            host,
            authority,
            port,
            base_path,
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        })
    }

    pub fn with_request_timeout_ms(mut self, timeout_ms: u64) -> DriverResult<Self> {
        if timeout_ms == 0 || timeout_ms > MAX_REQUEST_TIMEOUT_MS {
            return Err(DriverError::Protocol(
                "invalid iOS HTTP request timeout".to_owned(),
            ));
        }
        self.request_timeout_ms = timeout_ms;
        Ok(self)
    }

    /// Returns whether the endpoint host is an explicit numeric loopback IP.
    ///
    /// Hostnames such as `localhost` deliberately return `false`: callers
    /// enforcing a local-only transport boundary must not depend on DNS
    /// resolution remaining pinned to loopback.
    pub fn is_numeric_loopback(&self) -> bool {
        self.host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    pub(crate) fn authority(&self) -> &str {
        &self.authority
    }

    pub(crate) const fn port(&self) -> u16 {
        self.port
    }

    pub(crate) const fn request_timeout_ms(&self) -> u64 {
        self.request_timeout_ms
    }

    pub(crate) fn request_path(&self) -> &str {
        &self.base_path
    }

    pub(crate) fn route(&self, suffix: &str) -> DriverResult<String> {
        if !suffix.starts_with('/')
            || suffix.contains(['\r', '\n'])
            || suffix.len() > MAX_ENDPOINT_BYTES
        {
            return Err(DriverError::Protocol("invalid iOS HTTP route".to_owned()));
        }
        Ok(if self.base_path == "/" {
            suffix.to_owned()
        } else {
            format!("{}{suffix}", self.base_path)
        })
    }
}

/// Stable identity supplied by the process that owns WDA lifecycle/routing.
#[derive(Clone, PartialEq, Eq)]
pub struct IosDeviceConfig {
    id: DeviceId,
    name: String,
    os_version: Option<String>,
}

impl std::fmt::Debug for IosDeviceConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IosDeviceConfig")
            .field("device", &"[REDACTED]")
            .field("os_version_configured", &self.os_version.is_some())
            .finish()
    }
}

impl IosDeviceConfig {
    pub fn new(
        stable_token: impl Into<String>,
        name: impl Into<String>,
        os_version: Option<String>,
    ) -> DriverResult<Self> {
        let stable_token = stable_token.into();
        let name = name.into();
        if !bounded_text(&stable_token, MAX_DEVICE_TOKEN_BYTES)
            || stable_token
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
            || !bounded_text(&name, MAX_DEVICE_NAME_BYTES)
            || os_version
                .as_deref()
                .is_some_and(|value| !bounded_text(value, 256))
        {
            return Err(DriverError::Protocol(
                "invalid iOS device descriptor".to_owned(),
            ));
        }
        Ok(Self {
            id: DeviceId::new(format!("ios-wda:{stable_token}")),
            name,
            os_version,
        })
    }

    pub fn id(&self) -> &DeviceId {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn os_version(&self) -> Option<&str> {
        self.os_version.as_deref()
    }
}

fn normalize_base_path(path: &str) -> DriverResult<String> {
    if path.contains(['\r', '\n']) || path.split('/').any(|segment| segment == "..") {
        return Err(invalid_endpoint());
    }
    let trimmed = path.trim_end_matches('/');
    Ok(if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    })
}

fn bounded_text(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn invalid_endpoint() -> DriverError {
    DriverError::Protocol("invalid iOS HTTP endpoint".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{HttpEndpointConfig, IosDeviceConfig};

    #[test]
    fn endpoint_is_explicit_and_bounded() {
        let endpoint =
            HttpEndpointConfig::new("http://127.0.0.1:8100/wd/hub/").expect("valid endpoint");
        assert_eq!(endpoint.route("/status").expect("route"), "/wd/hub/status");
        assert!(HttpEndpointConfig::new("https://127.0.0.1:8100").is_err());
        assert!(HttpEndpointConfig::new("http://user@127.0.0.1:8100").is_err());
        assert!(HttpEndpointConfig::new("http://127.0.0.1:8100?secret=yes").is_err());
    }

    #[test]
    fn numeric_loopback_detection_does_not_trust_dns_names() {
        assert!(
            HttpEndpointConfig::new("http://127.0.0.1:8100")
                .expect("IPv4 loopback")
                .is_numeric_loopback()
        );
        assert!(
            HttpEndpointConfig::new("http://[::1]:8100")
                .expect("IPv6 loopback")
                .is_numeric_loopback()
        );
        assert!(
            !HttpEndpointConfig::new("http://localhost:8100")
                .expect("localhost hostname")
                .is_numeric_loopback()
        );
        assert!(
            !HttpEndpointConfig::new("http://192.0.2.1:8100")
                .expect("documentation address")
                .is_numeric_loopback()
        );
    }

    #[test]
    fn endpoint_debug_is_fully_redacted() {
        let endpoint =
            HttpEndpointConfig::new("http://127.0.0.1:8100/private/WDA-ENDPOINT-TOKEN-SENTINEL")
                .expect("valid endpoint");
        let debug = format!("{endpoint:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(debug.contains("request_timeout_ms"));
        assert!(!debug.contains("127.0.0.1"));
        assert!(!debug.contains("WDA-ENDPOINT-TOKEN-SENTINEL"));
    }

    #[test]
    fn device_debug_is_fully_redacted() {
        let device = IosDeviceConfig::new(
            "IOS-DEVICE-TOKEN-SENTINEL",
            "IOS-DEVICE-NAME-SENTINEL",
            Some("IOS-OS-VERSION-SENTINEL".to_owned()),
        )
        .expect("valid device descriptor");
        let debug = format!("{device:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("IOS-DEVICE-TOKEN-SENTINEL"));
        assert!(!debug.contains("IOS-DEVICE-NAME-SENTINEL"));
        assert!(!debug.contains("IOS-OS-VERSION-SENTINEL"));
    }

    #[test]
    fn device_identity_rejects_path_and_control_tokens() {
        let descriptor = IosDeviceConfig::new("00008030-001", "Test iPhone", Some("18.0".into()))
            .expect("valid descriptor");
        assert_eq!(descriptor.id().0, "ios-wda:00008030-001");
        assert!(IosDeviceConfig::new("../phone", "phone", None).is_err());
        assert!(IosDeviceConfig::new("phone", "\n", None).is_err());
    }
}
