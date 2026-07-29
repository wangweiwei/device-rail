use std::{
    collections::{BTreeMap, btree_map::Entry as MapEntry},
    fmt,
    sync::Arc,
};

use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, DeviceId, DeviceInfo, ErrorInfo,
    Observation,
};
use serde_json::json;
use thiserror::Error;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::{
    DeviceAccessGuard, DeviceDriver, DeviceLease, DevicePool, DevicePoolConfig, DevicePoolEntry,
    DevicePoolError, DevicePoolResult, DeviceRegistrationToken, DeviceRuntime, DriverError,
    EvidenceStore, ExecutionControl, LeaseId, LeaseOwnerId, OperationContext, PoolHealth,
    RuntimeError, RuntimeResult, ScreenshotPolicy, SessionEventStore, UnavailableEvidenceStore,
    inactive_control_error, timeout_error,
};

pub type RegistryResult<T> = Result<T, RegistryError>;

/// Stable failures produced while registering or resolving device Drivers.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("device id must not be empty: {0:?}")]
    InvalidDeviceId(DeviceId),
    #[error("device name must not be empty for {0}")]
    InvalidDeviceName(DeviceId),
    #[error("driver id {driver_id} does not match device info id {info_id}")]
    DeviceIdMismatch {
        driver_id: DeviceId,
        info_id: DeviceId,
    },
    #[error("device is already registered: {0}")]
    DeviceAlreadyRegistered(DeviceId),
    #[error("device is not registered: {0}")]
    DeviceNotFound(DeviceId),
    #[error("cannot unregister a leased device: {0}")]
    DeviceLeased(DeviceId),
    #[error("registry and device pool inventory are inconsistent for {0}")]
    InventoryInconsistent(DeviceId),
    #[error("no devices are registered")]
    NoDevicesRegistered,
    #[error("a device must be selected because {count} devices are registered")]
    DeviceSelectionRequired { count: usize },
}

impl RegistryError {
    pub fn to_error_info(&self) -> ErrorInfo {
        let (code, retryable, details) = match self {
            Self::InvalidDeviceId(device_id) => (
                "invalid_device_id",
                false,
                Some(json!({ "deviceId": device_id })),
            ),
            Self::InvalidDeviceName(device_id) => (
                "invalid_device_name",
                false,
                Some(json!({ "deviceId": device_id })),
            ),
            Self::DeviceIdMismatch { driver_id, info_id } => (
                "device_id_mismatch",
                false,
                Some(json!({ "driverId": driver_id, "deviceInfoId": info_id })),
            ),
            Self::DeviceAlreadyRegistered(device_id) => (
                "device_already_registered",
                false,
                Some(json!({ "deviceId": device_id })),
            ),
            Self::DeviceNotFound(device_id) => (
                "device_not_found",
                false,
                Some(json!({ "deviceId": device_id })),
            ),
            Self::DeviceLeased(device_id) => (
                "device_leased",
                true,
                Some(json!({ "deviceId": device_id })),
            ),
            Self::InventoryInconsistent(device_id) => (
                "registry_inventory_inconsistent",
                false,
                Some(json!({ "deviceId": device_id })),
            ),
            Self::NoDevicesRegistered => ("no_devices_registered", true, None),
            Self::DeviceSelectionRequired { count } => (
                "device_selection_required",
                false,
                Some(json!({ "deviceCount": count })),
            ),
        };

        ErrorInfo {
            code: code.to_owned(),
            message: self.to_string(),
            retryable,
            details,
        }
    }
}

struct Entry<S: ?Sized> {
    runtime: DeviceRuntime<dyn DeviceDriver, S>,
    registration: DeviceRegistrationToken,
    info: RwLock<DeviceInfo>,
    lifecycle: RwLock<()>,
}

/// A cloneable, fixed route to one registered Driver.
///
/// All device calls stay behind the per-device lifecycle gate. The underlying
/// runtime is deliberately not exposed so callers cannot bypass that gate.
pub struct DriverHandle<S: ?Sized> {
    entry: Arc<Entry<S>>,
}

impl<S: ?Sized> Clone for DriverHandle<S> {
    fn clone(&self) -> Self {
        Self {
            entry: Arc::clone(&self.entry),
        }
    }
}

impl<S> fmt::Debug for DriverHandle<S>
where
    S: SessionEventStore + ?Sized,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DriverHandle")
            .field("device_id", self.id())
            .finish_non_exhaustive()
    }
}

