use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use devicerail_core::{DeviceOperationResult, DriverOperationContext, ExecutionControl, now_ms};
use devicerail_protocol::{
    AssetRef, DeviceId, DeviceInfo, Observation, ScreenshotOmissionReason, Viewport,
};
use serde_json::{Map, Value, json};
use tokio::sync::{
    Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, Semaphore, SemaphorePermit,
};
use uuid::Uuid;

use crate::{
    AdbCommand, AdbCommandOutput, AdbCommandRunner, AdbDeviceState, AdbOperation, AdbProperty,
    AdbSerial, AndroidAdbError, AndroidAdbResult, DiscoveredAndroidDevice, ProtectedAdbInput,
    command::classify_protected_transport_stderr,
    observation::{
        AndroidObservationError, AndroidObservationGeometry, AndroidObservationResult, PixelSize,
        parse_display_only_geometry, parse_observation_geometry,
    },
};

const MAX_RECONNECT_ATTEMPTS: usize = 8;
const MAX_BOOT_CHECKS: usize = 240;
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_OBSERVATIONS_PER_DEVICE: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AndroidDeviceConfig {
    pub reconnect_attempts: usize,
    pub boot_checks: usize,
    pub poll_interval: Duration,
}

impl Default for AndroidDeviceConfig {
    fn default() -> Self {
        Self {
            reconnect_attempts: 1,
            boot_checks: 20,
            poll_interval: Duration::from_millis(250),
        }
    }
}

