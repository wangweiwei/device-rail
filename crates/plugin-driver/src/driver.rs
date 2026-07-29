use std::{
    collections::{BTreeMap, BTreeSet},
    future::pending,
    io::Cursor,
    sync::Arc,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devicerail_core::{
    DeviceDriver, DeviceOperationResult, DriverError, DriverOperationContext, DriverResult,
    ExecutionControl, ScreenshotPolicy, now_ms, run_bounded_blocking,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, AssetRef, DeviceId, DeviceInfo,
    Observation, ScreenshotOmissionReason,
};
use png::{DecodeOptions, Decoder, Limits, Transformations};
use serde_json::Value;
use tokio::{sync::Mutex, time};
use uuid::Uuid;

use crate::{
    DiscoveryConfig, PluginDescriptor, PluginFrame, PluginHello, PluginOperation, PluginRequest,
    PluginResponseResult, discover_plugin_descriptors, transport::PluginTransport,
};

const MAX_CAPABILITIES: usize = 128;
const MAX_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_SCHEMA_DEPTH: usize = 64;
const MAX_SCHEMA_NODES: usize = 8_192;
const MAX_DESCRIPTION_CHARS: usize = 1_024;
const MAX_METADATA_ENTRIES: usize = 64;
const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_ACTION_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_SCREENSHOT_ENCODED_BYTES: usize = 22 * 1024 * 1024;
const MAX_SCREENSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SCREENSHOT_DIMENSION: u32 = 8_192;
const MAX_SCREENSHOT_PIXELS: u64 = 16_000_000;
const MAX_SCREENSHOT_DECODED_BYTES: usize = 64 * 1024 * 1024;

pub struct PluginDriver {
    id: DeviceId,
    descriptor: PluginDescriptor,
    capabilities: Arc<Vec<ActionDefinition>>,
    protection: BTreeMap<String, ActionProtection>,
    transport: PluginTransport,
    connected: Mutex<bool>,
}

impl std::fmt::Debug for PluginDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginDriver")
            .field("id", &self.id)
            .field("plugin_id", &self.descriptor.manifest().plugin_id)
            .field("plugin_version", &self.descriptor.manifest().plugin_version)
            .finish_non_exhaustive()
    }
}

impl PluginDriver {
    pub async fn load(
        descriptor: PluginDescriptor,
        control: &ExecutionControl,
    ) -> DriverResult<Self> {
        let transport = PluginTransport::new(
            descriptor.executable().to_path_buf(),
            descriptor.transport(),
        );
        let request = PluginRequest::new(
            descriptor.selected_protocol(),
            PluginOperation::Hello {
                plugin_id: descriptor.manifest().plugin_id.clone(),
            },
        );
        let PluginResponseResult::Hello { hello } = transport.request(request, control).await?
        else {
            return Err(platform("plugin_hello_invalid", false));
        };
        validate_hello(&descriptor, &hello)?;
        let capabilities = Arc::new(hello.capabilities);
        let protection = capabilities
            .iter()
            .map(|definition| (definition.name.clone(), definition.protection))
            .collect();
        let id = DeviceId::new(format!(
            "plugin:{}:{}",
            descriptor.manifest().plugin_id,
            descriptor.manifest().device.key
        ));
        Ok(Self {
            id,
            descriptor,
            capabilities,
            protection,
            transport,
            connected: Mutex::new(false),
        })
    }

    pub async fn device_info(&self) -> DeviceInfo {
        let connected = *self.connected.lock().await;
        self.info(connected)
    }

    fn info(&self, connected: bool) -> DeviceInfo {
        let device = &self.descriptor.manifest().device;
        DeviceInfo {
            id: self.id.clone(),
            name: device.name.clone(),
            platform: device.platform.clone(),
            os_version: device.os_version.clone(),
            connected,
        }
    }