impl<S> DriverHandle<S>
where
    S: SessionEventStore + ?Sized,
{
    pub fn id(&self) -> &DeviceId {
        self.entry.runtime.device_id()
    }

    pub fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.entry.runtime.action_protection(name)
    }

    /// Returns the latest registry-owned device metadata snapshot.
    pub async fn info(&self) -> DeviceInfo {
        self.entry.info.read().await.clone()
    }

    /// Connects exclusively with respect to all other operations on this
    /// device, then publishes the returned metadata as the new snapshot.
    async fn connect(&self, control: &ExecutionControl) -> RuntimeResult<DeviceInfo> {
        let _lifecycle = acquire_write(&self.entry.lifecycle, control).await?;
        let info = self.entry.runtime.connect(control).await?;
        if info.id != *self.id() {
            return Err(DriverError::Protocol(format!(
                "connected device id {} does not match registered device {}",
                info.id,
                self.id()
            ))
            .into());
        }
        *self.entry.info.write().await = info.clone();
        Ok(info)
    }

    /// Disconnects exclusively and marks the snapshot disconnected only
    /// after the Driver confirms success.
    async fn disconnect(&self, control: &ExecutionControl) -> RuntimeResult<()> {
        let _lifecycle = acquire_write(&self.entry.lifecycle, control).await?;
        self.entry.runtime.disconnect(control).await?;
        self.entry.info.write().await.connected = false;
        Ok(())
    }

    pub async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> RuntimeResult<Vec<ActionDefinition>> {
        let _lifecycle = acquire_read(&self.entry.lifecycle, control).await?;
        self.entry.runtime.capabilities(control).await
    }

    /// Runs the Driver's non-mutating liveness probe. This is intentionally
    /// available on a raw route so the Pool can refresh health before lease
    /// acquisition; it must never perform device input or capture evidence.
    pub async fn health_check(&self, control: &ExecutionControl) -> RuntimeResult<()> {
        let _lifecycle = acquire_read(&self.entry.lifecycle, control).await?;
        self.entry.runtime.health_check(control).await
    }

    async fn observe(&self, context: &OperationContext) -> RuntimeResult<Observation> {
        let _lifecycle = acquire_read(&self.entry.lifecycle, &context.control).await?;
        self.entry.runtime.observe(context).await
    }

    async fn execute(
        &self,
        context: &OperationContext,
        call: ActionCall,
    ) -> RuntimeResult<ActionResult> {
        // Only the parent Request control governs time spent in this queue.
        // DeviceRuntime derives the Action control after ActionStarted is
        // durable, so the action-specific budget starts inside the runtime.
        let _lifecycle = acquire_read(&self.entry.lifecycle, &context.control).await?;
        self.entry.runtime.execute(context, call).await
    }
}

/// A Driver route pinned to a Core-issued Device Pool operation guard.
///
/// Mutation and observation methods are intentionally available only here;
/// resolving a raw [`DriverHandle`] cannot bypass lease arbitration.
pub struct DriverAccess<S: ?Sized> {
    handle: DriverHandle<S>,
    _access: DeviceAccessGuard,
}

/// A Pool-pinned route that intentionally exposes lifecycle operations only.
/// It cannot observe or execute without an explicit lease.
pub struct DriverLifecycleAccess<S: ?Sized> {
    handle: DriverHandle<S>,
    _access: DeviceAccessGuard,
}

impl<S> fmt::Debug for DriverAccess<S>
where
    S: SessionEventStore + ?Sized,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DriverAccess")
            .field("device_id", self.handle.id())
            .finish_non_exhaustive()
    }
}

impl<S> DriverAccess<S>
where
    S: SessionEventStore + ?Sized,
{
    pub fn id(&self) -> &DeviceId {
        self.handle.id()
    }

    pub fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.handle.action_protection(name)
    }

    pub async fn info(&self) -> DeviceInfo {
        self.handle.info().await
    }

    pub async fn connect(&self, control: &ExecutionControl) -> RuntimeResult<DeviceInfo> {
        self.handle.connect(control).await
    }

    pub async fn disconnect(&self, control: &ExecutionControl) -> RuntimeResult<()> {
        self.handle.disconnect(control).await
    }

    pub async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> RuntimeResult<Vec<ActionDefinition>> {
        self.handle.capabilities(control).await
    }

    pub async fn health_check(&self, control: &ExecutionControl) -> RuntimeResult<()> {
        self.handle.health_check(control).await
    }

    pub async fn observe(&self, context: &OperationContext) -> RuntimeResult<Observation> {
        self.handle.observe(context).await
    }

    pub async fn execute(
        &self,
        context: &OperationContext,
        call: ActionCall,
    ) -> RuntimeResult<ActionResult> {
        self.handle.execute(context, call).await
    }
}

impl<S> fmt::Debug for DriverLifecycleAccess<S>
where
    S: SessionEventStore + ?Sized,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DriverLifecycleAccess")
            .field("device_id", self.handle.id())
            .finish_non_exhaustive()
    }
}

impl<S> DriverLifecycleAccess<S>
where
    S: SessionEventStore + ?Sized,
{
    pub fn id(&self) -> &DeviceId {
        self.handle.id()
    }

    pub async fn info(&self) -> DeviceInfo {
        self.handle.info().await
    }

    pub async fn connect(&self, control: &ExecutionControl) -> RuntimeResult<DeviceInfo> {
        self.handle.connect(control).await
    }

    pub async fn disconnect(&self, control: &ExecutionControl) -> RuntimeResult<()> {
        self.handle.disconnect(control).await
    }
}

/// Registry for heterogeneous Drivers sharing one session event store.
pub struct DriverRegistry<S: ?Sized> {
    events: Arc<S>,
    evidence: Arc<dyn EvidenceStore>,
    strict_evidence_receipts: bool,
    screenshot_policy: ScreenshotPolicy,
    pool: DevicePool,
    entries: RwLock<BTreeMap<DeviceId, Arc<Entry<S>>>>,
}