impl AndroidDeviceConfig {
    fn validate(self) -> AndroidAdbResult<Self> {
        if self.reconnect_attempts > MAX_RECONNECT_ATTEMPTS {
            return Err(AndroidAdbError::InvalidValue {
                field: "reconnectAttempts",
                value: self.reconnect_attempts.to_string(),
            });
        }
        if self.boot_checks == 0 || self.boot_checks > MAX_BOOT_CHECKS {
            return Err(AndroidAdbError::InvalidValue {
                field: "bootChecks",
                value: self.boot_checks.to_string(),
            });
        }
        if self.poll_interval.is_zero() || self.poll_interval > MAX_POLL_INTERVAL {
            return Err(AndroidAdbError::InvalidValue {
                field: "pollInterval",
                value: format!("{:?}", self.poll_interval),
            });
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidHealth {
    pub adb_state: AdbDeviceState,
    pub boot_completed: bool,
    pub connected: bool,
}

struct LifecycleState {
    adb_state: AdbDeviceState,
    info: DeviceInfo,
}

pub struct AndroidDevice {
    id: DeviceId,
    serial: AdbSerial,
    runner: Arc<dyn AdbCommandRunner>,
    config: AndroidDeviceConfig,
    operation_gate: RwLock<()>,
    capture_slots: Semaphore,
    lifecycle: Mutex<LifecycleState>,
    /// Set only by explicit transport-connectivity classifications. Keeping
    /// this separate from the lifecycle mutex lets a failed ADB command
    /// invalidate cached connected state synchronously without awaiting a
    /// second lock while an operation is already unwinding.
    transport_invalidated: AtomicBool,
}

impl AndroidDevice {
    pub(crate) fn new(
        descriptor: DiscoveredAndroidDevice,
        runner: Arc<dyn AdbCommandRunner>,
        config: AndroidDeviceConfig,
    ) -> AndroidAdbResult<Self> {
        let config = config.validate()?;
        let info = descriptor.device_info();
        Ok(Self {
            id: info.id.clone(),
            serial: descriptor.serial,
            runner,
            config,
            operation_gate: RwLock::new(()),
            capture_slots: Semaphore::new(MAX_CONCURRENT_OBSERVATIONS_PER_DEVICE),
            lifecycle: Mutex::new(LifecycleState {
                adb_state: descriptor.state,
                info,
            }),
            transport_invalidated: AtomicBool::new(false),
        })
    }

    pub fn id(&self) -> &DeviceId {
        &self.id
    }

    pub fn serial(&self) -> &AdbSerial {
        &self.serial
    }

    pub async fn device_info(&self) -> DeviceInfo {
        let lifecycle = self.lifecycle.lock().await;
        let mut info = lifecycle.info.clone();
        if self.transport_invalidated.load(Ordering::SeqCst) {
            info.connected = false;
        }
        info
    }

    pub async fn connect(&self, control: &ExecutionControl) -> AndroidAdbResult<DeviceInfo> {
        let _operation = self.lock_operation_write(control, "connect").await?;
        {
            let mut lifecycle = self.lock_lifecycle(control, "connect").await?;
            if lifecycle.info.connected && !self.transport_invalidated.load(Ordering::SeqCst) {
                return Ok(lifecycle.info.clone());
            }
            lifecycle.info.connected = false;
        }

        let mut state = match self.read_state(control).await {
            Ok(state) => state,
            Err(error) if self.config.reconnect_attempts > 0 && is_transient_missing(&error) => {
                AdbDeviceState::Offline
            }
            Err(error) => return Err(error),
        };
        self.lock_lifecycle(control, "connect").await?.adb_state = state.clone();
        if state == AdbDeviceState::Offline {
            for attempt in 0..self.config.reconnect_attempts {
                let reconnect = self
                    .runner
                    .run(
                        AdbCommand::for_device(self.serial.clone(), AdbOperation::Reconnect),
                        control,
                    )
                    .await
                    .map_err(|error| classify_command_error(&self.id, error));
                if let Err(error) = reconnect {
                    if !is_transient_missing(&error) {
                        return Err(error);
                    }
                }

                let wait = self
                    .runner
                    .run(
                        AdbCommand::for_device(self.serial.clone(), AdbOperation::WaitForDevice),
                        control,
                    )
                    .await
                    .map_err(|error| classify_command_error(&self.id, error));
                if let Err(error) = wait {
                    if !is_transient_missing(&error) {
                        return Err(error);
                    }
                }

                state = match self.read_state(control).await {
                    Ok(state) => state,
                    Err(error) if is_transient_missing(&error) => AdbDeviceState::Offline,
                    Err(error) => return Err(error),
                };
                self.lock_lifecycle(control, "connect").await?.adb_state = state.clone();
                if state == AdbDeviceState::Ready {
                    break;
                }
                if state != AdbDeviceState::Offline {
                    break;
                }
                if attempt + 1 < self.config.reconnect_attempts {
                    sleep_controlled(control, self.config.poll_interval, "reconnect").await?;
                }
            }
        }
        require_ready(&self.id, &state, self.config.reconnect_attempts)?;

        let mut boot_completed = false;
        for attempt in 0..self.config.boot_checks {
            let value = self
                .read_property(AdbProperty::BootCompleted, control)
                .await?;
            if parse_boot_completed(value)? {
                boot_completed = true;
                break;
            }
            if attempt + 1 < self.config.boot_checks {
                sleep_controlled(control, self.config.poll_interval, "boot polling").await?;
            }
        }
        if !boot_completed {
            let mut lifecycle = self.lock_lifecycle(control, "connect").await?;
            lifecycle.adb_state = AdbDeviceState::Ready;
            lifecycle.info.connected = false;
            return Err(AndroidAdbError::BootingExhausted {
                device_id: self.id.clone(),
                attempts: self.config.boot_checks,
            });
        }

        let release = self
            .read_property(AdbProperty::ReleaseVersion, control)
            .await?;
        if release.trim().is_empty() {
            return Err(AndroidAdbError::InvalidValue {
                field: "ro.build.version.release",
                value: release,
            });
        }
        let manufacturer = self
            .read_property(AdbProperty::ProductManufacturer, control)
            .await?;
        let model = self
            .read_property(AdbProperty::ProductModel, control)
            .await?;
        let refreshed_name = [manufacturer.trim(), model.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let mut lifecycle = self.lock_lifecycle(control, "connect").await?;
        if !refreshed_name.is_empty() {
            lifecycle.info.name = refreshed_name;
        }
        lifecycle.adb_state = AdbDeviceState::Ready;
        lifecycle.info.os_version = Some(release);
        self.transport_invalidated.store(false, Ordering::SeqCst);
        lifecycle.info.connected = true;
        Ok(lifecycle.info.clone())
    }

    pub async fn health(&self, control: &ExecutionControl) -> AndroidAdbResult<AndroidHealth> {
        let _operation = self.lock_operation_write(control, "health").await?;
        {
            let mut lifecycle = self.lock_lifecycle(control, "health").await?;
            if !lifecycle.info.connected || self.transport_invalidated.load(Ordering::SeqCst) {
                lifecycle.info.connected = false;
                return Ok(AndroidHealth {
                    adb_state: lifecycle.adb_state.clone(),
                    boot_completed: false,
                    connected: false,
                });
            }
        }

        let state = match self.read_state(control).await {
            Ok(state) => state,
            Err(error) => {
                self.lock_lifecycle(control, "health").await?.info.connected = false;
                return Err(error);
            }
        };
        let boot_completed = if state == AdbDeviceState::Ready {
            match self
                .read_property(AdbProperty::BootCompleted, control)
                .await
                .and_then(parse_boot_completed)
            {
                Ok(value) => value,
                Err(error) => {
                    self.lock_lifecycle(control, "health").await?.info.connected = false;
                    return Err(error);
                }
            }
        } else {
            false
        };
        let connected = state == AdbDeviceState::Ready && boot_completed;
        let mut lifecycle = self.lock_lifecycle(control, "health").await?;
        lifecycle.adb_state = state.clone();
        lifecycle.info.connected = connected;
        Ok(AndroidHealth {
            adb_state: state,
            boot_completed,
            connected,
        })
    }

    /// Ends only DeviceRail's logical lifecycle. It never detaches USB,
    /// disconnects every TCP device, or mutates the shared adb server.
    pub async fn disconnect(&self, control: &ExecutionControl) -> AndroidAdbResult<()> {
        let _operation = self.lock_operation_write(control, "disconnect").await?;
        let mut lifecycle = self.lock_lifecycle(control, "disconnect").await?;
        lifecycle.info.connected = false;
        self.transport_invalidated.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Captures a Session-bound observation for a Core Driver integration.
    ///
    /// AndroidDriver delegates here for standalone observations. Platform
    /// failures become Driver failures, while Evidence Store failures retain
    /// Core's distinct `DeviceOperationError::Evidence` channel.
    pub async fn capture_observation(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        self.observe(context)
            .await
            .map_err(AndroidObservationError::into_device_operation_error)
    }

    /// Captures one Android observation and pins its screenshot to the
    /// operation Session before publishing the protocol DTO.
    ///
    /// This remains crate-private so the public Driver boundary cannot bypass
    /// Core's operation context or the device's operation gate.
    pub(crate) async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> AndroidObservationResult<Observation> {
        // Acquire the byte/process budget before the lifecycle read gate. A
        // backlog of observations therefore cannot hold read guards while an
        // Action or lifecycle writer is waiting.
        let _capture = self
            .acquire_capture_slot(context.control(), "observe")
            .await?;
        // A read lease keeps the logical lifecycle stable from capture through
        // durable evidence pinning while allowing a small bounded number of
        // independent observes.
        // Lifecycle state itself is only locked for short reads or updates.
        let _operation = self
            .lock_operation_read(context.control(), "observe")
            .await?;
        self.observe_gate_and_capture_slot_held(context).await
    }

    /// Captures an observation while the caller already holds this device's
    /// operation gate. AndroidDriver uses this from an exclusive Action lease
    /// so the before/action/after sequence cannot deadlock by recursively
    /// acquiring the read side of the same gate.
    pub(crate) async fn observe_gate_held(
        &self,
        context: &DriverOperationContext,
    ) -> AndroidObservationResult<Observation> {
        let _capture = self
            .acquire_capture_slot(context.control(), "observe")
            .await?;
        self.observe_gate_and_capture_slot_held(context).await
    }

    async fn observe_gate_and_capture_slot_held(
        &self,
        context: &DriverOperationContext,
    ) -> AndroidObservationResult<Observation> {
        match context.screenshot_policy() {
            devicerail_core::ScreenshotPolicy::Capture => {
                self.observe_capture_gate_held(context).await
            }
            devicerail_core::ScreenshotPolicy::Omit => {
                self.observe_display_gate_held(context, ScreenshotOmissionReason::Policy)
                    .await
            }
        }
    }

    pub(crate) async fn observe_protected_gate_held(
        &self,
        context: &DriverOperationContext,
    ) -> AndroidObservationResult<Observation> {
        let _capture = self
            .acquire_capture_slot(context.control(), "observe")
            .await?;
        self.observe_display_gate_held(context, ScreenshotOmissionReason::ProtectedAction)
            .await
    }

    async fn observe_capture_gate_held(
        &self,
        context: &DriverOperationContext,
    ) -> AndroidObservationResult<Observation> {
        {
            let lifecycle = self.lock_lifecycle(context.control(), "observe").await?;
            if !lifecycle.info.connected || self.transport_invalidated.load(Ordering::SeqCst) {
                return Err(AndroidObservationError::NotConnected(self.id.clone()));
            }
        }

        let screenshot_output = self
            .runner
            .run(
                AdbCommand::for_device(self.serial.clone(), AdbOperation::CaptureScreenshot),
                context.control(),
            )
            .await
            .map_err(|error| self.classify_device_command_error(error))?;
        ensure_active(context.control(), "observe")?;
        // Capture time describes the completed screenshot, not the later
        // geometry queries or durable Store write.
        let captured_at_ms = now_ms();
        let screenshot_png = screenshot_output.into_stdout_bytes();

        let size_output = self
            .runner
            .run(
                AdbCommand::for_device(self.serial.clone(), AdbOperation::WindowSize),
                context.control(),
            )
            .await
            .map_err(|error| self.classify_device_command_error(error))?;
        ensure_active(context.control(), "observe")?;
        let density_output = self
            .runner
            .run(
                AdbCommand::for_device(self.serial.clone(), AdbOperation::WindowDensity),
                context.control(),
            )
            .await
            .map_err(|error| self.classify_device_command_error(error))?;
        ensure_active(context.control(), "observe")?;

        let geometry = parse_observation_geometry(
            &screenshot_png,
            size_output.stdout_text()?,
            density_output.stdout_text()?,
        )?;
        ensure_active(context.control(), "observe")?;
        let declared_size = screenshot_png.len() as u64;
        let stored = context
            .evidence()
            .put_with_declared_size(
                "image/png",
                declared_size,
                Box::pin(Cursor::new(screenshot_png)),
            )
            .await?;
        ensure_active(context.control(), "observe")?;

        Ok(
            self.observation_from_geometry(
                captured_at_ms,
                geometry,
                Some(stored.asset_ref()),
                None,
            ),
        )
    }

    async fn observe_display_gate_held(
        &self,
        context: &DriverOperationContext,
        omission: ScreenshotOmissionReason,
    ) -> AndroidObservationResult<Observation> {
        {
            let lifecycle = self.lock_lifecycle(context.control(), "observe").await?;
            if !lifecycle.info.connected || self.transport_invalidated.load(Ordering::SeqCst) {
                return Err(AndroidObservationError::NotConnected(self.id.clone()));
            }
        }
        let size_output = self
            .runner
            .run(
                AdbCommand::for_device(self.serial.clone(), AdbOperation::WindowSize),
                context.control(),
            )
            .await
            .map_err(|error| self.classify_device_command_error(error))?;
        ensure_active(context.control(), "observe")?;
        let density_output = self
            .runner
            .run(
                AdbCommand::for_device(self.serial.clone(), AdbOperation::WindowDensity),
                context.control(),
            )
            .await
            .map_err(|error| self.classify_device_command_error(error))?;
        ensure_active(context.control(), "observe")?;
        let geometry =
            parse_display_only_geometry(size_output.stdout_text()?, density_output.stdout_text()?)?;
        ensure_active(context.control(), "observe")?;
        Ok(self.observation_from_geometry(now_ms(), geometry, None, Some(omission)))
    }

    fn observation_from_geometry(
        &self,
        captured_at_ms: u64,
        geometry: AndroidObservationGeometry,
        screenshot: Option<AssetRef>,
        screenshot_omission: Option<ScreenshotOmissionReason>,
    ) -> Observation {
        let mut metadata = Map::new();
        metadata.insert(
            "android".to_owned(),
            json!({
                "orientation": geometry.orientation.as_str(),
                "scaleFactor": geometry.scale_factor,
                "densityDpi": geometry.density_dpi,
                "physicalSize": pixel_size_value(geometry.display_size.physical),
                "overrideSize": geometry.display_size.override_size.map(pixel_size_value),
                "effectiveSize": pixel_size_value(geometry.display_size.effective()),
                "physicalDensityDpi": geometry.display_density.physical_dpi,
                "overrideDensityDpi": geometry.display_density.override_dpi,
            }),
        );
        Observation {
            id: Uuid::new_v4(),
            device_id: self.id.clone(),
            captured_at_ms,
            viewport: Viewport {
                width: geometry.viewport_size.width,
                height: geometry.viewport_size.height,
                scale_factor: geometry.scale_factor,
            },
            screenshot,
            screenshot_omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata,
        }
    }

    pub(crate) async fn connected_gate_held(
        &self,
        control: &ExecutionControl,
        operation: &'static str,
    ) -> AndroidAdbResult<bool> {
        let lifecycle = self.lock_lifecycle(control, operation).await?;
        Ok(lifecycle.info.connected && !self.transport_invalidated.load(Ordering::SeqCst))
    }

    pub(crate) async fn run_operation_gate_held(
        &self,
        operation: AdbOperation,
        control: &ExecutionControl,
    ) -> AndroidAdbResult<AdbCommandOutput> {
        let operation_name = operation.name();
        ensure_active(control, operation_name)?;
        let output = self
            .runner
            .run(
                AdbCommand::for_device(self.serial.clone(), operation),
                control,
            )
            .await
            .map_err(|error| self.classify_device_command_error(error))?;
        ensure_active(control, operation_name)?;
        Ok(output)
    }

    pub(crate) async fn run_protected_operation_gate_held(
        &self,
        input: ProtectedAdbInput,
        control: &ExecutionControl,
    ) -> AndroidAdbResult<()> {
        const OPERATION: &str = "input_secret";
        ensure_active(control, OPERATION)?;
        let result = self
            .runner
            .run_protected(
                AdbCommand::for_device(self.serial.clone(), AdbOperation::InputSecret),
                input,
                control,
            )
            .await;
        if let Err(error) = result {
            let classified = match error {
                AndroidAdbError::ProcessFailed {
                    operation,
                    status,
                    stderr_tail,
                } => classify_protected_transport_stderr(&self.serial, stderr_tail.as_bytes())
                    .unwrap_or(AndroidAdbError::ProtectedOperationFailed { operation, status }),
                error => error,
            };
            return Err(self.classify_device_command_error(classified));
        }
        ensure_active(control, OPERATION)
    }

    async fn lock_operation_read<'a>(
        &'a self,
        control: &ExecutionControl,
        operation: &'static str,
    ) -> AndroidAdbResult<RwLockReadGuard<'a, ()>> {
        ensure_active(control, operation)?;
        let lock = self.operation_gate.read();
        tokio::pin!(lock);
        let guard = match control.remaining() {
            Some(remaining) => {
                let deadline = tokio::time::sleep(remaining);
                tokio::pin!(deadline);
                tokio::select! {
                    biased;
                    guard = &mut lock => guard,
                    _ = control.cancelled() => return Err(AndroidAdbError::Cancelled),
                    () = &mut deadline => return Err(AndroidAdbError::TimedOut { operation }),
                }
            }
            None => {
                tokio::select! {
                    biased;
                    guard = &mut lock => guard,
                    _ = control.cancelled() => return Err(AndroidAdbError::Cancelled),
                }
            }
        };
        ensure_active(control, operation)?;
        Ok(guard)
    }

    async fn acquire_capture_slot<'a>(
        &'a self,
        control: &ExecutionControl,
        operation: &'static str,
    ) -> AndroidAdbResult<SemaphorePermit<'a>> {
        ensure_active(control, operation)?;
        let acquire = self.capture_slots.acquire();
        tokio::pin!(acquire);
        let permit = match control.remaining() {
            Some(remaining) => {
                let deadline = tokio::time::sleep(remaining);
                tokio::pin!(deadline);
                tokio::select! {
                    biased;
                    permit = &mut acquire => permit.expect("Android observation semaphore is never closed"),
                    _ = control.cancelled() => return Err(AndroidAdbError::Cancelled),
                    () = &mut deadline => return Err(AndroidAdbError::TimedOut { operation }),
                }
            }
            None => {
                tokio::select! {
                    biased;
                    permit = &mut acquire => permit.expect("Android observation semaphore is never closed"),
                    _ = control.cancelled() => return Err(AndroidAdbError::Cancelled),
                }
            }
        };
        ensure_active(control, operation)?;
        Ok(permit)
    }

    pub(crate) async fn lock_operation_write<'a>(
        &'a self,
        control: &ExecutionControl,
        operation: &'static str,
    ) -> AndroidAdbResult<RwLockWriteGuard<'a, ()>> {
        ensure_active(control, operation)?;
        let lock = self.operation_gate.write();
        tokio::pin!(lock);
        let guard = match control.remaining() {
            Some(remaining) => {
                let deadline = tokio::time::sleep(remaining);
                tokio::pin!(deadline);
                tokio::select! {
                    biased;
                    guard = &mut lock => guard,
                    _ = control.cancelled() => return Err(AndroidAdbError::Cancelled),
                    () = &mut deadline => return Err(AndroidAdbError::TimedOut { operation }),
                }
            }
            None => {
                tokio::select! {
                    biased;
                    guard = &mut lock => guard,
                    _ = control.cancelled() => return Err(AndroidAdbError::Cancelled),
                }
            }
        };
        ensure_active(control, operation)?;
        Ok(guard)
    }

    async fn lock_lifecycle<'a>(
        &'a self,
        control: &ExecutionControl,
        operation: &'static str,
    ) -> AndroidAdbResult<MutexGuard<'a, LifecycleState>> {
        ensure_active(control, operation)?;
        let lock = self.lifecycle.lock();
        tokio::pin!(lock);
        let guard = match control.remaining() {
            Some(remaining) => {
                let deadline = tokio::time::sleep(remaining);
                tokio::pin!(deadline);
                tokio::select! {
                    biased;
                    guard = &mut lock => guard,
                    _ = control.cancelled() => return Err(AndroidAdbError::Cancelled),
                    () = &mut deadline => return Err(AndroidAdbError::TimedOut { operation }),
                }
            }
            None => {
                tokio::select! {
                    biased;
                    guard = &mut lock => guard,
                    _ = control.cancelled() => return Err(AndroidAdbError::Cancelled),
                }
            }
        };
        ensure_active(control, operation)?;
        Ok(guard)
    }

    async fn read_state(&self, control: &ExecutionControl) -> AndroidAdbResult<AdbDeviceState> {
        let output = match self
            .runner
            .run(
                AdbCommand::for_device(self.serial.clone(), AdbOperation::GetState),
                control,
            )
            .await
        {
            Ok(output) => output,
            Err(error) => {
                return match self.classify_device_command_error(error) {
                    AndroidAdbError::OfflineExhausted { .. } => Ok(AdbDeviceState::Offline),
                    AndroidAdbError::Unauthorized { .. } => Ok(AdbDeviceState::Unauthorized),
                    AndroidAdbError::PermissionDenied { .. } => Ok(AdbDeviceState::NoPermissions),
                    error => Err(error),
                };
            }
        };
        let value = output.stdout_text()?.trim();
        if value.is_empty() {
            return Err(AndroidAdbError::InvalidValue {
                field: "adb get-state",
                value: String::new(),
            });
        }
        Ok(AdbDeviceState::parse(value, ""))
    }

    async fn read_property(
        &self,
        property: AdbProperty,
        control: &ExecutionControl,
    ) -> AndroidAdbResult<String> {
        let output = self
            .runner
            .run(
                AdbCommand::for_device(self.serial.clone(), AdbOperation::GetProperty(property)),
                control,
            )
            .await
            .map_err(|error| self.classify_device_command_error(error))?;
        Ok(output.stdout_text()?.trim().to_owned())
    }

    fn classify_device_command_error(&self, error: AndroidAdbError) -> AndroidAdbError {
        let error = classify_command_error(&self.id, error);
        if invalidates_transport(&error) {
            self.transport_invalidated.store(true, Ordering::SeqCst);
        }
        error
    }
}

