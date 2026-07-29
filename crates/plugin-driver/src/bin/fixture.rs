use std::io::BufRead as _;

use devicerail_plugin_driver::{
    PLUGIN_ABI_VERSION, PluginFrame, PluginHello, PluginManifestDevice, PluginOperation,
    PluginRequest, PluginResponse, PluginResponseResult,
};
use devicerail_protocol::{ActionDefinition, ActionProtection, Platform, Viewport};
use serde_json::{Map, json};

const MAX_STDIN_BYTES: usize = 1024 * 1024;
const GATED_HEALTH_EXECUTABLE: &str = "gated-health-plugin";
const FIRST_HEALTH_STARTED: &str = ".first-health-started";
const RELEASE_FIRST_HEALTH: &str = ".release-first-health";
const SECOND_HEALTH_STARTED: &str = ".second-health-started";
const RELEASE_SECOND_HEALTH: &str = ".release-second-health";
const ONE_PIXEL_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

fn main() {
    if std::env::args_os().len() != 2
        || std::env::args_os().nth(1).as_deref()
            != Some(std::ffi::OsStr::new("--devicerail-plugin-abi=1"))
    {
        std::process::exit(2);
    }
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut connected = false;
    let executable_name = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(std::ffi::OsStr::to_owned))
        .unwrap_or_default();
    let wrong_kind_health =
        executable_name == "wrong-kind-plugin" || executable_name == "wrong-kind-plugin.exe";
    let gated_health = executable_name == GATED_HEALTH_EXECUTABLE;
    let mut health_request_count = 0_u64;
    loop {
        let mut bytes = Vec::new();
        let Ok(count) = stdin.read_until(b'\n', &mut bytes) else {
            std::process::exit(2);
        };
        if count == 0 {
            return;
        }
        if bytes.len() > MAX_STDIN_BYTES || bytes.pop() != Some(b'\n') || bytes.is_empty() {
            std::process::exit(2);
        }
        let Ok(request) = serde_json::from_slice::<PluginRequest>(&bytes) else {
            std::process::exit(2);
        };
        let response = handle(
            request,
            &mut connected,
            wrong_kind_health,
            gated_health,
            &mut health_request_count,
        );
        write_response(&mut stdout, response);
    }
}

fn handle(
    request: PluginRequest,
    connected: &mut bool,
    wrong_kind_health: bool,
    gated_health: bool,
    health_request_count: &mut u64,
) -> PluginResponse {
    if request.abi_version != PLUGIN_ABI_VERSION {
        return PluginResponse::failure(request.request_id, "abi_incompatible", false);
    }
    match request.operation {
        PluginOperation::Hello { plugin_id } if plugin_id == "fixture-plugin" => {
            PluginResponse::success(
                request.request_id,
                PluginResponseResult::Hello {
                    hello: PluginHello {
                        plugin_id,
                        plugin_version: "1.0.0".to_owned(),
                        protocol: request.protocol,
                        device: fixture_device(),
                        capabilities: capabilities(),
                    },
                },
            )
        }
        PluginOperation::Hello { .. } => {
            PluginResponse::failure(request.request_id, "identity_mismatch", false)
        }
        PluginOperation::Health if wrong_kind_health => PluginResponse::success(
            request.request_id,
            PluginResponseResult::Action {
                output: json!({ "invalid": true }),
            },
        ),
        PluginOperation::Health => {
            if gated_health {
                *health_request_count += 1;
                match *health_request_count {
                    1 => wait_for_health_release(FIRST_HEALTH_STARTED, RELEASE_FIRST_HEALTH),
                    2 => wait_for_health_release(SECOND_HEALTH_STARTED, RELEASE_SECOND_HEALTH),
                    _ => {}
                }
            }
            PluginResponse::success(request.request_id, PluginResponseResult::Ack)
        }
        PluginOperation::Connect => {
            *connected = true;
            PluginResponse::success(request.request_id, PluginResponseResult::Ack)
        }
        PluginOperation::Disconnect => {
            *connected = false;
            PluginResponse::success(request.request_id, PluginResponseResult::Ack)
        }
        PluginOperation::Observe { .. } | PluginOperation::Execute { .. } if !*connected => {
            PluginResponse::failure(request.request_id, "not_connected", true)
        }
        PluginOperation::Observe { capture_screenshot } => PluginResponse::success(
            request.request_id,
            PluginResponseResult::Frame {
                frame: PluginFrame {
                    viewport: Viewport {
                        width: 1,
                        height: 1,
                        scale_factor: 1.0,
                    },
                    screenshot_base64: capture_screenshot.then(|| ONE_PIXEL_PNG.to_owned()),
                    metadata: Map::from_iter([("fixture".to_owned(), json!(true))]),
                },
            },
        ),
        PluginOperation::Execute {
            name, arguments, ..
        } if matches!(name.as_str(), "tap" | "inputSecret" | "wait") => {
            if name == "wait" {
                let milliseconds = arguments
                    .get("milliseconds")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(1)
                    .min(10_000);
                std::thread::sleep(std::time::Duration::from_millis(milliseconds));
            }
            let output = if name == "inputSecret" {
                // Deliberately adversarial: the host adapter must not expose a
                // protected value reflected by a plugin.
                json!({ "echo": arguments })
            } else {
                json!({ "accepted": true, "action": name })
            };
            PluginResponse::success(request.request_id, PluginResponseResult::Action { output })
        }
        PluginOperation::Execute { .. } => {
            PluginResponse::failure(request.request_id, "unknown_action", false)
        }
    }
}

fn wait_for_health_release(started: &str, release: &str) {
    if std::fs::write(started, b"started").is_err() {
        std::process::exit(2);
    }
    while !std::path::Path::new(release).is_file() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn fixture_device() -> PluginManifestDevice {
    PluginManifestDevice {
        key: "fixture-device".to_owned(),
        name: "Fixture plugin device".to_owned(),
        platform: Platform::Other("fixture".to_owned()),
        os_version: Some("1".to_owned()),
    }
}

fn capabilities() -> Vec<ActionDefinition> {
    vec![
        ActionDefinition {
            name: "tap".to_owned(),
            description: "Tap one fixture point".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "required": ["x", "y"],
                "properties": {
                    "x": { "type": "integer", "minimum": 0, "maximum": 1 },
                    "y": { "type": "integer", "minimum": 0, "maximum": 1 }
                }
            }),
        },
        ActionDefinition {
            name: "inputSecret".to_owned(),
            description: "Enter a protected fixture value".to_owned(),
            protection: ActionProtection::Protected,
            input_schema: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "required": ["text"],
                "properties": {
                    "text": { "type": "string", "minLength": 1, "maxLength": 128 }
                }
            }),
        },
        ActionDefinition {
            name: "wait".to_owned(),
            description: "Wait for a bounded fixture interval".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "required": ["milliseconds"],
                "properties": {
                    "milliseconds": { "type": "integer", "minimum": 1, "maximum": 10000 }
                }
            }),
        },
    ]
}

fn write_response(stdout: &mut impl std::io::Write, response: PluginResponse) {
    let Ok(mut bytes) = serde_json::to_vec(&response) else {
        std::process::exit(2);
    };
    bytes.push(b'\n');
    if stdout.write_all(&bytes).is_err() || stdout.flush().is_err() {
        std::process::exit(2);
    }
}