impl<S> DriverRegistry<S>
where
    S: SessionEventStore + ?Sized,
{
    /// Creates a Registry whose routes explicitly reject evidence writes.
    /// Evidence-producing Drivers require [`Self::with_evidence`].
    pub fn new(events: Arc<S>) -> Self {
        Self::new_with_pool_config(events, DevicePoolConfig::default())
            .expect("default Device Pool configuration is valid")
    }

    pub fn new_with_pool_config(
        events: Arc<S>,
        pool_config: DevicePoolConfig,
    ) -> DevicePoolResult<Self> {
        Ok(Self {
            events,
            evidence: Arc::new(UnavailableEvidenceStore),
            strict_evidence_receipts: false,
            screenshot_policy: ScreenshotPolicy::Capture,
            pool: DevicePool::new(pool_config)?,
            entries: RwLock::new(BTreeMap::new()),
        })
    }

    /// Creates a Registry that shares one Evidence Store across every route;
    /// each runtime operation still receives only a Session-bound writer and
    /// must reconcile successful results with that writer's receipt set.
    pub fn with_evidence(events: Arc<S>, evidence: Arc<dyn EvidenceStore>) -> Self {
        Self::with_evidence_and_pool_config(events, evidence, DevicePoolConfig::default())
            .expect("default Device Pool configuration is valid")
    }

    pub fn with_evidence_and_pool_config(
        events: Arc<S>,
        evidence: Arc<dyn EvidenceStore>,
        pool_config: DevicePoolConfig,
    ) -> DevicePoolResult<Self> {
        Ok(Self {
            events,
            evidence,
            strict_evidence_receipts: true,
            screenshot_policy: ScreenshotPolicy::Capture,
            pool: DevicePool::new(pool_config)?,
            entries: RwLock::new(BTreeMap::new()),
        })
    }

    pub const fn with_screenshot_policy(mut self, screenshot_policy: ScreenshotPolicy) -> Self {
        self.screenshot_policy = screenshot_policy;
        self
    }

    pub const fn screenshot_policy(&self) -> ScreenshotPolicy {
        self.screenshot_policy
    }

    /// Clones the shared Session event-store boundary for Core services.
    ///
    /// This deliberately exposes only the Session event-store boundary, not
    /// a raw Driver handle or a way around device lease arbitration.
    pub fn event_store(&self) -> Arc<S> {
        Arc::clone(&self.events)
    }

    /// Registers a Driver without replacing any existing route.
    pub async fn register(
        &self,
        driver: Arc<dyn DeviceDriver>,
        info: DeviceInfo,
    ) -> RegistryResult<DriverHandle<S>> {
        let driver_id = driver.id().clone();
        validate_registration(&driver_id, &info)?;

        let runtime = if self.strict_evidence_receipts {
            DeviceRuntime::with_evidence(
                driver,
                Arc::clone(&self.events),
                Arc::clone(&self.evidence),
            )
        } else {
            DeviceRuntime::new(driver, Arc::clone(&self.events))
        }
        .with_screenshot_policy(self.screenshot_policy);
        let mut entries = self.entries.write().await;
        match entries.entry(driver_id.clone()) {
            MapEntry::Vacant(slot) => {
                let registration = self
                    .pool
                    .register(driver_id.clone())
                    .await
                    .map_err(|_| RegistryError::DeviceAlreadyRegistered(driver_id.clone()))?;
                let entry = Arc::new(Entry {
                    runtime,
                    registration,
                    info: RwLock::new(info),
                    lifecycle: RwLock::new(()),
                });
                slot.insert(Arc::clone(&entry));
                Ok(DriverHandle { entry })
            }
            MapEntry::Occupied(_) => Err(RegistryError::DeviceAlreadyRegistered(driver_id)),
        }
    }

    /// Returns metadata in stable DeviceId order.
    pub async fn list(&self) -> Vec<DeviceInfo> {
        let entries = self.entries.read().await;
        let mut devices = Vec::with_capacity(entries.len());
        for entry in entries.values() {
            devices.push(entry.info.read().await.clone());
        }
        devices
    }

    /// Removes a route and its Device Pool record as one Registry mutation.
    ///
    /// The Registry write lock prevents registration, resolution, health
    /// publication, and new operation admission from observing only one side
    /// of the mutation. Existing operation guards must finish first, and an
    /// active lease makes removal fail without changing either inventory.
    pub async fn unregister(&self, device_id: &DeviceId, now_ms: u64) -> RegistryResult<()> {
        let mut entries = self.entries.write().await;
        if !entries.contains_key(device_id) {
            return Err(RegistryError::DeviceNotFound(device_id.clone()));
        }

        self.pool
            .remove(device_id, now_ms)
            .await
            .map_err(|error| unregister_error(device_id, error))?;
        let removed = entries.remove(device_id);
        debug_assert!(removed.is_some(), "entry was checked under the write lock");
        Ok(())
    }

    /// Resolves a route and releases the map lock before returning it.
    pub async fn resolve(&self, device_id: &DeviceId) -> RegistryResult<DriverHandle<S>> {
        let entry = self.entries.read().await.get(device_id).cloned();
        entry
            .map(|entry| DriverHandle { entry })
            .ok_or_else(|| RegistryError::DeviceNotFound(device_id.clone()))
    }

    pub async fn action_protection(
        &self,
        device_id: &DeviceId,
        name: &str,
    ) -> RegistryResult<Option<ActionProtection>> {
        self.resolve(device_id)
            .await
            .map(|handle| handle.action_protection(name))
    }

    /// Resolves the sole registered device for backwards-compatible clients.
    pub async fn sole(&self) -> RegistryResult<DriverHandle<S>> {
        let entries = self.entries.read().await;
        match entries.len() {
            0 => Err(RegistryError::NoDevicesRegistered),
            1 => Ok(DriverHandle {
                entry: Arc::clone(entries.values().next().expect("one registry entry")),
            }),
            count => Err(RegistryError::DeviceSelectionRequired { count }),
        }
    }

    /// Takes a stable ordered snapshot of all routes. No returned handle keeps
    /// the registry map locked while it waits on a device.
    pub async fn handles(&self) -> Vec<DriverHandle<S>> {
        self.entries
            .read()
            .await
            .values()
            .map(|entry| DriverHandle {
                entry: Arc::clone(entry),
            })
            .collect()
    }

    /// Records one bounded health probe result for a registered device.
    pub async fn record_health(
        &self,
        handle: &DriverHandle<S>,
        health: PoolHealth,
        now_ms: u64,
    ) -> DevicePoolResult<PoolHealth> {
        let entries = self.entries.read().await;
        ensure_current_entry(&entries, handle)?;
        let result = self
            .pool
            .record_health(&handle.entry.registration, health, now_ms)
            .await;
        drop(entries);
        result
    }

    /// Atomically acquires or renews the owner's exclusive device lease.
    pub async fn acquire_lease(
        &self,
        handle: &DriverHandle<S>,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DeviceLease> {
        let entries = self.entries.read().await;
        ensure_current_entry(&entries, handle)?;
        let result = self.pool.acquire(handle.id(), owner_id, now_ms).await;
        drop(entries);
        result
    }

    pub async fn access_with_lease(
        &self,
        handle: DriverHandle<S>,
        lease_id: LeaseId,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DriverAccess<S>> {
        let entries = self.entries.read().await;
        ensure_current_entry(&entries, &handle)?;
        let access = self
            .pool
            .access_with_lease(lease_id, handle.id(), owner_id, now_ms)
            .await?;
        drop(entries);
        Ok(DriverAccess {
            handle,
            _access: access,
        })
    }

    pub async fn access_available_to(
        &self,
        handle: DriverHandle<S>,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DriverLifecycleAccess<S>> {
        let entries = self.entries.read().await;
        ensure_current_entry(&entries, &handle)?;
        let access = self
            .pool
            .access_available_to(handle.id(), owner_id, now_ms)
            .await?;
        drop(entries);
        Ok(DriverLifecycleAccess {
            handle,
            _access: access,
        })
    }

    pub async fn cleanup_access_with_lease(
        &self,
        handle: DriverHandle<S>,
        lease_id: LeaseId,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DriverLifecycleAccess<S>> {
        let entries = self.entries.read().await;
        ensure_current_entry(&entries, &handle)?;
        let access = self
            .pool
            .access_with_lease_for_cleanup(lease_id, handle.id(), owner_id, now_ms)
            .await?;
        drop(entries);
        Ok(DriverLifecycleAccess {
            handle,
            _access: access,
        })
    }

    pub async fn release_lease(
        &self,
        lease_id: LeaseId,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DeviceLease> {
        let _entries = self.entries.read().await;
        self.pool.release(lease_id, owner_id, now_ms).await
    }

    pub async fn release_owner_leases(
        &self,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> Vec<DeviceLease> {
        let _entries = self.entries.read().await;
        self.pool.release_owner(owner_id, now_ms).await
    }

    pub async fn release_all_leases(&self, now_ms: u64) -> Vec<DeviceLease> {
        let _entries = self.entries.read().await;
        self.pool.release_all(now_ms).await
    }

    pub async fn pool_entries(&self, now_ms: u64) -> Vec<DevicePoolEntry> {
        let _entries = self.entries.read().await;
        self.pool.entries(now_ms).await
    }
}

fn ensure_current_entry<S: SessionEventStore + ?Sized>(
    entries: &BTreeMap<DeviceId, Arc<Entry<S>>>,
    handle: &DriverHandle<S>,
) -> DevicePoolResult<()> {
    match entries.get(handle.id()) {
        Some(entry) if Arc::ptr_eq(entry, &handle.entry) => Ok(()),
        Some(_) => Err(DevicePoolError::DeviceRegistrationChanged(
            handle.id().clone(),
        )),
        None => Err(DevicePoolError::DeviceNotFound(handle.id().clone())),
    }
}

fn unregister_error(device_id: &DeviceId, error: DevicePoolError) -> RegistryError {
    match error {
        DevicePoolError::DeviceLeased(_) => RegistryError::DeviceLeased(device_id.clone()),
        _ => RegistryError::InventoryInconsistent(device_id.clone()),
    }
}

fn validate_registration(driver_id: &DeviceId, info: &DeviceInfo) -> RegistryResult<()> {
    if driver_id.0.trim().is_empty() {
        return Err(RegistryError::InvalidDeviceId(driver_id.clone()));
    }
    if info.id.0.trim().is_empty() {
        return Err(RegistryError::InvalidDeviceId(info.id.clone()));
    }
    if info.name.trim().is_empty() {
        return Err(RegistryError::InvalidDeviceName(info.id.clone()));
    }
    if info.id != *driver_id {
        return Err(RegistryError::DeviceIdMismatch {
            driver_id: driver_id.clone(),
            info_id: info.id.clone(),
        });
    }
    Ok(())
}

async fn acquire_read<'a>(
    lifecycle: &'a RwLock<()>,
    control: &ExecutionControl,
) -> RuntimeResult<RwLockReadGuard<'a, ()>> {
    if let Some(error) = inactive_control_error(control) {
        return Err(error);
    }

    tokio::select! {
        biased;
        guard = lifecycle.read() => Ok(guard),
        reason = control.cancelled() => Err(RuntimeError::Cancelled { reason }),
        () = control.deadline_elapsed() => Err(timeout_error(control)),
    }
}

async fn acquire_write<'a>(
    lifecycle: &'a RwLock<()>,
    control: &ExecutionControl,
) -> RuntimeResult<RwLockWriteGuard<'a, ()>> {
    if let Some(error) = inactive_control_error(control) {
        return Err(error);
    }

    tokio::select! {
        biased;
        guard = lifecycle.write() => Ok(guard),
        reason = control.cancelled() => Err(RuntimeError::Cancelled { reason }),
        () = control.deadline_elapsed() => Err(timeout_error(control)),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use devicerail_protocol::{
        ActionCall, ActionDefinition, ActionProtection, ActionResult, DeviceId, DeviceInfo,
        Observation, Platform, ScreenshotOmissionReason, Viewport,
    };
    use serde_json::{Map, Value, json};
    use tokio::sync::Notify;
    use uuid::Uuid;

    use super::{DriverHandle, DriverRegistry, RegistryError};
    use crate::{
        CancellationReason, DeviceDriver, DeviceOperationResult, DevicePoolError, DriverError,
        DriverOperationContext, DriverResult, ExecutionControl, ExecutionController, LeaseOwnerId,
        MemoryEventStore, OperationContext, PoolHealth, PoolHealthState, RuntimeError,
        ScreenshotPolicy, SessionEventStore, StartSession, TimeoutScope, now_ms,
    };

    #[derive(Default)]
    struct ConnectGate {
        entered: Notify,
        released: Notify,
    }

    struct TestDriver {
        id: DeviceId,
        connect_info: Mutex<DeviceInfo>,
        connect_gate: Option<Arc<ConnectGate>>,
        capability_calls: AtomicUsize,
    }

    impl TestDriver {
        fn new(id: &str) -> Self {
            Self {
                id: DeviceId::new(id),
                connect_info: Mutex::new(device_info(id, id, true)),
                connect_gate: None,
                capability_calls: AtomicUsize::new(0),
            }
        }

        fn with_connect_gate(mut self, gate: Arc<ConnectGate>) -> Self {
            self.connect_gate = Some(gate);
            self
        }

        fn set_connect_info(&self, info: DeviceInfo) {
            *self
                .connect_info
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = info;
        }
    }

    #[async_trait]
    impl DeviceDriver for TestDriver {
        fn id(&self) -> &DeviceId {
            &self.id
        }

        async fn connect(&self, _control: &ExecutionControl) -> DriverResult<DeviceInfo> {
            if let Some(gate) = &self.connect_gate {
                gate.entered.notify_one();
                gate.released.notified().await;
            }
            Ok(self
                .connect_info
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone())
        }

        async fn disconnect(&self, _control: &ExecutionControl) -> DriverResult<()> {
            Ok(())
        }

        async fn capabilities(
            &self,
            _control: &ExecutionControl,
        ) -> DriverResult<Vec<ActionDefinition>> {
            self.capability_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![ActionDefinition {
                name: format!("noop-{}", self.id),
                description: "test action".to_owned(),
                input_schema: json!({ "type": "object" }),
                protection: ActionProtection::Standard,
            }])
        }

        fn action_protection(&self, name: &str) -> Option<ActionProtection> {
            (name == format!("noop-{}", self.id)).then_some(ActionProtection::Standard)
        }

        async fn observe(
            &self,
            context: &DriverOperationContext,
        ) -> DeviceOperationResult<Observation> {
            let omission = (context.screenshot_policy() == ScreenshotPolicy::Omit)
                .then_some(ScreenshotOmissionReason::Policy);
            Ok(observation_with_omission(&self.id, omission))
        }

        async fn execute(
            &self,
            context: &DriverOperationContext,
            call: ActionCall,
        ) -> DeviceOperationResult<ActionResult> {
            let omission = (context.screenshot_policy() == ScreenshotPolicy::Omit)
                .then_some(ScreenshotOmissionReason::Policy);
            Ok(ActionResult {
                call_id: call.id,
                started_at_ms: now_ms(),
                finished_at_ms: now_ms(),
                output: Value::Null,
                before: None,
                after: Some(observation_with_omission(&self.id, omission)),
                evidence: Vec::new(),
                execution: None,
            })
        }
    }

    fn device_info(id: &str, name: &str, connected: bool) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new(id),
            name: name.to_owned(),
            platform: Platform::Mock,
            os_version: None,
            connected,
        }
    }

    fn observation_with_omission(
        device_id: &DeviceId,
        screenshot_omission: Option<ScreenshotOmissionReason>,
    ) -> Observation {
        Observation {
            id: Uuid::new_v4(),
            device_id: device_id.clone(),
            captured_at_ms: now_ms(),
            viewport: Viewport {
                width: 1,
                height: 1,
                scale_factor: 1.0,
            },
            screenshot: None,
            screenshot_omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata: Map::new(),
        }
    }

    async fn register(
        registry: &DriverRegistry<MemoryEventStore>,
        driver: &Arc<TestDriver>,
        info: DeviceInfo,
    ) -> Result<DriverHandle<MemoryEventStore>, RegistryError> {
        let erased: Arc<dyn DeviceDriver> = driver.clone();
        registry.register(erased, info).await
    }

    async fn wait_for_connect(gate: &ConnectGate) {
        tokio::time::timeout(Duration::from_secs(1), gate.entered.notified())
            .await
            .expect("connect entered Driver");
    }

    #[tokio::test]
    async fn registration_is_validated_sorted_and_routed_without_overwrite() {
        let events = Arc::new(MemoryEventStore::default());
        let registry = DriverRegistry::new(events);
        assert_eq!(
            registry.sole().await.expect_err("empty registry"),
            RegistryError::NoDevicesRegistered
        );

        let blank_id = Arc::new(TestDriver::new(" "));
        assert_eq!(
            register(&registry, &blank_id, device_info(" ", "blank", false))
                .await
                .expect_err("blank id"),
            RegistryError::InvalidDeviceId(DeviceId::new(" "))
        );

        let mismatch = Arc::new(TestDriver::new("driver-id"));
        assert_eq!(
            register(
                &registry,
                &mismatch,
                device_info("different-id", "mismatch", false),
            )
            .await
            .expect_err("mismatched identity"),
            RegistryError::DeviceIdMismatch {
                driver_id: DeviceId::new("driver-id"),
                info_id: DeviceId::new("different-id"),
            }
        );

        let unnamed = Arc::new(TestDriver::new("unnamed"));
        assert_eq!(
            register(&registry, &unnamed, device_info("unnamed", "  ", false))
                .await
                .expect_err("blank name"),
            RegistryError::InvalidDeviceName(DeviceId::new("unnamed"))
        );

        let driver_b = Arc::new(TestDriver::new("device-b"));
        let driver_a = Arc::new(TestDriver::new("device-a"));
        register(
            &registry,
            &driver_b,
            device_info("device-b", "original-b", false),
        )
        .await
        .expect("register b");
        register(
            &registry,
            &driver_a,
            device_info("device-a", "original-a", false),
        )
        .await
        .expect("register a");

        let replacement = Arc::new(TestDriver::new("device-b"));
        assert_eq!(
            register(
                &registry,
                &replacement,
                device_info("device-b", "replacement", true),
            )
            .await
            .expect_err("duplicate registration"),
            RegistryError::DeviceAlreadyRegistered(DeviceId::new("device-b"))
        );

        let listed = registry.list().await;
        assert_eq!(
            listed
                .iter()
                .map(|info| info.id.0.as_str())
                .collect::<Vec<_>>(),
            ["device-a", "device-b"]
        );
        assert_eq!(listed[1].name, "original-b");
        assert_eq!(
            registry
                .handles()
                .await
                .iter()
                .map(|handle| handle.id().0.as_str())
                .collect::<Vec<_>>(),
            ["device-a", "device-b"]
        );
        assert_eq!(
            registry.sole().await.expect_err("selection required"),
            RegistryError::DeviceSelectionRequired { count: 2 }
        );
        assert_eq!(
            registry
                .resolve(&DeviceId::new("missing"))
                .await
                .expect_err("unknown device"),
            RegistryError::DeviceNotFound(DeviceId::new("missing"))
        );

        let selected = registry
            .resolve(&DeviceId::new("device-b"))
            .await
            .expect("resolve b");
        let capabilities = selected
            .capabilities(&ExecutionControl::unbounded())
            .await
            .expect("route to b");
        assert_eq!(capabilities[0].name, "noop-device-b");
        assert_eq!(
            selected.action_protection("noop-device-b"),
            Some(ActionProtection::Standard)
        );
        assert_eq!(selected.action_protection("missing"), None);
        assert_eq!(
            registry
                .action_protection(&DeviceId::new("device-b"), "noop-device-b")
                .await
                .expect("registry protection delegation"),
            Some(ActionProtection::Standard)
        );
        assert_eq!(driver_b.capability_calls.load(Ordering::SeqCst), 1);
        assert_eq!(driver_a.capability_calls.load(Ordering::SeqCst), 0);
        assert_eq!(replacement.capability_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unregister_waits_for_operations_and_rejects_stale_handles_after_reregister() {
        let registry = Arc::new(DriverRegistry::new(Arc::new(MemoryEventStore::default())));
        let id = DeviceId::new("reused-device");
        let original = Arc::new(TestDriver::new(&id.0));
        let stale = register(&registry, &original, device_info(&id.0, "original", false))
            .await
            .expect("register original");
        let owner_id = LeaseOwnerId::new(Uuid::from_u128(7));
        let access = registry
            .access_available_to(stale.clone(), owner_id, 1)
            .await
            .expect("pin original operation");

        let mut unregister = tokio::spawn({
            let registry = Arc::clone(&registry);
            let id = id.clone();
            async move { registry.unregister(&id, 2).await }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut unregister)
                .await
                .is_err(),
            "unregister must wait for the admitted operation"
        );
        drop(access);
        unregister
            .await
            .expect("unregister task")
            .expect("unregister original");
        assert!(registry.list().await.is_empty());
        assert!(registry.pool_entries(3).await.is_empty());

        let replacement = Arc::new(TestDriver::new(&id.0));
        let current = register(
            &registry,
            &replacement,
            device_info(&id.0, "replacement", false),
        )
        .await
        .expect("register replacement");

        assert!(matches!(
            registry
                .access_available_to(stale.clone(), owner_id, 4)
                .await,
            Err(DevicePoolError::DeviceRegistrationChanged(device_id)) if device_id == id
        ));
        assert!(matches!(
            registry
                .record_health(&stale, PoolHealth::healthy(5), 5)
                .await,
            Err(DevicePoolError::DeviceRegistrationChanged(device_id)) if device_id == id
        ));
        assert_eq!(
            registry.pool_entries(6).await[0].health.state,
            PoolHealthState::Unknown,
            "a completed probe from the retired route must not update the replacement"
        );
        registry
            .access_available_to(current, owner_id, 7)
            .await
            .expect("replacement route is usable");
    }

    #[tokio::test]
    async fn unregistering_a_leased_device_changes_neither_inventory() {
        let registry = DriverRegistry::new(Arc::new(MemoryEventStore::default()));
        let id = DeviceId::new("leased-device");
        let driver = Arc::new(TestDriver::new(&id.0));
        let handle = register(&registry, &driver, device_info(&id.0, "leased", false))
            .await
            .expect("register");
        registry
            .record_health(&handle, PoolHealth::healthy(1), 1)
            .await
            .expect("health");
        let owner_id = LeaseOwnerId::new(Uuid::from_u128(9));
        let lease = registry
            .acquire_lease(&handle, owner_id, 2)
            .await
            .expect("lease");

        assert_eq!(
            registry.unregister(&id, 3).await,
            Err(RegistryError::DeviceLeased(id.clone()))
        );
        assert_eq!(registry.list().await.len(), 1);
        assert_eq!(registry.pool_entries(4).await.len(), 1);
        registry
            .release_lease(lease.id, owner_id, 5)
            .await
            .expect("release");
        registry.unregister(&id, 6).await.expect("unregister");
        assert!(registry.list().await.is_empty());
        assert!(registry.pool_entries(7).await.is_empty());
    }

    #[tokio::test]
    async fn connect_identity_validation_and_disconnect_update_the_snapshot() {
        let registry = DriverRegistry::new(Arc::new(MemoryEventStore::default()));
        let driver = Arc::new(TestDriver::new("snapshot-device"));
        let initial = device_info("snapshot-device", "initial", false);
        let handle = register(&registry, &driver, initial.clone())
            .await
            .expect("register");

        driver.set_connect_info(device_info("wrong-device", "wrong", true));
        let error = handle
            .connect(&ExecutionControl::unbounded())
            .await
            .expect_err("connect identity mismatch");
        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::Protocol(_))
        ));
        assert_eq!(handle.info().await, initial);

        let connected = device_info("snapshot-device", "connected name", true);
        driver.set_connect_info(connected.clone());
        assert_eq!(
            handle
                .connect(&ExecutionControl::unbounded())
                .await
                .expect("connect"),
            connected
        );
        assert_eq!(handle.info().await, connected);

        handle
            .disconnect(&ExecutionControl::unbounded())
            .await
            .expect("disconnect");
        let disconnected = handle.info().await;
        assert_eq!(disconnected.name, "connected name");
        assert!(!disconnected.connected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lifecycle_gates_one_device_without_blocking_another() {
        let registry = DriverRegistry::new(Arc::new(MemoryEventStore::default()));
        let gate = Arc::new(ConnectGate::default());
        let driver_a = Arc::new(TestDriver::new("device-a").with_connect_gate(gate.clone()));
        let driver_b = Arc::new(TestDriver::new("device-b"));
        let handle_a = register(&registry, &driver_a, device_info("device-a", "a", false))
            .await
            .expect("register a");
        let handle_b = register(&registry, &driver_b, device_info("device-b", "b", false))
            .await
            .expect("register b");

        let connect_task = tokio::spawn({
            let handle = handle_a.clone();
            async move {
                let control = ExecutionControl::unbounded();
                handle.connect(&control).await
            }
        });
        wait_for_connect(&gate).await;

        let mut same_device = tokio::spawn({
            let handle = handle_a.clone();
            async move {
                let control = ExecutionControl::unbounded();
                handle.capabilities(&control).await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut same_device)
                .await
                .is_err(),
            "same-device operation must wait behind connect"
        );
        assert_eq!(driver_a.capability_calls.load(Ordering::SeqCst), 0);

        tokio::time::timeout(Duration::from_secs(1), async {
            handle_b.capabilities(&ExecutionControl::unbounded()).await
        })
        .await
        .expect("other device is not gated")
        .expect("other device capabilities");
        assert_eq!(driver_b.capability_calls.load(Ordering::SeqCst), 1);

        gate.released.notify_one();
        connect_task.await.expect("connect task").expect("connect");
        same_device
            .await
            .expect("same-device task")
            .expect("capabilities after connect");
        assert_eq!(driver_a.capability_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn queued_operations_honor_request_cancellation_and_deadlines() {
        let registry = DriverRegistry::new(Arc::new(MemoryEventStore::default()));
        let gate = Arc::new(ConnectGate::default());
        let driver = Arc::new(TestDriver::new("queued-device").with_connect_gate(gate.clone()));
        let handle = register(
            &registry,
            &driver,
            device_info("queued-device", "queued", false),
        )
        .await
        .expect("register");

        let connect_task = tokio::spawn({
            let handle = handle.clone();
            async move {
                let control = ExecutionControl::unbounded();
                handle.connect(&control).await
            }
        });
        wait_for_connect(&gate).await;

        let (controller, cancelled_control) = ExecutionController::new();
        let mut cancelled_task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.capabilities(&cancelled_control).await }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut cancelled_task)
                .await
                .is_err(),
            "operation is queued before cancellation"
        );
        assert!(controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            cancelled_task
                .await
                .expect("cancelled task")
                .expect_err("cancelled while queued"),
            RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            }
        ));

        let (_, timeout_control) = ExecutionController::with_timeout(20, TimeoutScope::Request);
        assert!(matches!(
            handle
                .capabilities(&timeout_control)
                .await
                .expect_err("request expires while queued"),
            RuntimeError::TimedOut {
                scope: TimeoutScope::Request,
                timeout_ms: 20
            }
        ));
        assert_eq!(driver.capability_calls.load(Ordering::SeqCst), 0);

        gate.released.notify_one();
        connect_task.await.expect("connect task").expect("connect");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn action_timeout_starts_after_waiting_for_the_lifecycle_gate() {
        let events = Arc::new(MemoryEventStore::default());
        let registry = DriverRegistry::new(Arc::clone(&events));
        let gate = Arc::new(ConnectGate::default());
        let driver = Arc::new(TestDriver::new("action-device").with_connect_gate(gate.clone()));
        let handle = register(
            &registry,
            &driver,
            device_info("action-device", "action", false),
        )
        .await
        .expect("register");

        let start = StartSession::new(None, Some(handle.id().clone()), now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start session");

        let connect_task = tokio::spawn({
            let handle = handle.clone();
            async move {
                let control = ExecutionControl::unbounded();
                handle.connect(&control).await
            }
        });
        wait_for_connect(&gate).await;

        let mut execute_task = tokio::spawn({
            let handle = handle.clone();
            let context = OperationContext::new(session_id, None).with_action_timeout_ms(10);
            async move {
                handle
                    .execute(
                        &context,
                        ActionCall {
                            id: Uuid::new_v4(),
                            name: "noop-action-device".to_owned(),
                            arguments: json!({}),
                        },
                    )
                    .await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(35), &mut execute_task)
                .await
                .is_err(),
            "action timeout is not spent while waiting for lifecycle access"
        );

        gate.released.notify_one();
        connect_task.await.expect("connect task").expect("connect");
        execute_task
            .await
            .expect("execute task")
            .expect("action starts its timeout inside runtime");
    }

    #[tokio::test]
    async fn registry_propagates_the_global_screenshot_policy_to_registered_routes() {
        let events = Arc::new(MemoryEventStore::default());
        let registry =
            DriverRegistry::new(Arc::clone(&events)).with_screenshot_policy(ScreenshotPolicy::Omit);
        assert_eq!(registry.screenshot_policy(), ScreenshotPolicy::Omit);
        assert!(Arc::ptr_eq(&registry.event_store(), &events));

        let driver = Arc::new(TestDriver::new("omit-device"));
        let handle = register(&registry, &driver, device_info("omit-device", "omit", true))
            .await
            .expect("register omit route");
        let start = StartSession::new(None, Some(handle.id().clone()), now_ms());
        let context = OperationContext::new(start.session_id.clone(), None);
        events.start_session(start).await.expect("start session");

        let captured = handle.observe(&context).await.expect("omitted observation");
        assert!(captured.screenshot.is_none());
        assert_eq!(
            captured.screenshot_omission,
            Some(ScreenshotOmissionReason::Policy)
        );
    }

    #[test]
    fn registry_error_codes_are_stable() {
        let cases = [
            (
                RegistryError::InvalidDeviceId(DeviceId::new("")),
                "invalid_device_id",
                false,
            ),
            (
                RegistryError::InvalidDeviceName(DeviceId::new("device")),
                "invalid_device_name",
                false,
            ),
            (
                RegistryError::DeviceIdMismatch {
                    driver_id: DeviceId::new("driver"),
                    info_id: DeviceId::new("info"),
                },
                "device_id_mismatch",
                false,
            ),
            (
                RegistryError::DeviceAlreadyRegistered(DeviceId::new("device")),
                "device_already_registered",
                false,
            ),
            (
                RegistryError::DeviceNotFound(DeviceId::new("device")),
                "device_not_found",
                false,
            ),
            (
                RegistryError::DeviceLeased(DeviceId::new("device")),
                "device_leased",
                true,
            ),
            (
                RegistryError::InventoryInconsistent(DeviceId::new("device")),
                "registry_inventory_inconsistent",
                false,
            ),
            (
                RegistryError::NoDevicesRegistered,
                "no_devices_registered",
                true,
            ),
            (
                RegistryError::DeviceSelectionRequired { count: 2 },
                "device_selection_required",
                false,
            ),
        ];

        for (error, code, retryable) in cases {
            let info = error.to_error_info();
            assert_eq!(info.code, code);
            assert_eq!(info.retryable, retryable);
            assert!(!info.message.is_empty());
        }
    }
}