fn pixel_size_value(size: PixelSize) -> Value {
    json!({
        "width": size.width,
        "height": size.height,
    })
}

fn require_ready(
    device_id: &DeviceId,
    state: &AdbDeviceState,
    reconnect_attempts: usize,
) -> AndroidAdbResult<()> {
    match state {
        AdbDeviceState::Ready => Ok(()),
        AdbDeviceState::Offline => Err(AndroidAdbError::OfflineExhausted {
            device_id: device_id.clone(),
            attempts: reconnect_attempts,
        }),
        AdbDeviceState::Unauthorized | AdbDeviceState::Authorizing => {
            Err(AndroidAdbError::Unauthorized {
                device_id: device_id.clone(),
            })
        }
        AdbDeviceState::NoPermissions => Err(AndroidAdbError::PermissionDenied {
            device_id: device_id.clone(),
        }),
        state => Err(AndroidAdbError::UnsupportedState {
            device_id: device_id.clone(),
            state: state.clone(),
        }),
    }
}

fn classify_command_error(device_id: &DeviceId, error: AndroidAdbError) -> AndroidAdbError {
    let AndroidAdbError::ProcessFailed {
        operation,
        status,
        stderr_tail,
    } = error
    else {
        return error;
    };
    let normalized = stderr_tail.to_ascii_lowercase();
    if normalized.contains("unauthorized") {
        AndroidAdbError::Unauthorized {
            device_id: device_id.clone(),
        }
    } else if normalized.contains("no permissions") || normalized.contains("permission denied") {
        AndroidAdbError::PermissionDenied {
            device_id: device_id.clone(),
        }
    } else if normalized.contains("not found") || normalized.contains("no devices") {
        AndroidAdbError::Missing {
            device_id: device_id.clone(),
        }
    } else if normalized.contains("offline") {
        AndroidAdbError::OfflineExhausted {
            device_id: device_id.clone(),
            attempts: 0,
        }
    } else {
        AndroidAdbError::ProcessFailed {
            operation,
            status,
            stderr_tail,
        }
    }
}

fn is_transient_missing(error: &AndroidAdbError) -> bool {
    matches!(
        error,
        AndroidAdbError::Missing { .. } | AndroidAdbError::OfflineExhausted { .. }
    )
}

fn invalidates_transport(error: &AndroidAdbError) -> bool {
    matches!(
        error,
        AndroidAdbError::Missing { .. }
            | AndroidAdbError::OfflineExhausted { .. }
            | AndroidAdbError::Unauthorized { .. }
            | AndroidAdbError::PermissionDenied { .. }
    )
}

fn parse_boot_completed(value: String) -> AndroidAdbResult<bool> {
    match value.as_str() {
        "1" => Ok(true),
        "0" | "" => Ok(false),
        _ => Err(AndroidAdbError::InvalidValue {
            field: "sys.boot_completed",
            value,
        }),
    }
}

fn ensure_active(control: &ExecutionControl, operation: &'static str) -> AndroidAdbResult<()> {
    if control.is_cancelled() {
        Err(AndroidAdbError::Cancelled)
    } else if control.is_expired() {
        Err(AndroidAdbError::TimedOut { operation })
    } else {
        Ok(())
    }
}