    async fn capture(
        &self,
        context: &DriverOperationContext,
        omission: Option<ScreenshotOmissionReason>,
        redact_metadata: bool,
    ) -> DeviceOperationResult<Observation> {
        let request = PluginRequest::new(
            self.descriptor.selected_protocol(),
            PluginOperation::Observe {
                capture_screenshot: omission.is_none(),
            },
        );
        let PluginResponseResult::Frame { frame } =
            self.transport.request(request, context.control()).await?
        else {
            return Err(platform("plugin_frame_invalid", false).into());
        };
        validate_frame(&frame, omission.is_none())?;
        let screenshot = match frame.screenshot_base64 {
            Some(encoded) => {
                let viewport = frame.viewport.clone();
                let png = run_bounded_blocking(
                    context.control(),
                    move || decode_and_canonicalize_png(&encoded, &viewport),
                    || platform("plugin_screenshot_invalid", false),
                )
                .await?;
                let size = png.len() as u64;
                let stored = context
                    .evidence()
                    .put_with_declared_size("image/png", size, Box::pin(Cursor::new(png)))
                    .await?;
                Some(stored.asset_ref())
            }
            None => None,
        };
        Ok(Observation {
            id: Uuid::new_v4(),
            device_id: self.id.clone(),
            captured_at_ms: now_ms(),
            viewport: frame.viewport,
            screenshot,
            screenshot_omission: omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata: if redact_metadata {
                Default::default()
            } else {
                frame.metadata
            },
        })
    }
}