async fn sleep_controlled(
    control: &ExecutionControl,
    duration: Duration,
    operation: &'static str,
) -> AndroidAdbResult<()> {
    ensure_active(control, operation)?;
    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);
    match control.remaining() {
        Some(remaining) => {
            let deadline = tokio::time::sleep(remaining);
            tokio::pin!(deadline);
            tokio::select! {
                biased;
                _ = control.cancelled() => Err(AndroidAdbError::Cancelled),
                () = &mut deadline => Err(AndroidAdbError::TimedOut { operation }),
                () = &mut sleep => Ok(()),
            }
        }
        None => {
            tokio::select! {
                biased;
                _ = control.cancelled() => Err(AndroidAdbError::Cancelled),
                () = &mut sleep => Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use devicerail_core::{
        CancellationReason, ExecutionControl, ExecutionController, TimeoutScope,
    };
    use tokio::sync::Notify;

    use super::{AndroidDevice, AndroidDeviceConfig, AndroidHealth, invalidates_transport};
    use crate::{
        AdbCommand, AdbCommandOutput, AdbCommandRunner, AdbDeviceState, AdbOperation, AdbProperty,
        AdbSerial, AndroidAdbError, AndroidAdbResult, DiscoveredAndroidDevice,
    };

    struct Step {
        operation: AdbOperation,
        result: AndroidAdbResult<AdbCommandOutput>,
    }

    #[test]
    fn only_explicit_connectivity_failures_invalidate_cached_transport_state() {
        let device_id = AdbSerial::parse("emulator-5554")
            .expect("serial")
            .device_id();
        for error in [
            AndroidAdbError::Missing {
                device_id: device_id.clone(),
            },
            AndroidAdbError::OfflineExhausted {
                device_id: device_id.clone(),
                attempts: 0,
            },
            AndroidAdbError::Unauthorized {
                device_id: device_id.clone(),
            },
            AndroidAdbError::PermissionDenied {
                device_id: device_id.clone(),
            },
        ] {
            assert!(invalidates_transport(&error), "{}", error.code());
        }
        for error in [
            AndroidAdbError::Cancelled,
            AndroidAdbError::TimedOut { operation: "test" },
            AndroidAdbError::ProcessFailed {
                operation: "test",
                status: Some(1),
                stderr_tail: "generic failure".to_owned(),
            },
        ] {
            assert!(!invalidates_transport(&error), "{}", error.code());
        }
    }

    impl Step {
        fn text(operation: AdbOperation, stdout: impl Into<String>) -> Self {
            let name = operation.name();
            Self {
                operation,
                result: Ok(AdbCommandOutput::text(name, stdout)),
            }
        }

        fn error(operation: AdbOperation, error: AndroidAdbError) -> Self {
            Self {
                operation,
                result: Err(error),
            }
        }
    }

    struct FakeRunner {
        serial: AdbSerial,
        steps: StdMutex<VecDeque<Step>>,
        calls: StdMutex<Vec<AdbCommand>>,
    }

    impl FakeRunner {
        fn new(serial: AdbSerial, steps: Vec<Step>) -> Self {
            Self {
                serial,
                steps: StdMutex::new(steps.into()),
                calls: StdMutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().expect("calls lock").len()
        }

        fn operations(&self) -> Vec<AdbOperation> {
            self.calls
                .lock()
                .expect("calls lock")
                .iter()
                .map(|command| command.operation().clone())
                .collect()
        }

        fn assert_exhausted(&self) {
            assert!(
                self.steps.lock().expect("steps lock").is_empty(),
                "fake runner still has unconsumed steps"
            );
        }
    }

    #[async_trait]
    impl AdbCommandRunner for FakeRunner {
        async fn run(
            &self,
            command: AdbCommand,
            _control: &ExecutionControl,
        ) -> AndroidAdbResult<AdbCommandOutput> {
            assert_eq!(
                command.serial(),
                Some(&self.serial),
                "every lifecycle command must target the selected serial"
            );
            let step = self
                .steps
                .lock()
                .expect("steps lock")
                .pop_front()
                .expect("unexpected adb command");
            assert_eq!(command.operation(), &step.operation);
            self.calls.lock().expect("calls lock").push(command);
            step.result
        }
    }

    struct BlockingWaitRunner {
        serial: AdbSerial,
        wait_started: Notify,
        calls: StdMutex<Vec<AdbCommand>>,
    }

    impl BlockingWaitRunner {
        fn new(serial: AdbSerial) -> Self {
            Self {
                serial,
                wait_started: Notify::new(),
                calls: StdMutex::new(Vec::new()),
            }
        }

        fn operations(&self) -> Vec<AdbOperation> {
            self.calls
                .lock()
                .expect("calls lock")
                .iter()
                .map(|command| command.operation().clone())
                .collect()
        }
    }

    #[async_trait]
    impl AdbCommandRunner for BlockingWaitRunner {
        async fn run(
            &self,
            command: AdbCommand,
            control: &ExecutionControl,
        ) -> AndroidAdbResult<AdbCommandOutput> {
            assert_eq!(command.serial(), Some(&self.serial));
            let operation = command.operation().clone();
            self.calls.lock().expect("calls lock").push(command);
            match operation {
                AdbOperation::GetState => Ok(AdbCommandOutput::text(
                    AdbOperation::GetState.name(),
                    "offline\n",
                )),
                AdbOperation::Reconnect => Ok(AdbCommandOutput::text(
                    AdbOperation::Reconnect.name(),
                    "reconnecting\n",
                )),
                AdbOperation::WaitForDevice => {
                    self.wait_started.notify_one();
                    super::sleep_controlled(
                        control,
                        Duration::from_secs(60),
                        AdbOperation::WaitForDevice.name(),
                    )
                    .await?;
                    Ok(AdbCommandOutput::text(
                        AdbOperation::WaitForDevice.name(),
                        "",
                    ))
                }
                operation => panic!("unexpected blocking-runner operation: {operation:?}"),
            }
        }
    }

    struct SerialIsolatingRunner {
        blocked_serial: AdbSerial,
        other_serial: AdbSerial,
        block_first_state: AtomicBool,
        blocked_started: Notify,
        release_blocked: Notify,
        calls: StdMutex<Vec<AdbCommand>>,
    }

    impl SerialIsolatingRunner {
        fn new(blocked_serial: AdbSerial, other_serial: AdbSerial) -> Self {
            Self {
                blocked_serial,
                other_serial,
                block_first_state: AtomicBool::new(true),
                blocked_started: Notify::new(),
                release_blocked: Notify::new(),
                calls: StdMutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<AdbCommand> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    #[async_trait]
    impl AdbCommandRunner for SerialIsolatingRunner {
        async fn run(
            &self,
            command: AdbCommand,
            _control: &ExecutionControl,
        ) -> AndroidAdbResult<AdbCommandOutput> {
            let serial = command.serial().expect("device-scoped command").clone();
            assert!(serial == self.blocked_serial || serial == self.other_serial);
            let operation = command.operation().clone();
            self.calls.lock().expect("calls lock").push(command);

            if serial == self.blocked_serial
                && operation == AdbOperation::GetState
                && self.block_first_state.swap(false, Ordering::AcqRel)
            {
                self.blocked_started.notify_one();
                self.release_blocked.notified().await;
            }

            let (name, value) = match operation {
                AdbOperation::GetState => (AdbOperation::GetState.name(), "device\n"),
                AdbOperation::GetProperty(AdbProperty::BootCompleted) => (
                    AdbOperation::GetProperty(AdbProperty::BootCompleted).name(),
                    "1\n",
                ),
                AdbOperation::GetProperty(AdbProperty::ReleaseVersion) => (
                    AdbOperation::GetProperty(AdbProperty::ReleaseVersion).name(),
                    "14\n",
                ),
                AdbOperation::GetProperty(AdbProperty::ProductManufacturer) => (
                    AdbOperation::GetProperty(AdbProperty::ProductManufacturer).name(),
                    "Fixture\n",
                ),
                AdbOperation::GetProperty(AdbProperty::ProductModel) => (
                    AdbOperation::GetProperty(AdbProperty::ProductModel).name(),
                    "Device\n",
                ),
                operation => panic!("unexpected shared-runner operation: {operation:?}"),
            };
            Ok(AdbCommandOutput::text(name, value))
        }
    }

    fn serial() -> AdbSerial {
        AdbSerial::parse("emulator-5554").expect("serial")
    }

    fn descriptor(state: AdbDeviceState) -> DiscoveredAndroidDevice {
        descriptor_for(serial(), state)
    }

    fn descriptor_for(serial: AdbSerial, state: AdbDeviceState) -> DiscoveredAndroidDevice {
        DiscoveredAndroidDevice {
            serial,
            state,
            product: Some("fixture_product".to_owned()),
            model: Some("discovered_model".to_owned()),
            device: Some("fixture_device".to_owned()),
            transport_id: Some(7),
            extensions: BTreeMap::new(),
        }
    }

    fn config(reconnect_attempts: usize, boot_checks: usize) -> AndroidDeviceConfig {
        AndroidDeviceConfig {
            reconnect_attempts,
            boot_checks,
            poll_interval: Duration::from_millis(1),
        }
    }

    fn device(
        state: AdbDeviceState,
        runner: &Arc<FakeRunner>,
        config: AndroidDeviceConfig,
    ) -> AndroidDevice {
        AndroidDevice::new(descriptor(state), runner.clone(), config).expect("device")
    }

    fn ready_steps() -> Vec<Step> {
        vec![
            Step::text(AdbOperation::GetState, "device\n"),
            Step::text(AdbOperation::GetProperty(AdbProperty::BootCompleted), "1\n"),
            Step::text(
                AdbOperation::GetProperty(AdbProperty::ReleaseVersion),
                "14\n",
            ),
            Step::text(
                AdbOperation::GetProperty(AdbProperty::ProductManufacturer),
                "Google\n",
            ),
            Step::text(
                AdbOperation::GetProperty(AdbProperty::ProductModel),
                "Pixel 8\n",
            ),
        ]
    }

    fn process_failure(operation: &'static str, stderr_tail: &str) -> AndroidAdbError {
        AndroidAdbError::ProcessFailed {
            operation,
            status: Some(1),
            stderr_tail: stderr_tail.to_owned(),
        }
    }

    #[tokio::test]
    async fn ready_connect_polls_boot_and_refreshes_stable_metadata() {
        let mut steps = vec![
            Step::text(AdbOperation::GetState, "device\n"),
            Step::text(AdbOperation::GetProperty(AdbProperty::BootCompleted), "0\n"),
            Step::text(AdbOperation::GetProperty(AdbProperty::BootCompleted), "1\n"),
        ];
        steps.extend(ready_steps().into_iter().skip(2));
        let runner = Arc::new(FakeRunner::new(serial(), steps));
        let device = device(AdbDeviceState::Ready, &runner, config(1, 2));

        let discovered = device.device_info().await;
        assert_eq!(discovered.name, "discovered model");
        assert!(!discovered.connected);

        let connected = device
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("connect");
        assert_eq!(connected.id, device.id().clone());
        assert_eq!(connected.name, "Google Pixel 8");
        assert_eq!(connected.os_version.as_deref(), Some("14"));
        assert!(connected.connected);
        assert_eq!(device.device_info().await, connected);
        assert_eq!(
            runner.operations(),
            vec![
                AdbOperation::GetState,
                AdbOperation::GetProperty(AdbProperty::BootCompleted),
                AdbOperation::GetProperty(AdbProperty::BootCompleted),
                AdbOperation::GetProperty(AdbProperty::ReleaseVersion),
                AdbOperation::GetProperty(AdbProperty::ProductManufacturer),
                AdbOperation::GetProperty(AdbProperty::ProductModel),
            ]
        );
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn connect_and_disconnect_are_idempotent_without_extra_adb_commands() {
        let runner = Arc::new(FakeRunner::new(serial(), ready_steps()));
        let device = device(AdbDeviceState::Ready, &runner, config(1, 1));
        let control = ExecutionControl::unbounded();

        let first = device.connect(&control).await.expect("first connect");
        let repeated = device.connect(&control).await.expect("repeated connect");
        assert_eq!(repeated, first);
        assert_eq!(runner.call_count(), 5);

        device.disconnect(&control).await.expect("disconnect");
        device
            .disconnect(&control)
            .await
            .expect("repeated disconnect");
        assert!(!device.device_info().await.connected);
        assert_eq!(runner.call_count(), 5);
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn shared_runner_keeps_serial_routes_independent_when_one_device_blocks() {
        let blocked_serial = AdbSerial::parse("emulator-5554").expect("blocked serial");
        let other_serial = AdbSerial::parse("emulator-5556").expect("other serial");
        let runner = Arc::new(SerialIsolatingRunner::new(
            blocked_serial.clone(),
            other_serial.clone(),
        ));
        let blocked = Arc::new(
            AndroidDevice::new(
                descriptor_for(blocked_serial.clone(), AdbDeviceState::Ready),
                runner.clone(),
                config(1, 1),
            )
            .expect("blocked device"),
        );
        let other = AndroidDevice::new(
            descriptor_for(other_serial.clone(), AdbDeviceState::Ready),
            runner.clone(),
            config(1, 1),
        )
        .expect("other device");

        let blocked_connect = tokio::spawn({
            let blocked = Arc::clone(&blocked);
            async move { blocked.connect(&ExecutionControl::unbounded()).await }
        });
        runner.blocked_started.notified().await;
        assert!(!blocked_connect.is_finished());

        let other_info = tokio::time::timeout(
            Duration::from_millis(250),
            other.connect(&ExecutionControl::unbounded()),
        )
        .await
        .expect("other serial must not wait for blocked serial")
        .expect("other connect");
        assert_eq!(other_info.id, other_serial.device_id());
        assert!(!blocked_connect.is_finished());

        runner.release_blocked.notify_one();
        let blocked_info = blocked_connect
            .await
            .expect("blocked connect task")
            .expect("blocked connect");
        assert_eq!(blocked_info.id, blocked_serial.device_id());

        let calls = runner.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|command| command.serial() == Some(&blocked_serial))
                .count(),
            5
        );
        assert_eq!(
            calls
                .iter()
                .filter(|command| command.serial() == Some(&other_serial))
                .count(),
            5
        );
        assert!(calls.iter().all(|command| command.serial().is_some()));
    }

    #[tokio::test]
    async fn offline_device_reconnects_with_a_bounded_serial_scoped_sequence() {
        let mut steps = vec![
            Step::text(AdbOperation::GetState, "offline\n"),
            Step::text(AdbOperation::Reconnect, "reconnecting\n"),
            Step::text(AdbOperation::WaitForDevice, ""),
            Step::text(AdbOperation::GetState, "device\n"),
        ];
        steps.extend(ready_steps().into_iter().skip(1));
        let runner = Arc::new(FakeRunner::new(serial(), steps));
        let device = device(AdbDeviceState::Offline, &runner, config(2, 1));

        let info = device
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("reconnect");
        assert!(info.connected);
        assert_eq!(
            runner.operations()[..4],
            [
                AdbOperation::GetState,
                AdbOperation::Reconnect,
                AdbOperation::WaitForDevice,
                AdbOperation::GetState,
            ]
        );
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn offline_reconnect_exhaustion_preserves_offline_health_state() {
        let runner = Arc::new(FakeRunner::new(
            serial(),
            vec![
                Step::text(AdbOperation::GetState, "offline\n"),
                Step::text(AdbOperation::Reconnect, "reconnecting\n"),
                Step::text(AdbOperation::WaitForDevice, ""),
                Step::text(AdbOperation::GetState, "offline\n"),
                Step::text(AdbOperation::Reconnect, "reconnecting\n"),
                Step::text(AdbOperation::WaitForDevice, ""),
                Step::text(AdbOperation::GetState, "offline\n"),
            ],
        ));
        let device = device(AdbDeviceState::Ready, &runner, config(2, 1));

        assert!(matches!(
            device.connect(&ExecutionControl::unbounded()).await,
            Err(AndroidAdbError::OfflineExhausted {
                attempts: 2,
                device_id,
            }) if device_id == *device.id()
        ));
        assert!(!device.device_info().await.connected);
        assert_eq!(
            device
                .health(&ExecutionControl::unbounded())
                .await
                .expect("cached health"),
            AndroidHealth {
                adb_state: AdbDeviceState::Offline,
                boot_completed: false,
                connected: false,
            }
        );
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn transient_missing_and_offline_failures_do_not_end_reconnect_early() {
        let mut steps = vec![
            Step::error(
                AdbOperation::GetState,
                process_failure("get_state", "error: device not found"),
            ),
            Step::error(
                AdbOperation::Reconnect,
                process_failure("reconnect", "error: device offline"),
            ),
            Step::error(
                AdbOperation::WaitForDevice,
                process_failure("wait_for_device", "error: no devices/emulators found"),
            ),
            Step::error(
                AdbOperation::GetState,
                process_failure("get_state", "error: device not found"),
            ),
            Step::text(AdbOperation::Reconnect, "reconnecting\n"),
            Step::text(AdbOperation::WaitForDevice, ""),
            Step::text(AdbOperation::GetState, "device\n"),
        ];
        steps.extend(ready_steps().into_iter().skip(1));
        let runner = Arc::new(FakeRunner::new(serial(), steps));
        let device = device(AdbDeviceState::Ready, &runner, config(2, 1));

        let info = device
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("second reconnect attempt recovers");
        assert!(info.connected);
        assert_eq!(
            &runner.operations()[..7],
            &[
                AdbOperation::GetState,
                AdbOperation::Reconnect,
                AdbOperation::WaitForDevice,
                AdbOperation::GetState,
                AdbOperation::Reconnect,
                AdbOperation::WaitForDevice,
                AdbOperation::GetState,
            ]
        );
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn connect_classifies_unauthorized_permission_denied_and_missing_devices() {
        let unauthorized_runner = Arc::new(FakeRunner::new(
            serial(),
            vec![Step::text(AdbOperation::GetState, "unauthorized\n")],
        ));
        let unauthorized = device(AdbDeviceState::Ready, &unauthorized_runner, config(1, 1));
        assert!(matches!(
            unauthorized.connect(&ExecutionControl::unbounded()).await,
            Err(AndroidAdbError::Unauthorized { device_id })
                if device_id == *unauthorized.id()
        ));
        assert_eq!(
            unauthorized
                .health(&ExecutionControl::unbounded())
                .await
                .expect("cached health")
                .adb_state,
            AdbDeviceState::Unauthorized
        );
        unauthorized_runner.assert_exhausted();

        let permissions_runner = Arc::new(FakeRunner::new(
            serial(),
            vec![Step::error(
                AdbOperation::GetState,
                process_failure("get_state", "device has no permissions"),
            )],
        ));
        let permissions = device(AdbDeviceState::Ready, &permissions_runner, config(1, 1));
        assert!(matches!(
            permissions.connect(&ExecutionControl::unbounded()).await,
            Err(AndroidAdbError::PermissionDenied { device_id })
                if device_id == *permissions.id()
        ));
        assert_eq!(
            permissions
                .health(&ExecutionControl::unbounded())
                .await
                .expect("cached health")
                .adb_state,
            AdbDeviceState::NoPermissions
        );
        permissions_runner.assert_exhausted();

        let missing_runner = Arc::new(FakeRunner::new(
            serial(),
            vec![Step::error(
                AdbOperation::GetState,
                process_failure("get_state", "error: device not found"),
            )],
        ));
        let missing = device(AdbDeviceState::Ready, &missing_runner, config(0, 1));
        assert!(matches!(
            missing.connect(&ExecutionControl::unbounded()).await,
            Err(AndroidAdbError::Missing { device_id }) if device_id == *missing.id()
        ));
        missing_runner.assert_exhausted();
    }

    #[tokio::test]
    async fn connect_rejects_authorizing_and_non_runtime_adb_states_explicitly() {
        let authorizing = Arc::new(FakeRunner::new(
            serial(),
            vec![Step::text(AdbOperation::GetState, "authorizing\n")],
        ));
        let error = device(AdbDeviceState::Authorizing, &authorizing, config(0, 1))
            .connect(&ExecutionControl::unbounded())
            .await
            .expect_err("authorizing device is not ready");
        assert!(matches!(error, AndroidAdbError::Unauthorized { .. }));
        authorizing.assert_exhausted();

        for state in ["recovery", "sideload", "bootloader", "future-state"] {
            let runner = Arc::new(FakeRunner::new(
                serial(),
                vec![Step::text(AdbOperation::GetState, format!("{state}\n"))],
            ));
            let error = device(AdbDeviceState::Ready, &runner, config(0, 1))
                .connect(&ExecutionControl::unbounded())
                .await
                .expect_err("non-runtime adb state is unsupported");
            assert!(matches!(
                error,
                AndroidAdbError::UnsupportedState { state: actual, .. }
                    if actual == AdbDeviceState::parse(state, "")
            ));
            runner.assert_exhausted();
        }
    }

    #[tokio::test]
    async fn boot_polling_exhaustion_is_explicit_and_leaves_device_disconnected() {
        let runner = Arc::new(FakeRunner::new(
            serial(),
            vec![
                Step::text(AdbOperation::GetState, "device\n"),
                Step::text(AdbOperation::GetProperty(AdbProperty::BootCompleted), "0\n"),
                Step::text(AdbOperation::GetProperty(AdbProperty::BootCompleted), "\n"),
                Step::text(AdbOperation::GetProperty(AdbProperty::BootCompleted), "0\n"),
            ],
        ));
        let device = device(AdbDeviceState::Ready, &runner, config(1, 3));

        assert!(matches!(
            device.connect(&ExecutionControl::unbounded()).await,
            Err(AndroidAdbError::BootingExhausted {
                attempts: 3,
                device_id,
            }) if device_id == *device.id()
        ));
        assert!(!device.device_info().await.connected);
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn health_detects_drop_and_caches_disconnected_state_without_more_commands() {
        let mut steps = ready_steps();
        steps.push(Step::text(AdbOperation::GetState, "offline\n"));
        let runner = Arc::new(FakeRunner::new(serial(), steps));
        let device = device(AdbDeviceState::Ready, &runner, config(1, 1));
        let control = ExecutionControl::unbounded();
        device.connect(&control).await.expect("connect");

        let dropped = device.health(&control).await.expect("health");
        assert_eq!(
            dropped,
            AndroidHealth {
                adb_state: AdbDeviceState::Offline,
                boot_completed: false,
                connected: false,
            }
        );
        let calls_after_drop = runner.call_count();
        assert_eq!(
            device.health(&control).await.expect("cached health"),
            dropped
        );
        assert_eq!(runner.call_count(), calls_after_drop);
        assert!(!device.device_info().await.connected);
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn health_rejects_non_boolean_boot_completed_values() {
        let mut steps = ready_steps();
        steps.extend([
            Step::text(AdbOperation::GetState, "device\n"),
            Step::text(
                AdbOperation::GetProperty(AdbProperty::BootCompleted),
                "true\n",
            ),
        ]);
        let runner = Arc::new(FakeRunner::new(serial(), steps));
        let device = device(AdbDeviceState::Ready, &runner, config(1, 1));
        let control = ExecutionControl::unbounded();
        device.connect(&control).await.expect("connect");

        assert!(matches!(
            device.health(&control).await,
            Err(AndroidAdbError::InvalidValue {
                field: "sys.boot_completed",
                value,
            }) if value == "true"
        ));
        assert!(!device.device_info().await.connected);
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn pre_cancelled_and_pre_expired_controls_never_reach_adb() {
        let runner = Arc::new(FakeRunner::new(serial(), Vec::new()));
        let device = device(AdbDeviceState::Ready, &runner, config(1, 1));
        let (controller, cancelled) = ExecutionController::new();
        assert!(controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            device.connect(&cancelled).await,
            Err(AndroidAdbError::Cancelled)
        ));

        let (_, expired) = ExecutionController::with_timeout(0, TimeoutScope::Request);
        assert!(matches!(
            device.connect(&expired).await,
            Err(AndroidAdbError::TimedOut {
                operation: "connect"
            })
        ));
        assert_eq!(runner.call_count(), 0);
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn lifecycle_lock_contention_honors_cancel_and_deadline_for_every_operation() {
        let connect_runner = Arc::new(FakeRunner::new(serial(), Vec::new()));
        let connect_device = Arc::new(device(AdbDeviceState::Ready, &connect_runner, config(1, 1)));
        let connect_guard = connect_device.lifecycle.lock().await;
        let (connect_controller, connect_control) = ExecutionController::new();
        let connect = tokio::spawn({
            let device = Arc::clone(&connect_device);
            async move { device.connect(&connect_control).await }
        });
        tokio::task::yield_now().await;
        assert!(!connect.is_finished());
        assert!(connect_controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(250), connect)
                .await
                .expect("cancelled connect must stop while the lock remains held")
                .expect("connect task"),
            Err(AndroidAdbError::Cancelled)
        ));
        drop(connect_guard);
        assert_eq!(connect_runner.call_count(), 0);

        let health_runner = Arc::new(FakeRunner::new(serial(), Vec::new()));
        let health_device = Arc::new(device(AdbDeviceState::Ready, &health_runner, config(1, 1)));
        let health_guard = health_device.lifecycle.lock().await;
        let (_, health_control) = ExecutionController::with_timeout(5, TimeoutScope::Request);
        let health = tokio::spawn({
            let device = Arc::clone(&health_device);
            async move { device.health(&health_control).await }
        });
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(250), health)
                .await
                .expect("expired health must stop while the lock remains held")
                .expect("health task"),
            Err(AndroidAdbError::TimedOut {
                operation: "health"
            })
        ));
        drop(health_guard);
        assert_eq!(health_runner.call_count(), 0);

        let disconnect_runner = Arc::new(FakeRunner::new(serial(), Vec::new()));
        let disconnect_device = Arc::new(device(
            AdbDeviceState::Ready,
            &disconnect_runner,
            config(1, 1),
        ));
        let disconnect_guard = disconnect_device.lifecycle.lock().await;
        let (disconnect_controller, disconnect_control) = ExecutionController::new();
        let disconnect = tokio::spawn({
            let device = Arc::clone(&disconnect_device);
            async move { device.disconnect(&disconnect_control).await }
        });
        tokio::task::yield_now().await;
        assert!(!disconnect.is_finished());
        assert!(disconnect_controller.cancel(CancellationReason::Requested));
        drop(disconnect_guard);
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(250), disconnect)
                .await
                .expect("disconnect must recheck control after acquiring the lock")
                .expect("disconnect task"),
            Err(AndroidAdbError::Cancelled)
        ));
        assert_eq!(disconnect_runner.call_count(), 0);
    }

    #[tokio::test]
    async fn cancellation_interrupts_serial_scoped_wait_for_device() {
        let runner = Arc::new(BlockingWaitRunner::new(serial()));
        let device = Arc::new(
            AndroidDevice::new(
                descriptor(AdbDeviceState::Offline),
                runner.clone(),
                config(1, 1),
            )
            .expect("device"),
        );
        let (controller, control) = ExecutionController::new();
        let connect = tokio::spawn({
            let device = Arc::clone(&device);
            async move { device.connect(&control).await }
        });
        runner.wait_started.notified().await;
        assert!(controller.cancel(CancellationReason::Requested));

        assert!(matches!(
            connect.await.expect("connect task"),
            Err(AndroidAdbError::Cancelled)
        ));
        assert_eq!(
            runner.operations(),
            vec![
                AdbOperation::GetState,
                AdbOperation::Reconnect,
                AdbOperation::WaitForDevice,
            ]
        );
    }

    #[tokio::test]
    async fn deadline_interrupts_serial_scoped_wait_for_device() {
        let runner = Arc::new(BlockingWaitRunner::new(serial()));
        let device = AndroidDevice::new(
            descriptor(AdbDeviceState::Offline),
            runner.clone(),
            config(1, 1),
        )
        .expect("device");
        let (_, control) = ExecutionController::with_timeout(5, TimeoutScope::Request);

        assert!(matches!(
            device.connect(&control).await,
            Err(AndroidAdbError::TimedOut {
                operation: "wait_for_device"
            })
        ));
        assert_eq!(
            runner.operations(),
            vec![
                AdbOperation::GetState,
                AdbOperation::Reconnect,
                AdbOperation::WaitForDevice,
            ]
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_reconnect_backoff_without_an_extra_state_probe() {
        let runner = Arc::new(FakeRunner::new(
            serial(),
            vec![
                Step::text(AdbOperation::GetState, "offline\n"),
                Step::text(AdbOperation::Reconnect, "reconnecting\n"),
                Step::text(AdbOperation::WaitForDevice, ""),
                Step::text(AdbOperation::GetState, "offline\n"),
            ],
        ));
        let device = Arc::new(device(
            AdbDeviceState::Offline,
            &runner,
            AndroidDeviceConfig {
                reconnect_attempts: 2,
                boot_checks: 1,
                poll_interval: Duration::from_secs(1),
            },
        ));
        let (controller, control) = ExecutionController::new();
        let connect = tokio::spawn({
            let device = Arc::clone(&device);
            async move { device.connect(&control).await }
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            while runner.call_count() < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("connect reached reconnect backoff");
        assert!(controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            connect.await.expect("connect task"),
            Err(AndroidAdbError::Cancelled)
        ));
        assert_eq!(runner.call_count(), 4);
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn deadline_interrupts_reconnect_backoff_without_an_extra_state_probe() {
        let runner = Arc::new(FakeRunner::new(
            serial(),
            vec![
                Step::text(AdbOperation::GetState, "offline\n"),
                Step::text(AdbOperation::Reconnect, "reconnecting\n"),
                Step::text(AdbOperation::WaitForDevice, ""),
                Step::text(AdbOperation::GetState, "offline\n"),
            ],
        ));
        let device = device(
            AdbDeviceState::Offline,
            &runner,
            AndroidDeviceConfig {
                reconnect_attempts: 2,
                boot_checks: 1,
                poll_interval: Duration::from_secs(1),
            },
        );
        let (_, control) = ExecutionController::with_timeout(5, TimeoutScope::Request);

        assert!(matches!(
            device.connect(&control).await,
            Err(AndroidAdbError::TimedOut {
                operation: "reconnect"
            })
        ));
        assert_eq!(runner.call_count(), 4);
        runner.assert_exhausted();
    }
}

#[cfg(test)]
mod observation_integration_tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        future::pending,
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use devicerail_core::{
        CancellationReason, DeviceDriver, DeviceOperationResult, DeviceRuntime, DriverError,
        DriverOperationContext, DriverResult, EvidenceError, EvidenceInput, EvidenceMetadata,
        EvidenceOutput, EvidenceResult, EvidenceStore, ExecutionControl, ExecutionController,
        GcPolicy, GcReport, MemoryEventStore, OperationContext, PutEvidence, ReleaseReport,
        RuntimeError, SessionEventStore, Sha256Digest, StartSession, StoredEvidence, now_ms,
    };
    use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
    use devicerail_protocol::{
        ActionCall, ActionDefinition, ActionResult, DeviceId, DeviceInfo, EventSequence,
        Observation, SessionId, TestEventPayload,
    };
    use serde_json::json;
    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;
    use tokio::{io::AsyncReadExt as _, sync::Notify};

    use super::{AndroidDevice, AndroidDeviceConfig, MAX_CONCURRENT_OBSERVATIONS_PER_DEVICE};
    use crate::{
        AdbCommand, AdbCommandOutput, AdbCommandRunner, AdbDeviceState, AdbOperation, AdbSerial,
        AndroidAdbError, AndroidAdbResult, DiscoveredAndroidDevice,
    };

    struct Step {
        operation: AdbOperation,
        result: AndroidAdbResult<AdbCommandOutput>,
    }

    impl Step {
        fn text(operation: AdbOperation, stdout: impl Into<String>) -> Self {
            Self {
                result: Ok(AdbCommandOutput::text(operation.name(), stdout)),
                operation,
            }
        }

        fn binary(operation: AdbOperation, stdout: Vec<u8>) -> Self {
            Self {
                result: Ok(AdbCommandOutput::binary(operation.name(), stdout)),
                operation,
            }
        }

        fn error(operation: AdbOperation, error: AndroidAdbError) -> Self {
            Self {
                operation,
                result: Err(error),
            }
        }
    }

    struct ObservationRunner {
        serial: AdbSerial,
        steps: StdMutex<VecDeque<Step>>,
        calls: StdMutex<Vec<AdbCommand>>,
        cancel_after: Option<(AdbOperation, ExecutionController)>,
    }

    impl ObservationRunner {
        fn new(steps: Vec<Step>) -> Arc<Self> {
            Arc::new(Self {
                serial: serial(),
                steps: StdMutex::new(steps.into()),
                calls: StdMutex::new(Vec::new()),
                cancel_after: None,
            })
        }

        fn cancelling_after(
            steps: Vec<Step>,
            operation: AdbOperation,
            controller: ExecutionController,
        ) -> Arc<Self> {
            Arc::new(Self {
                serial: serial(),
                steps: StdMutex::new(steps.into()),
                calls: StdMutex::new(Vec::new()),
                cancel_after: Some((operation, controller)),
            })
        }

        fn operations(&self) -> Vec<AdbOperation> {
            self.calls
                .lock()
                .expect("calls lock")
                .iter()
                .map(|command| command.operation().clone())
                .collect()
        }

        fn call_count(&self) -> usize {
            self.calls.lock().expect("calls lock").len()
        }

        fn assert_exhausted(&self) {
            assert!(self.steps.lock().expect("steps lock").is_empty());
        }
    }

    #[async_trait]
    impl AdbCommandRunner for ObservationRunner {
        async fn run(
            &self,
            command: AdbCommand,
            _control: &ExecutionControl,
        ) -> AndroidAdbResult<AdbCommandOutput> {
            assert_eq!(command.serial(), Some(&self.serial));
            let step = self
                .steps
                .lock()
                .expect("steps lock")
                .pop_front()
                .expect("unexpected observation adb command");
            assert_eq!(command.operation(), &step.operation);
            self.calls.lock().expect("calls lock").push(command);
            if let Some((operation, controller)) = &self.cancel_after
                && operation == &step.operation
            {
                controller.cancel(CancellationReason::Requested);
            }
            step.result
        }
    }

    struct BlockingPhaseRunner {
        serial: AdbSerial,
        blocked_operation: AdbOperation,
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
        calls: StdMutex<Vec<AdbOperation>>,
    }

    impl BlockingPhaseRunner {
        fn new(blocked_operation: AdbOperation) -> Arc<Self> {
            Arc::new(Self {
                serial: serial(),
                blocked_operation,
                started: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                calls: StdMutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl AdbCommandRunner for BlockingPhaseRunner {
        async fn run(
            &self,
            command: AdbCommand,
            _control: &ExecutionControl,
        ) -> AndroidAdbResult<AdbCommandOutput> {
            assert_eq!(command.serial(), Some(&self.serial));
            let operation = command.operation().clone();
            self.calls
                .lock()
                .expect("phase calls lock")
                .push(operation.clone());
            if operation == self.blocked_operation {
                self.started.notify_one();
                self.release.notified().await;
            }
            Ok(match operation {
                AdbOperation::CaptureScreenshot => AdbCommandOutput::binary(
                    AdbOperation::CaptureScreenshot.name(),
                    fixture_png(10, 20),
                ),
                AdbOperation::WindowSize => AdbCommandOutput::text(
                    AdbOperation::WindowSize.name(),
                    "Physical size: 10x20\n",
                ),
                AdbOperation::WindowDensity => AdbCommandOutput::text(
                    AdbOperation::WindowDensity.name(),
                    "Physical density: 420\n",
                ),
                other => panic!("unexpected blocking observation operation: {other:?}"),
            })
        }
    }

    struct BoundedObservationRunner {
        serial: AdbSerial,
        active_captures: AtomicUsize,
        max_active_captures: AtomicUsize,
        started_captures: AtomicUsize,
        released: AtomicBool,
        release: Notify,
    }

    impl BoundedObservationRunner {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                serial: serial(),
                active_captures: AtomicUsize::new(0),
                max_active_captures: AtomicUsize::new(0),
                started_captures: AtomicUsize::new(0),
                released: AtomicBool::new(false),
                release: Notify::new(),
            })
        }

        async fn wait_for_started(&self, expected: usize) {
            while self.started_captures.load(Ordering::SeqCst) < expected {
                tokio::task::yield_now().await;
            }
        }

        fn release_all(&self) {
            self.released.store(true, Ordering::SeqCst);
            self.release.notify_waiters();
        }
    }

    #[async_trait]
    impl AdbCommandRunner for BoundedObservationRunner {
        async fn run(
            &self,
            command: AdbCommand,
            _control: &ExecutionControl,
        ) -> AndroidAdbResult<AdbCommandOutput> {
            assert_eq!(command.serial(), Some(&self.serial));
            let operation = command.operation().clone();
            if operation == AdbOperation::CaptureScreenshot {
                let active = self.active_captures.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active_captures.fetch_max(active, Ordering::SeqCst);
                self.started_captures.fetch_add(1, Ordering::SeqCst);
                while !self.released.load(Ordering::SeqCst) {
                    self.release.notified().await;
                }
                self.active_captures.fetch_sub(1, Ordering::SeqCst);
            }
            Ok(match operation {
                AdbOperation::CaptureScreenshot => AdbCommandOutput::binary(
                    AdbOperation::CaptureScreenshot.name(),
                    fixture_png(10, 20),
                ),
                AdbOperation::WindowSize => AdbCommandOutput::text(
                    AdbOperation::WindowSize.name(),
                    "Physical size: 10x20\n",
                ),
                AdbOperation::WindowDensity => AdbCommandOutput::text(
                    AdbOperation::WindowDensity.name(),
                    "Physical density: 420\n",
                ),
                other => panic!("unexpected bounded observation operation: {other:?}"),
            })
        }
    }

    /// Test-only bridge used solely to obtain Core's non-constructable
    /// `DriverOperationContext`. This is not the Android production Driver and
    /// is deliberately never passed to the shared conformance suite.
    struct ObservationTestDriver {
        device: Arc<AndroidDevice>,
    }

    #[async_trait]
    impl DeviceDriver for ObservationTestDriver {
        fn id(&self) -> &DeviceId {
            self.device.id()
        }

        fn action_protection(&self, _name: &str) -> Option<devicerail_protocol::ActionProtection> {
            None
        }

        async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
            self.device
                .connect(control)
                .await
                .map_err(|error| DriverError::Internal(error.to_string()))
        }

        async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
            self.device
                .disconnect(control)
                .await
                .map_err(|error| DriverError::Internal(error.to_string()))
        }

        async fn capabilities(
            &self,
            _control: &ExecutionControl,
        ) -> DriverResult<Vec<ActionDefinition>> {
            Ok(Vec::new())
        }

        async fn observe(
            &self,
            context: &DriverOperationContext,
        ) -> DeviceOperationResult<Observation> {
            self.device.capture_observation(context).await
        }

        async fn execute(
            &self,
            _context: &DriverOperationContext,
            call: ActionCall,
        ) -> DeviceOperationResult<ActionResult> {
            Err(DriverError::UnknownAction(call.name).into())
        }
    }

    fn serial() -> AdbSerial {
        AdbSerial::parse("emulator-5554").expect("serial")
    }

    fn descriptor() -> DiscoveredAndroidDevice {
        DiscoveredAndroidDevice {
            serial: serial(),
            state: AdbDeviceState::Ready,
            product: Some("fixture".to_owned()),
            model: Some("pixel".to_owned()),
            device: Some("fixture".to_owned()),
            transport_id: Some(7),
            extensions: BTreeMap::new(),
        }
    }

    async fn device(runner: &Arc<ObservationRunner>, connected: bool) -> Arc<AndroidDevice> {
        let runner_trait: Arc<dyn AdbCommandRunner> = runner.clone();
        device_with_runner(runner_trait, connected).await
    }

    async fn device_with_runner(
        runner: Arc<dyn AdbCommandRunner>,
        connected: bool,
    ) -> Arc<AndroidDevice> {
        let device = Arc::new(
            AndroidDevice::new(descriptor(), runner, AndroidDeviceConfig::default())
                .expect("device"),
        );
        device.lifecycle.lock().await.info.connected = connected;
        device
    }

    async fn session_context(
        events: &Arc<MemoryEventStore>,
        device_id: &DeviceId,
    ) -> OperationContext {
        let start = StartSession::new(None, Some(device_id.clone()), now_ms());
        let context = OperationContext::new(start.session_id.clone(), None);
        events.start_session(start).await.expect("start session");
        context
    }

    fn observation_steps(png: Vec<u8>) -> Vec<Step> {
        vec![
            Step::binary(AdbOperation::CaptureScreenshot, png),
            Step::text(
                AdbOperation::WindowSize,
                "Physical size: 1080x2400\nOverride size: 720x1280\n",
            ),
            Step::text(
                AdbOperation::WindowDensity,
                "Physical density: 420\nOverride density: 560\n",
            ),
        ]
    }

    fn fixture_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header");
            let pixels = usize::try_from(u64::from(width) * u64::from(height))
                .expect("fixture dimensions fit usize");
            writer
                .write_image_data(&vec![0; pixels])
                .expect("PNG image data");
            writer.finish().expect("PNG trailer");
        }
        bytes
    }

    #[tokio::test]
    async fn observation_pins_canonical_png_and_publishes_stable_android_metadata() {
        let png = fixture_png(2400, 1080);
        let expected_digest = hex::encode(Sha256::digest(&png));
        let runner = ObservationRunner::new(observation_steps(png.clone()));
        let device = device(&runner, true).await;
        let driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&device),
        });
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let store = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("evidence store"),
        );
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let runtime = DeviceRuntime::with_evidence(driver, Arc::clone(&events), evidence);
        let context = session_context(&events, device.id()).await;
        let before = now_ms();

        let observation = runtime.observe(&context).await.expect("observation");
        let after = now_ms();

        assert_eq!(
            runner.operations(),
            vec![
                AdbOperation::CaptureScreenshot,
                AdbOperation::WindowSize,
                AdbOperation::WindowDensity,
            ]
        );
        runner.assert_exhausted();
        assert_eq!(observation.device_id, *device.id());
        assert_ne!(observation.id, uuid::Uuid::nil());
        assert!((before..=after).contains(&observation.captured_at_ms));
        assert_eq!(observation.viewport.width, 2400);
        assert_eq!(observation.viewport.height, 1080);
        assert_eq!(observation.viewport.scale_factor, 3.5);
        assert_eq!(
            observation.metadata["android"],
            json!({
                "orientation": "landscape",
                "scaleFactor": 3.5,
                "densityDpi": 560,
                "physicalSize": { "width": 1080, "height": 2400 },
                "overrideSize": { "width": 720, "height": 1280 },
                "effectiveSize": { "width": 720, "height": 1280 },
                "physicalDensityDpi": 420,
                "overrideDensityDpi": 560,
            })
        );

        let screenshot = observation.screenshot.expect("screenshot reference");
        assert_eq!(screenshot.media_type, "image/png");
        assert_eq!(screenshot.sha256.as_deref(), Some(expected_digest.as_str()));
        assert_eq!(screenshot.id, format!("sha256:{expected_digest}"));
        assert_eq!(
            screenshot.uri,
            format!("devicerail://assets/sha256/{expected_digest}")
        );
        assert!(!screenshot.uri.starts_with("data:"));

        let mut stored = store.open_asset(&screenshot).await.expect("open evidence");
        let mut stored_bytes = Vec::new();
        stored
            .read_to_end(&mut stored_bytes)
            .await
            .expect("read evidence");
        assert_eq!(stored_bytes, png);
        assert_eq!(
            store.referenced_sessions().await.expect("session pins"),
            vec![context.session_id]
        );
        device
            .disconnect(&ExecutionControl::unbounded())
            .await
            .expect("completed observation releases operation gate");
        assert!(!device.device_info().await.connected);
    }

    #[tokio::test]
    async fn observations_on_one_device_remain_owned_by_their_individual_sessions() {
        let png = fixture_png(10, 20);
        let mut steps = observation_steps(png.clone());
        steps.extend(observation_steps(png.clone()));
        let runner = ObservationRunner::new(steps);
        let device = device(&runner, true).await;
        let driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&device),
        });
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let store = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("evidence store"),
        );
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let runtime = DeviceRuntime::with_evidence(driver, Arc::clone(&events), evidence);
        let first = session_context(&events, device.id()).await;
        let second = session_context(&events, device.id()).await;

        let first_observation = runtime.observe(&first).await.expect("first observation");
        let second_observation = runtime.observe(&second).await.expect("second observation");
        assert_eq!(first_observation.screenshot, second_observation.screenshot);

        let mut expected_sessions = vec![first.session_id.clone(), second.session_id.clone()];
        expected_sessions.sort();
        assert_eq!(
            store.referenced_sessions().await.expect("both pins"),
            expected_sessions
        );

        store
            .release_session(&first.session_id, now_ms())
            .await
            .expect("release first Session");
        assert_eq!(
            store
                .referenced_sessions()
                .await
                .expect("remaining Session pin"),
            vec![second.session_id.clone()]
        );
        let screenshot = second_observation.screenshot.expect("second screenshot");
        let mut stored = store
            .open_asset(&screenshot)
            .await
            .expect("shared object remains readable");
        let mut stored_bytes = Vec::new();
        stored
            .read_to_end(&mut stored_bytes)
            .await
            .expect("read shared object");
        assert_eq!(stored_bytes, png);

        store
            .release_session(&second.session_id, now_ms())
            .await
            .expect("release second Session");
        assert!(
            store
                .referenced_sessions()
                .await
                .expect("all pins released")
                .is_empty()
        );
        runner.assert_exhausted();
    }

    #[tokio::test]
    async fn disconnected_observation_is_explicit_and_runs_no_adb_command() {
        let runner = ObservationRunner::new(Vec::new());
        let device = device(&runner, false).await;
        let driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&device),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(driver, Arc::clone(&events));
        let context = session_context(&events, device.id()).await;

        let error = runtime
            .observe(&context)
            .await
            .expect_err("disconnected observation");

        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::NotConnected(device_id))
                if device_id == *device.id()
        ));
        assert_eq!(runner.call_count(), 0);
    }

    #[tokio::test]
    async fn adb_parser_and_store_failures_remain_distinct() {
        const SECRET: &str = "dr013-secret-sentinel-do-not-expose";
        let adb_runner = ObservationRunner::new(vec![Step::error(
            AdbOperation::CaptureScreenshot,
            AndroidAdbError::ProcessFailed {
                operation: AdbOperation::CaptureScreenshot.name(),
                status: Some(1),
                stderr_tail: SECRET.to_owned(),
            },
        )]);
        let adb_device = device(&adb_runner, true).await;
        let adb_driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&adb_device),
        });
        let adb_events = Arc::new(MemoryEventStore::default());
        let adb_runtime = DeviceRuntime::new(adb_driver, Arc::clone(&adb_events));
        let adb_context = session_context(&adb_events, adb_device.id()).await;
        let adb_error = adb_runtime
            .observe(&adb_context)
            .await
            .expect_err("adb error");
        assert!(matches!(
            &adb_error,
            RuntimeError::Driver(DriverError::Platform { code, retryable: true })
                if code == "android_adb_process_failed"
        ));
        assert!(!adb_error.to_string().contains(SECRET));
        let serialized_events = serde_json::to_string(
            &adb_events
                .list_after(&adb_context.session_id, None)
                .await
                .expect("adb error events"),
        )
        .expect("serialize events");
        assert!(!serialized_events.contains(SECRET));

        let parser_runner = ObservationRunner::new(observation_steps(b"not a png".to_vec()));
        let parser_device = device(&parser_runner, true).await;
        let parser_driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&parser_device),
        });
        let parser_events = Arc::new(MemoryEventStore::default());
        let parser_runtime = DeviceRuntime::new(parser_driver, Arc::clone(&parser_events));
        let parser_context = session_context(&parser_events, parser_device.id()).await;
        assert!(matches!(
            parser_runtime.observe(&parser_context).await,
            Err(RuntimeError::Driver(DriverError::Platform { code, retryable: true }))
                if code == "android_adb_malformed_png"
        ));

        let wm_runner = ObservationRunner::new(vec![
            Step::binary(AdbOperation::CaptureScreenshot, fixture_png(10, 20)),
            Step::text(AdbOperation::WindowSize, SECRET),
            Step::text(AdbOperation::WindowDensity, "Physical density: 420\n"),
        ]);
        let wm_device = device(&wm_runner, true).await;
        let wm_driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&wm_device),
        });
        let wm_events = Arc::new(MemoryEventStore::default());
        let wm_runtime = DeviceRuntime::new(wm_driver, Arc::clone(&wm_events));
        let wm_context = session_context(&wm_events, wm_device.id()).await;
        let wm_error = wm_runtime
            .observe(&wm_context)
            .await
            .expect_err("malformed wm output");
        assert!(matches!(
            &wm_error,
            RuntimeError::Driver(DriverError::Platform { code, retryable: false })
                if code == "android_adb_malformed_observation"
        ));
        assert!(!wm_error.to_string().contains(SECRET));
        let serialized_events = serde_json::to_string(
            &wm_events
                .list_after(&wm_context.session_id, None)
                .await
                .expect("wm error events"),
        )
        .expect("serialize events");
        assert!(!serialized_events.contains(SECRET));

        let store_runner = ObservationRunner::new(observation_steps(fixture_png(10, 20)));
        let store_device = device(&store_runner, true).await;
        let store_driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&store_device),
        });
        let store_events = Arc::new(MemoryEventStore::default());
        let store_runtime = DeviceRuntime::new(store_driver, Arc::clone(&store_events));
        let store_context = session_context(&store_events, store_device.id()).await;
        assert!(matches!(
            store_runtime.observe(&store_context).await,
            Err(RuntimeError::Evidence(EvidenceError::Unavailable))
        ));
    }

    #[tokio::test]
    async fn cancellation_after_each_adb_result_is_checked_before_evidence_put() {
        for cancel_after in [
            AdbOperation::CaptureScreenshot,
            AdbOperation::WindowSize,
            AdbOperation::WindowDensity,
        ] {
            let (controller, control) = ExecutionController::new();
            let runner = ObservationRunner::cancelling_after(
                observation_steps(fixture_png(10, 20)),
                cancel_after.clone(),
                controller,
            );
            let device = device(&runner, true).await;
            let driver = Arc::new(ObservationTestDriver {
                device: Arc::clone(&device),
            });
            let events = Arc::new(MemoryEventStore::default());
            let store = Arc::new(EagerEvidenceStore::new(None));
            let evidence: Arc<dyn EvidenceStore> = store.clone();
            let runtime = DeviceRuntime::with_evidence(driver, Arc::clone(&events), evidence);
            let context = session_context(&events, device.id())
                .await
                .with_control(control);

            assert!(matches!(
                runtime.observe(&context).await,
                Err(RuntimeError::Cancelled {
                    reason: CancellationReason::Requested
                })
            ));
            assert_eq!(
                store.put_count(),
                0,
                "{cancel_after:?} cancellation must precede Store I/O"
            );
            let events = events
                .list_after(&context.session_id, None)
                .await
                .expect("events");
            assert!(events.iter().all(|event| !matches!(
                event.payload,
                TestEventPayload::ObservationCaptured { .. }
            )));
        }
    }

    #[tokio::test]
    async fn cancellation_synchronously_signalled_by_successful_put_is_not_published() {
        let (controller, control) = ExecutionController::new();
        let runner = ObservationRunner::new(observation_steps(fixture_png(10, 20)));
        let device = device(&runner, true).await;
        let driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&device),
        });
        let events = Arc::new(MemoryEventStore::default());
        let store = Arc::new(EagerEvidenceStore::new(Some(controller)));
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let runtime = DeviceRuntime::with_evidence(driver, Arc::clone(&events), evidence);
        let context = session_context(&events, device.id())
            .await
            .with_control(control);

        assert!(matches!(
            runtime.observe(&context).await,
            Err(RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            })
        ));
        assert_eq!(store.put_count(), 1);
        let events = events
            .list_after(&context.session_id, None)
            .await
            .expect("events");
        assert!(
            events.iter().all(|event| !matches!(
                event.payload,
                TestEventPayload::ObservationCaptured { .. }
            ))
        );
    }

    #[tokio::test]
    async fn observation_read_gate_blocks_lifecycle_writers_at_every_adb_phase() {
        for blocked_operation in [
            AdbOperation::CaptureScreenshot,
            AdbOperation::WindowSize,
            AdbOperation::WindowDensity,
        ] {
            let runner = BlockingPhaseRunner::new(blocked_operation.clone());
            let runner_trait: Arc<dyn AdbCommandRunner> = runner.clone();
            let device = device_with_runner(runner_trait, true).await;
            let driver = Arc::new(ObservationTestDriver {
                device: Arc::clone(&device),
            });
            let events = Arc::new(MemoryEventStore::default());
            let runtime = Arc::new(DeviceRuntime::new(driver, Arc::clone(&events)));
            let (observe_controller, observe_control) = ExecutionController::new();
            let context = session_context(&events, device.id())
                .await
                .with_control(observe_control);
            let observe = tokio::spawn({
                let runtime = Arc::clone(&runtime);
                let context = context.clone();
                async move { runtime.observe(&context).await }
            });

            tokio::time::timeout(Duration::from_secs(1), runner.started.notified())
                .await
                .expect("observation reaches blocked ADB phase");
            let info = tokio::time::timeout(Duration::from_millis(100), device.device_info())
                .await
                .expect("device_info does not wait for operation gate");
            assert!(info.connected);

            let (_, health_control) =
                ExecutionController::with_timeout(5, devicerail_core::TimeoutScope::Request);
            assert!(matches!(
                tokio::time::timeout(Duration::from_millis(250), device.health(&health_control))
                    .await
                    .expect("health exits on its own deadline"),
                Err(AndroidAdbError::TimedOut {
                    operation: "health"
                })
            ));
            assert!(device.device_info().await.connected);

            let (disconnect_controller, disconnect_control) = ExecutionController::new();
            let disconnect = tokio::spawn({
                let device = Arc::clone(&device);
                async move { device.disconnect(&disconnect_control).await }
            });
            tokio::task::yield_now().await;
            assert!(!disconnect.is_finished());
            assert!(disconnect_controller.cancel(CancellationReason::Requested));
            assert!(matches!(
                tokio::time::timeout(Duration::from_millis(250), disconnect)
                    .await
                    .expect("disconnect exits on cancellation")
                    .expect("disconnect task"),
                Err(AndroidAdbError::Cancelled)
            ));
            assert!(device.device_info().await.connected);

            assert!(observe_controller.cancel(CancellationReason::Requested));
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(1), observe)
                    .await
                    .expect("observation exits on cancellation")
                    .expect("observation task"),
                Err(RuntimeError::Cancelled {
                    reason: CancellationReason::Requested
                })
            ));

            device
                .disconnect(&ExecutionControl::unbounded())
                .await
                .expect("read gate releases after cancellation");
            assert!(!device.device_info().await.connected);
        }
    }

    #[tokio::test]
    async fn observation_deadline_interrupts_every_blocked_adb_phase_and_releases_gate() {
        for blocked_operation in [
            AdbOperation::CaptureScreenshot,
            AdbOperation::WindowSize,
            AdbOperation::WindowDensity,
        ] {
            let runner = BlockingPhaseRunner::new(blocked_operation.clone());
            let runner_trait: Arc<dyn AdbCommandRunner> = runner.clone();
            let device = device_with_runner(runner_trait, true).await;
            let driver = Arc::new(ObservationTestDriver {
                device: Arc::clone(&device),
            });
            let events = Arc::new(MemoryEventStore::default());
            let runtime = DeviceRuntime::new(driver, Arc::clone(&events));
            let (_, control) =
                ExecutionController::with_timeout(5, devicerail_core::TimeoutScope::Request);
            let context = session_context(&events, device.id())
                .await
                .with_control(control);

            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(1), runtime.observe(&context))
                    .await
                    .expect("Core observation deadline fires"),
                Err(RuntimeError::TimedOut {
                    scope: devicerail_core::TimeoutScope::Request,
                    timeout_ms: 5
                })
            ));
            assert!(
                runner
                    .calls
                    .lock()
                    .expect("phase calls")
                    .contains(&blocked_operation)
            );
            device
                .disconnect(&ExecutionControl::unbounded())
                .await
                .expect("deadline releases observation gate");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 6)]
    async fn observations_are_bounded_before_starting_adb_capture() {
        let runner = BoundedObservationRunner::new();
        let runner_trait: Arc<dyn AdbCommandRunner> = runner.clone();
        let device = device_with_runner(runner_trait, true).await;
        let driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&device),
        });
        let events = Arc::new(MemoryEventStore::default());
        let store = Arc::new(EagerEvidenceStore::new(None));
        let evidence: Arc<dyn EvidenceStore> = store;
        let runtime = Arc::new(DeviceRuntime::with_evidence(
            driver,
            Arc::clone(&events),
            evidence,
        ));
        let context = session_context(&events, device.id()).await;
        let mut observations = Vec::new();
        for _ in 0..(MAX_CONCURRENT_OBSERVATIONS_PER_DEVICE + 1) {
            let runtime = Arc::clone(&runtime);
            let context = context.clone();
            observations.push(tokio::spawn(async move { runtime.observe(&context).await }));
        }

        tokio::time::timeout(
            Duration::from_secs(1),
            runner.wait_for_started(MAX_CONCURRENT_OBSERVATIONS_PER_DEVICE),
        )
        .await
        .expect("the configured capture slots start");
        assert_eq!(
            runner.started_captures.load(Ordering::SeqCst),
            MAX_CONCURRENT_OBSERVATIONS_PER_DEVICE
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), runner.wait_for_started(5))
                .await
                .is_err(),
            "the fifth capture must wait before invoking ADB"
        );

        runner.release_all();
        for observation in observations {
            observation
                .await
                .expect("observation task")
                .expect("bounded observation");
        }
        assert_eq!(runner.started_captures.load(Ordering::SeqCst), 5);
        assert_eq!(
            runner.max_active_captures.load(Ordering::SeqCst),
            MAX_CONCURRENT_OBSERVATIONS_PER_DEVICE
        );
    }

    struct PutDropGuard(Arc<AtomicBool>);

    impl Drop for PutDropGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct BlockingEvidenceStore {
        started: tokio::sync::Notify,
        put_dropped: Arc<AtomicBool>,
        session: StdMutex<Option<SessionId>>,
    }

    impl BlockingEvidenceStore {
        fn new() -> Self {
            Self {
                started: tokio::sync::Notify::new(),
                put_dropped: Arc::new(AtomicBool::new(false)),
                session: StdMutex::new(None),
            }
        }
    }

    #[async_trait]
    impl EvidenceStore for BlockingEvidenceStore {
        async fn put(
            &self,
            request: PutEvidence,
            _input: EvidenceInput,
        ) -> EvidenceResult<StoredEvidence> {
            *self.session.lock().expect("session lock") = Some(request.session_id().clone());
            let _guard = PutDropGuard(Arc::clone(&self.put_dropped));
            self.started.notify_one();
            pending::<()>().await;
            unreachable!("blocking evidence put completed")
        }

        async fn attach(
            &self,
            _session_id: &SessionId,
            _asset: &devicerail_protocol::AssetRef,
        ) -> EvidenceResult<StoredEvidence> {
            Err(EvidenceError::Internal("unexpected attach".to_owned()))
        }

        async fn verify_session_reference(
            &self,
            _session_id: &SessionId,
            _asset: &devicerail_protocol::AssetRef,
        ) -> EvidenceResult<EvidenceMetadata> {
            Err(EvidenceError::Internal(
                "unexpected session reference verification".to_owned(),
            ))
        }

        async fn open(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
            Err(EvidenceError::Internal("unexpected open".to_owned()))
        }

        async fn metadata(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
            Err(EvidenceError::Internal("unexpected metadata".to_owned()))
        }

        async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
            Ok(Vec::new())
        }

        async fn release_session(
            &self,
            _session_id: &SessionId,
            _released_at_ms: u64,
        ) -> EvidenceResult<ReleaseReport> {
            Err(EvidenceError::Internal(
                "unexpected release_session".to_owned(),
            ))
        }

        async fn gc(&self, _policy: GcPolicy) -> EvidenceResult<GcReport> {
            Err(EvidenceError::Internal("unexpected gc".to_owned()))
        }
    }

    struct EagerEvidenceStore {
        puts: AtomicUsize,
        cancel_after_put: Option<ExecutionController>,
    }

    impl EagerEvidenceStore {
        fn new(cancel_after_put: Option<ExecutionController>) -> Self {
            Self {
                puts: AtomicUsize::new(0),
                cancel_after_put,
            }
        }

        fn put_count(&self) -> usize {
            self.puts.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EvidenceStore for EagerEvidenceStore {
        async fn put(
            &self,
            request: PutEvidence,
            _input: EvidenceInput,
        ) -> EvidenceResult<StoredEvidence> {
            self.puts.fetch_add(1, Ordering::SeqCst);
            if let Some(controller) = &self.cancel_after_put {
                controller.cancel(CancellationReason::Requested);
            }
            let metadata = EvidenceMetadata::new(
                Sha256Digest::parse("0".repeat(64))?,
                request.media_type(),
                request.declared_size_bytes().unwrap_or_default(),
                now_ms(),
                1,
            )?;
            Ok(StoredEvidence::new(metadata, false))
        }

        async fn attach(
            &self,
            _session_id: &SessionId,
            _asset: &devicerail_protocol::AssetRef,
        ) -> EvidenceResult<StoredEvidence> {
            Err(EvidenceError::Internal("unexpected attach".to_owned()))
        }

        async fn verify_session_reference(
            &self,
            _session_id: &SessionId,
            _asset: &devicerail_protocol::AssetRef,
        ) -> EvidenceResult<EvidenceMetadata> {
            Err(EvidenceError::Internal(
                "unexpected session reference verification".to_owned(),
            ))
        }

        async fn open(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
            Err(EvidenceError::Internal("unexpected open".to_owned()))
        }

        async fn metadata(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
            Err(EvidenceError::Internal("unexpected metadata".to_owned()))
        }

        async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
            Ok(Vec::new())
        }

        async fn release_session(
            &self,
            session_id: &SessionId,
            _released_at_ms: u64,
        ) -> EvidenceResult<ReleaseReport> {
            Ok(ReleaseReport {
                session_id: session_id.clone(),
                released_references: 0,
                newly_unreferenced_assets: 0,
                newly_unreferenced_bytes: 0,
            })
        }

        async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
            Ok(GcReport {
                dry_run: policy.dry_run,
                ..GcReport::default()
            })
        }
    }

    #[tokio::test]
    async fn cancellation_during_store_never_publishes_a_false_observation() {
        let runner = ObservationRunner::new(observation_steps(fixture_png(10, 20)));
        let device = device(&runner, true).await;
        let driver = Arc::new(ObservationTestDriver {
            device: Arc::clone(&device),
        });
        let events = Arc::new(MemoryEventStore::default());
        let store = Arc::new(BlockingEvidenceStore::new());
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let runtime = Arc::new(DeviceRuntime::with_evidence(
            driver,
            Arc::clone(&events),
            evidence,
        ));
        let (controller, control) = ExecutionController::new();
        let context = session_context(&events, device.id())
            .await
            .with_control(control);
        let task = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            let context = context.clone();
            async move { runtime.observe(&context).await }
        });

        tokio::time::timeout(Duration::from_secs(1), store.started.notified())
            .await
            .expect("Store write starts");
        assert_eq!(
            *store.session.lock().expect("session lock"),
            Some(context.session_id.clone())
        );
        assert!(device.device_info().await.connected);

        let (_, health_control) =
            ExecutionController::with_timeout(5, devicerail_core::TimeoutScope::Request);
        assert!(matches!(
            device.health(&health_control).await,
            Err(AndroidAdbError::TimedOut {
                operation: "health"
            })
        ));
        let (disconnect_controller, disconnect_control) = ExecutionController::new();
        let disconnect = tokio::spawn({
            let device = Arc::clone(&device);
            async move { device.disconnect(&disconnect_control).await }
        });
        tokio::task::yield_now().await;
        assert!(!disconnect.is_finished());
        assert!(disconnect_controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            disconnect.await.expect("disconnect task"),
            Err(AndroidAdbError::Cancelled)
        ));
        assert!(device.device_info().await.connected);

        assert!(controller.cancel(CancellationReason::Requested));
        let error = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("observation stops")
            .expect("observation task")
            .expect_err("cancelled observation");
        assert!(matches!(
            error,
            RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            }
        ));
        assert!(store.put_dropped.load(Ordering::SeqCst));

        let events = events
            .list_after(&context.session_id, Some(EventSequence::FIRST))
            .await
            .expect("events");
        assert!(
            events.iter().all(|event| !matches!(
                event.payload,
                TestEventPayload::ObservationCaptured { .. }
            ))
        );
        assert!(matches!(
            events.as_slice(),
            [event] if matches!(event.payload, TestEventPayload::Error { .. })
        ));
        device
            .disconnect(&ExecutionControl::unbounded())
            .await
            .expect("Store cancellation releases observation gate");
        assert!(!device.device_info().await.connected);
        runner.assert_exhausted();
    }
}