#[async_trait]
impl DeviceDriver for PluginDriver {
    fn id(&self) -> &DeviceId {
        &self.id
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.protection.get(name).copied()
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        let mut connected = lock_connected(&self.connected, control).await?;
        if *connected {
            return Ok(self.info(true));
        }
        expect_ack(
            self.transport
                .request(
                    PluginRequest::new(
                        self.descriptor.selected_protocol(),
                        PluginOperation::Connect,
                    ),
                    control,
                )
                .await?,
        )?;
        *connected = true;
        Ok(self.info(true))
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        let mut connected = lock_connected(&self.connected, control).await?;
        if !*connected {
            return Ok(());
        }
        expect_ack(
            self.transport
                .request(
                    PluginRequest::new(
                        self.descriptor.selected_protocol(),
                        PluginOperation::Disconnect,
                    ),
                    control,
                )
                .await?,
        )?;
        *connected = false;
        Ok(())
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        ensure_active(control)?;
        Ok(self.capabilities.as_ref().clone())
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        expect_ack(
            self.transport
                .request(
                    PluginRequest::new(
                        self.descriptor.selected_protocol(),
                        PluginOperation::Health,
                    ),
                    control,
                )
                .await?,
        )
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let connected = lock_connected(&self.connected, context.control()).await?;
        if !*connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        self.capture(context, omission, false).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let connected = lock_connected(&self.connected, context.control()).await?;
        if !*connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let definition = self
            .capabilities
            .iter()
            .find(|definition| definition.name == call.name)
            .ok_or_else(|| DriverError::UnknownAction(call.name.clone()))?;
        let validator = jsonschema::validator_for(&definition.input_schema).map_err(|_| {
            DriverError::Protocol("plugin capability schema is no longer valid".to_owned())
        })?;
        if !validator.is_valid(&call.arguments) {
            return Err(DriverError::InvalidArguments {
                action: call.name.clone(),
                message: "arguments do not satisfy the advertised schema".to_owned(),
            }
            .into());
        }
        let protected = definition.protection == ActionProtection::Protected;
        let omission = if protected {
            Some(ScreenshotOmissionReason::ProtectedAction)
        } else {
            match context.screenshot_policy() {
                ScreenshotPolicy::Capture => None,
                ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
            }
        };
        let before = self.capture(context, omission, protected).await?;
        let started_at_ms = now_ms();
        let request = PluginRequest::new(
            self.descriptor.selected_protocol(),
            PluginOperation::Execute {
                call_id: call.id,
                name: call.name.clone(),
                arguments: call.arguments,
            },
        );
        let PluginResponseResult::Action { output } =
            self.transport.request(request, context.control()).await?
        else {
            return Err(platform("plugin_action_invalid", false).into());
        };
        if serde_json::to_vec(&output).map_or(true, |bytes| bytes.len() > MAX_ACTION_OUTPUT_BYTES) {
            return Err(platform("plugin_action_output_limit", false).into());
        }
        // Protected arguments are already redacted by Core. Do not allow an
        // untrusted plugin to reflect them back through its durable output.
        let output = if protected {
            serde_json::json!({ "accepted": true })
        } else {
            output
        };
        let after = self.capture(context, omission, protected).await?;
        ensure_active(context.control())?;
        let finished_at_ms = now_ms().max(started_at_ms);
        let evidence = deduplicated_screenshots(&before, &after);
        Ok(ActionResult {
            call_id: call.id,
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

pub async fn discover_plugin_drivers(
    config: &DiscoveryConfig,
    control: &ExecutionControl,
) -> DriverResult<Vec<Arc<PluginDriver>>> {
    let descriptors =
        discover_plugin_descriptors(config).map_err(|error| platform(error.code(), false))?;
    let mut drivers = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        drivers.push(Arc::new(PluginDriver::load(descriptor, control).await?));
    }
    drivers.sort_by(|left, right| left.id().cmp(right.id()));
    Ok(drivers)
}

fn validate_hello(descriptor: &PluginDescriptor, hello: &PluginHello) -> DriverResult<()> {
    let manifest = descriptor.manifest();
    if hello.plugin_id != manifest.plugin_id
        || hello.plugin_version != manifest.plugin_version
        || hello.protocol != descriptor.selected_protocol()
        || hello.device != manifest.device
        || hello.capabilities.is_empty()
        || hello.capabilities.len() > MAX_CAPABILITIES
    {
        return Err(platform("plugin_hello_mismatch", false));
    }
    let declarations = manifest
        .capabilities
        .iter()
        .map(|capability| (capability.name.as_str(), capability.protection))
        .collect::<BTreeMap<_, _>>();
    let mut names = BTreeSet::new();
    for definition in &hello.capabilities {
        if definition.name.is_empty()
            || definition.name.len() > 64
            || !names.insert(definition.name.as_str())
            || definition.description.is_empty()
            || definition.description.chars().count() > MAX_DESCRIPTION_CHARS
            || definition.description.chars().any(char::is_control)
            || declarations.get(definition.name.as_str()) != Some(&definition.protection)
        {
            return Err(platform("plugin_capability_mismatch", false));
        }
        validate_action_schema(&definition.input_schema)?;
    }
    if names.len() != declarations.len() {
        return Err(platform("plugin_capability_mismatch", false));
    }
    Ok(())
}

fn validate_action_schema(schema: &Value) -> DriverResult<()> {
    if schema.get("type") != Some(&Value::String("object".to_owned()))
        || serde_json::to_vec(schema).map_or(true, |bytes| bytes.len() > MAX_SCHEMA_BYTES)
        || jsonschema::meta::validate(schema).is_err()
    {
        return Err(platform("plugin_capability_schema_invalid", false));
    }
    let mut nodes = 0;
    validate_schema_tree(schema, 0, &mut nodes)?;
    jsonschema::validator_for(schema)
        .map(drop)
        .map_err(|_| platform("plugin_capability_schema_invalid", false))
}

fn validate_schema_tree(value: &Value, depth: usize, nodes: &mut usize) -> DriverResult<()> {
    *nodes = nodes.saturating_add(1);
    if depth > MAX_SCHEMA_DEPTH || *nodes > MAX_SCHEMA_NODES {
        return Err(platform("plugin_capability_schema_limit", false));
    }
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if matches!(key.as_str(), "$ref" | "$dynamicRef")
                    && child
                        .as_str()
                        .is_none_or(|reference| !reference.starts_with('#'))
                {
                    return Err(platform("plugin_capability_schema_external_ref", false));
                }
                validate_schema_tree(child, depth + 1, nodes)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                validate_schema_tree(child, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_frame(frame: &PluginFrame, screenshot_required: bool) -> DriverResult<()> {
    let pixels = u64::from(frame.viewport.width)
        .checked_mul(u64::from(frame.viewport.height))
        .ok_or_else(|| platform("plugin_frame_invalid", false))?;
    if frame.viewport.width == 0
        || frame.viewport.height == 0
        || frame.viewport.width > MAX_SCREENSHOT_DIMENSION
        || frame.viewport.height > MAX_SCREENSHOT_DIMENSION
        || pixels > MAX_SCREENSHOT_PIXELS
        || !frame.viewport.scale_factor.is_finite()
        || frame.viewport.scale_factor <= 0.0
        || frame.viewport.scale_factor > 16.0
        || frame.metadata.len() > MAX_METADATA_ENTRIES
        || frame
            .metadata
            .keys()
            .any(|key| key.is_empty() || key.len() > 128 || key.chars().any(char::is_control))
        || serde_json::to_vec(&frame.metadata)
            .map_or(true, |bytes| bytes.len() > MAX_METADATA_BYTES)
        || screenshot_required != frame.screenshot_base64.is_some()
        || frame
            .screenshot_base64
            .as_deref()
            .is_some_and(|encoded| encoded.len() > MAX_SCREENSHOT_ENCODED_BYTES)
    {
        return Err(platform("plugin_frame_invalid", false));
    }
    Ok(())
}

fn decode_and_canonicalize_png(
    encoded: &str,
    viewport: &devicerail_protocol::Viewport,
) -> DriverResult<Vec<u8>> {
    let png = BASE64
        .decode(encoded)
        .map_err(|_| platform("plugin_screenshot_invalid", false))?;
    if png.is_empty() || png.len() > MAX_SCREENSHOT_BYTES {
        return Err(platform("plugin_screenshot_invalid", false));
    }
    let mut options = DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(true);
    options.set_ignore_iccp_chunk(true);
    let mut decoder = Decoder::new_with_options(Cursor::new(&png), options);
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    decoder.set_limits(Limits {
        bytes: MAX_SCREENSHOT_DECODED_BYTES,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|_| platform("plugin_screenshot_invalid", false))?;
    let info = reader.info();
    if info.width != viewport.width
        || info.height != viewport.height
        || info.animation_control.is_some()
        || reader
            .output_buffer_size()
            .is_none_or(|size| size > MAX_SCREENSHOT_DECODED_BYTES)
    {
        return Err(platform("plugin_screenshot_invalid", false));
    }
    let output_size = reader
        .output_buffer_size()
        .ok_or_else(|| platform("plugin_screenshot_invalid", false))?;
    let mut pixels = vec![0; output_size];
    let output = reader
        .next_frame(&mut pixels)
        .map_err(|_| platform("plugin_screenshot_invalid", false))?;
    pixels.truncate(output.buffer_size());
    reader
        .finish()
        .map_err(|_| platform("plugin_screenshot_invalid", false))?;
    let mut canonical = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut canonical, output.width, output.height);
        encoder.set_color(output.color_type);
        encoder.set_depth(output.bit_depth);
        let mut writer = encoder
            .write_header()
            .map_err(|_| platform("plugin_screenshot_invalid", false))?;
        writer
            .write_image_data(&pixels)
            .map_err(|_| platform("plugin_screenshot_invalid", false))?;
    }
    if canonical.len() > MAX_SCREENSHOT_BYTES {
        return Err(platform("plugin_screenshot_invalid", false));
    }
    Ok(canonical)
}

fn expect_ack(result: PluginResponseResult) -> DriverResult<()> {
    if matches!(result, PluginResponseResult::Ack) {
        Ok(())
    } else {
        Err(platform("plugin_response_kind_invalid", false))
    }
}

async fn lock_connected<'a>(
    connected: &'a Mutex<bool>,
    control: &ExecutionControl,
) -> DriverResult<tokio::sync::MutexGuard<'a, bool>> {
    ensure_active(control)?;
    let deadline = async {
        match control.remaining() {
            Some(remaining) => time::sleep(remaining).await,
            None => pending::<()>().await,
        }
    };
    tokio::select! {
        guard = connected.lock() => Ok(guard),
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

fn platform(code: &str, retryable: bool) -> DriverError {
    DriverError::Platform {
        code: code.to_owned(),
        retryable,
    }
}
