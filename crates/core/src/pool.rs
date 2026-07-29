use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use devicerail_protocol::DeviceId;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedRwLockReadGuard, RwLock};
use uuid::Uuid;

pub type DevicePoolResult<T> = Result<T, DevicePoolError>;

const MAX_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;
const MAX_HEALTH_CODE_BYTES: usize = 64;

/// Runtime policy for exclusive device leases and health freshness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DevicePoolConfig {
    pub lease_ttl_ms: u64,
    pub health_validity_ms: u64,
}

impl DevicePoolConfig {
    pub fn new(lease_ttl_ms: u64, health_validity_ms: u64) -> DevicePoolResult<Self> {
        if lease_ttl_ms == 0
            || lease_ttl_ms > MAX_DURATION_MS
            || health_validity_ms == 0
            || health_validity_ms > MAX_DURATION_MS
        {
            return Err(DevicePoolError::InvalidConfiguration);
        }
        Ok(Self {
            lease_ttl_ms,
            health_validity_ms,
        })
    }

    fn lease_ttl(self) -> Duration {
        Duration::from_millis(self.lease_ttl_ms)
    }

    fn health_validity(self) -> Duration {
        Duration::from_millis(self.health_validity_ms)
    }
}

impl Default for DevicePoolConfig {
    fn default() -> Self {
        Self {
            lease_ttl_ms: 5 * 60 * 1_000,
            health_validity_ms: 30 * 1_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaseId(pub Uuid);

impl LeaseId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for LeaseId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaseOwnerId(pub Uuid);

impl LeaseOwnerId {
    pub const fn new(value: Uuid) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceLease {
    pub id: LeaseId,
    pub device_id: DeviceId,
    pub owner_id: LeaseOwnerId,
    /// Wall-clock audit timestamp only. Expiration never depends on it.
    pub issued_at_ms: u64,
    /// Wall-clock estimate for diagnostics only. The authoritative deadline is
    /// an internal monotonic [`Instant`].
    pub expires_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolHealthState {
    Unknown,
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolHealth {
    pub state: PoolHealthState,
    pub checked_at_ms: Option<u64>,
    pub code: Option<String>,
}

impl PoolHealth {
    pub const fn unknown() -> Self {
        Self {
            state: PoolHealthState::Unknown,
            checked_at_ms: None,
            code: None,
        }
    }

    pub const fn healthy(checked_at_ms: u64) -> Self {
        Self {
            state: PoolHealthState::Healthy,
            checked_at_ms: Some(checked_at_ms),
            code: None,
        }
    }

    pub fn degraded(checked_at_ms: u64, code: impl Into<String>) -> DevicePoolResult<Self> {
        Self::classified(PoolHealthState::Degraded, checked_at_ms, code)
    }

    pub fn unhealthy(checked_at_ms: u64, code: impl Into<String>) -> DevicePoolResult<Self> {
        Self::classified(PoolHealthState::Unhealthy, checked_at_ms, code)
    }

    fn classified(
        state: PoolHealthState,
        checked_at_ms: u64,
        code: impl Into<String>,
    ) -> DevicePoolResult<Self> {
        let code = code.into();
        if !valid_health_code(&code) {
            return Err(DevicePoolError::InvalidHealthCode);
        }
        Ok(Self {
            state,
            checked_at_ms: Some(checked_at_ms),
            code: Some(code),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevicePoolEntry {
    pub device_id: DeviceId,
    pub health: PoolHealth,
    pub lease: Option<DeviceLease>,
}

/// Opaque identity for one concrete registration of a DeviceId.
///
/// Health probes must retain this token from admission until publication. A
/// token from a removed registration can never update a later registration
/// that reuses the same DeviceId.
#[must_use = "the registration token is required to publish device health"]
#[derive(Clone)]
pub struct DeviceRegistrationToken {
    device_id: DeviceId,
    gate: Arc<RwLock<()>>,
}

impl DeviceRegistrationToken {
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }
}

impl fmt::Debug for DeviceRegistrationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceRegistrationToken")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// A Core-issued operation pin. While this value is alive, lease release,
/// expiration handoff, device removal, and acquisition by another owner all
/// wait on the same per-device gate.
pub struct DeviceAccessGuard {
    device_id: DeviceId,
    _guard: OwnedRwLockReadGuard<()>,
}

impl DeviceAccessGuard {
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }
}

impl fmt::Debug for DeviceAccessGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceAccessGuard")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DevicePoolError {
    #[error("invalid device pool configuration")]
    InvalidConfiguration,
    #[error("invalid health classification code")]
    InvalidHealthCode,
    #[error("device id must not be empty")]
    InvalidDeviceId,
    #[error("device is already in the pool: {0}")]
    DeviceAlreadyRegistered(DeviceId),
    #[error("device is not in the pool: {0}")]
    DeviceNotFound(DeviceId),
    #[error("device registration changed before the operation completed: {0}")]
    DeviceRegistrationChanged(DeviceId),
    #[error("device health is unknown: {0}")]
    HealthUnknown(DeviceId),
    #[error("device health is stale: {0}")]
    HealthStale(DeviceId),
    #[error("device is unhealthy: {device_id} ({code})")]
    DeviceUnhealthy { device_id: DeviceId, code: String },
    #[error("device is leased by another owner: {0}")]
    DeviceInUse(DeviceId),
    #[error("device lease was not found")]
    LeaseNotFound,
    #[error("device lease does not match the requested device or owner")]
    LeaseMismatch,
    #[error("device lease has expired")]
    LeaseExpired,
    #[error("cannot remove a leased device: {0}")]
    DeviceLeased(DeviceId),
}

struct LeaseRecord {
    value: DeviceLease,
    deadline: Instant,
}

struct PoolRecord {
    device_id: DeviceId,
    health: PoolHealth,
    health_checked_at: Option<Instant>,
    lease: Option<LeaseRecord>,
    gate: Arc<RwLock<()>>,
}

impl PoolRecord {
    fn public_entry(&self, now: Instant) -> DevicePoolEntry {
        DevicePoolEntry {
            device_id: self.device_id.clone(),
            health: self.health.clone(),
            lease: self
                .lease
                .as_ref()
                .filter(|lease| lease.deadline > now)
                .map(|lease| lease.value.clone()),
        }
    }
}

struct PoolState {
    wall_high_water_ms: u64,
    devices: BTreeMap<DeviceId, PoolRecord>,
    lease_devices: BTreeMap<LeaseId, DeviceId>,
    owner_leases: BTreeMap<LeaseOwnerId, BTreeSet<LeaseId>>,
    lease_deadlines: BTreeSet<(Instant, LeaseId)>,
}

impl PoolState {
    fn install_lease(&mut self, device_id: &DeviceId, lease: LeaseRecord) {
        let lease_id = lease.value.id;
        let owner_id = lease.value.owner_id;
        let deadline = lease.deadline;
        debug_assert!(!self.lease_devices.contains_key(&lease_id));
        debug_assert!(
            self.devices
                .get(device_id)
                .is_some_and(|record| record.lease.is_none())
        );
        self.lease_devices.insert(lease_id, device_id.clone());
        self.owner_leases
            .entry(owner_id)
            .or_default()
            .insert(lease_id);
        self.lease_deadlines.insert((deadline, lease_id));
        self.devices
            .get_mut(device_id)
            .expect("lease device remains registered")
            .lease = Some(lease);
    }

    fn take_lease(&mut self, device_id: &DeviceId) -> Option<LeaseRecord> {
        let lease = self.devices.get_mut(device_id)?.lease.take()?;
        self.lease_devices.remove(&lease.value.id);
        if let Entry::Occupied(mut owner) = self.owner_leases.entry(lease.value.owner_id) {
            owner.get_mut().remove(&lease.value.id);
            if owner.get().is_empty() {
                owner.remove();
            }
        }
        self.lease_deadlines
            .remove(&(lease.deadline, lease.value.id));
        Some(lease)
    }

    fn expire_lease(&mut self, device_id: &DeviceId, now: Instant) -> Option<DeviceLease> {
        let expired = self
            .devices
            .get(device_id)
            .and_then(|record| record.lease.as_ref())
            .is_some_and(|lease| lease.deadline <= now);
        expired
            .then(|| self.take_lease(device_id).map(|lease| lease.value))
            .flatten()
    }

    fn renew_lease(
        &mut self,
        device_id: &DeviceId,
        deadline: Instant,
        expires_at_ms: u64,
    ) -> Option<DeviceLease> {
        let (lease_id, previous_deadline) = self
            .devices
            .get(device_id)
            .and_then(|record| record.lease.as_ref())
            .map(|lease| (lease.value.id, lease.deadline))?;
        self.lease_deadlines.remove(&(previous_deadline, lease_id));
        let lease = self
            .devices
            .get_mut(device_id)
            .and_then(|record| record.lease.as_mut())
            .expect("indexed lease remains installed");
        lease.deadline = deadline;
        lease.value.expires_at_ms = expires_at_ms;
        let value = lease.value.clone();
        self.lease_deadlines.insert((deadline, lease_id));
        Some(value)
    }

    fn lease_targets(
        &self,
        lease_ids: impl IntoIterator<Item = LeaseId>,
    ) -> Vec<(DeviceId, Arc<RwLock<()>>)> {
        lease_ids
            .into_iter()
            .filter_map(|lease_id| self.lease_devices.get(&lease_id))
            .filter_map(|device_id| {
                self.devices
                    .get(device_id)
                    .map(|record| (device_id.clone(), Arc::clone(&record.gate)))
            })
            .collect()
    }

    #[cfg(test)]
    fn indexes_are_consistent(&self) -> bool {
        let mut lease_devices = BTreeMap::new();
        let mut owner_leases = BTreeMap::<LeaseOwnerId, BTreeSet<LeaseId>>::new();
        let mut lease_deadlines = BTreeSet::new();
        for (device_id, record) in &self.devices {
            if let Some(lease) = &record.lease {
                lease_devices.insert(lease.value.id, device_id.clone());
                owner_leases
                    .entry(lease.value.owner_id)
                    .or_default()
                    .insert(lease.value.id);
                lease_deadlines.insert((lease.deadline, lease.value.id));
            }
        }
        self.lease_devices == lease_devices
            && self.owner_leases == owner_leases
            && self.lease_deadlines == lease_deadlines
    }
}

/// Concurrency-safe inventory with exclusive, expiring, owner-bound leases.
///
/// Wall-clock values supplied by callers are retained only for diagnostics.
/// Lease expiry and health freshness use monotonic `Instant`s, so system clock
/// rollback cannot extend a lease or keep health fresh indefinitely.
pub struct DevicePool {
    config: DevicePoolConfig,
    state: Mutex<PoolState>,
}

impl DevicePool {
    pub fn new(config: DevicePoolConfig) -> DevicePoolResult<Self> {
        let config = DevicePoolConfig::new(config.lease_ttl_ms, config.health_validity_ms)?;
        Ok(Self {
            config,
            state: Mutex::new(PoolState {
                wall_high_water_ms: 0,
                devices: BTreeMap::new(),
                lease_devices: BTreeMap::new(),
                owner_leases: BTreeMap::new(),
                lease_deadlines: BTreeSet::new(),
            }),
        })
    }

    pub fn config(&self) -> DevicePoolConfig {
        self.config
    }

    pub async fn register(&self, device_id: DeviceId) -> DevicePoolResult<DeviceRegistrationToken> {
        if device_id.0.trim().is_empty() {
            return Err(DevicePoolError::InvalidDeviceId);
        }
        let mut state = self.state.lock().await;
        match state.devices.entry(device_id.clone()) {
            Entry::Vacant(entry) => {
                let gate = Arc::new(RwLock::new(()));
                let registration = DeviceRegistrationToken {
                    device_id: device_id.clone(),
                    gate: Arc::clone(&gate),
                };
                entry.insert(PoolRecord {
                    device_id,
                    health: PoolHealth::unknown(),
                    health_checked_at: None,
                    lease: None,
                    gate,
                });
                Ok(registration)
            }
            Entry::Occupied(_) => Err(DevicePoolError::DeviceAlreadyRegistered(device_id)),
        }
    }

    pub async fn remove(&self, device_id: &DeviceId, now_ms: u64) -> DevicePoolResult<()> {
        let gate = self.gate(device_id).await?;
        self.remove_with_gate(device_id, now_ms, gate).await
    }

    async fn remove_with_gate(
        &self,
        device_id: &DeviceId,
        now_ms: u64,
        gate: Arc<RwLock<()>>,
    ) -> DevicePoolResult<()> {
        let _exclusive = Arc::clone(&gate).write_owned().await;
        let now = Instant::now();
        let mut state = self.state.lock().await;
        ensure_current_gate(&state, device_id, &gate)?;
        advance_wall_clock(&mut state, now_ms);
        state.expire_lease(device_id, now);
        if state
            .devices
            .get(device_id)
            .ok_or_else(|| DevicePoolError::DeviceNotFound(device_id.clone()))?
            .lease
            .is_some()
        {
            return Err(DevicePoolError::DeviceLeased(device_id.clone()));
        }
        state.devices.remove(device_id);
        Ok(())
    }

    pub async fn record_health(
        &self,
        registration: &DeviceRegistrationToken,
        health: PoolHealth,
        now_ms: u64,
    ) -> DevicePoolResult<PoolHealth> {
        validate_health(&health)?;
        let monotonic_now = Instant::now();
        let mut state = self.state.lock().await;
        ensure_current_gate(&state, registration.device_id(), &registration.gate)?;
        let wall_now = advance_wall_clock(&mut state, now_ms);
        let mut health = health;
        let checked_at = if health.state == PoolHealthState::Unknown {
            health.checked_at_ms = None;
            health.code = None;
            None
        } else {
            health.checked_at_ms = Some(wall_now);
            Some(monotonic_now)
        };
        let record = state
            .devices
            .get_mut(registration.device_id())
            .ok_or_else(|| DevicePoolError::DeviceNotFound(registration.device_id().clone()))?;
        record.health = health.clone();
        record.health_checked_at = checked_at;
        Ok(health)
    }

    pub async fn acquire(
        &self,
        device_id: &DeviceId,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DeviceLease> {
        let gate = self.gate(device_id).await?;
        let _exclusive = Arc::clone(&gate).write_owned().await;
        let now = Instant::now();
        let deadline = now
            .checked_add(self.config.lease_ttl())
            .ok_or(DevicePoolError::InvalidConfiguration)?;
        let mut state = self.state.lock().await;
        ensure_current_gate(&state, device_id, &gate)?;
        let wall_now = advance_wall_clock(&mut state, now_ms);
        state.expire_lease(device_id, now);
        let record = state
            .devices
            .get(device_id)
            .ok_or_else(|| DevicePoolError::DeviceNotFound(device_id.clone()))?;
        ensure_healthy(record, now, self.config.health_validity())?;
        if let Some(lease) = &record.lease {
            if lease.value.owner_id != owner_id {
                return Err(DevicePoolError::DeviceInUse(device_id.clone()));
            }
            return Ok(state
                .renew_lease(
                    device_id,
                    deadline,
                    wall_now.saturating_add(self.config.lease_ttl_ms),
                )
                .expect("existing lease remains indexed"));
        }
        let lease_id = loop {
            let candidate = LeaseId::new();
            if !state.lease_devices.contains_key(&candidate) {
                break candidate;
            }
        };
        let value = DeviceLease {
            id: lease_id,
            device_id: device_id.clone(),
            owner_id,
            issued_at_ms: wall_now,
            expires_at_ms: wall_now.saturating_add(self.config.lease_ttl_ms),
        };
        state.install_lease(
            device_id,
            LeaseRecord {
                value: value.clone(),
                deadline,
            },
        );
        Ok(value)
    }

    /// Pins a lease-authorized operation to the device gate and renews the
    /// lease. The caller must retain the returned guard until Driver I/O has
    /// completed.
    pub async fn access_with_lease(
        &self,
        lease_id: LeaseId,
        device_id: &DeviceId,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DeviceAccessGuard> {
        self.access_with_lease_inner(lease_id, device_id, owner_id, now_ms, true)
            .await
    }

    /// Pins cleanup for the matching owner even when the last health probe is
    /// stale or unhealthy. Consumers must expose only cleanup operations while
    /// holding this guard.
    pub async fn access_with_lease_for_cleanup(
        &self,
        lease_id: LeaseId,
        device_id: &DeviceId,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DeviceAccessGuard> {
        self.access_with_lease_inner(lease_id, device_id, owner_id, now_ms, false)
            .await
    }

    async fn access_with_lease_inner(
        &self,
        lease_id: LeaseId,
        device_id: &DeviceId,
        owner_id: LeaseOwnerId,
        now_ms: u64,
        require_fresh_health: bool,
    ) -> DevicePoolResult<DeviceAccessGuard> {
        let gate = self.gate(device_id).await?;
        let guard = Arc::clone(&gate).read_owned().await;
        let now = Instant::now();
        let deadline = now
            .checked_add(self.config.lease_ttl())
            .ok_or(DevicePoolError::InvalidConfiguration)?;
        let mut state = self.state.lock().await;
        ensure_current_gate(&state, device_id, &gate)?;
        let wall_now = advance_wall_clock(&mut state, now_ms);
        let record = state
            .devices
            .get(device_id)
            .ok_or_else(|| DevicePoolError::DeviceNotFound(device_id.clone()))?;
        if require_fresh_health {
            ensure_healthy(record, now, self.config.health_validity())?;
        }
        let Some(lease) = &record.lease else {
            return Err(DevicePoolError::LeaseNotFound);
        };
        if lease.deadline <= now {
            state.take_lease(device_id);
            return Err(DevicePoolError::LeaseExpired);
        }
        if lease.value.id != lease_id
            || lease.value.owner_id != owner_id
            || lease.value.device_id != *device_id
        {
            return Err(DevicePoolError::LeaseMismatch);
        }
        state
            .renew_lease(
                device_id,
                deadline,
                wall_now.saturating_add(self.config.lease_ttl_ms),
            )
            .expect("validated lease remains indexed");
        drop(state);
        Ok(DeviceAccessGuard {
            device_id: device_id.clone(),
            _guard: guard,
        })
    }

    /// Pins an unleased lifecycle operation, or an operation by the current
    /// owner, so a lease handoff cannot race the Driver call.
    pub async fn access_available_to(
        &self,
        device_id: &DeviceId,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DeviceAccessGuard> {
        let gate = self.gate(device_id).await?;
        let guard = Arc::clone(&gate).read_owned().await;
        let now = Instant::now();
        let mut state = self.state.lock().await;
        ensure_current_gate(&state, device_id, &gate)?;
        advance_wall_clock(&mut state, now_ms);
        state.expire_lease(device_id, now);
        let record = state
            .devices
            .get(device_id)
            .ok_or_else(|| DevicePoolError::DeviceNotFound(device_id.clone()))?;
        if record
            .lease
            .as_ref()
            .is_some_and(|lease| lease.value.owner_id != owner_id)
        {
            return Err(DevicePoolError::DeviceInUse(device_id.clone()));
        }
        drop(state);
        Ok(DeviceAccessGuard {
            device_id: device_id.clone(),
            _guard: guard,
        })
    }

    pub async fn release(
        &self,
        lease_id: LeaseId,
        owner_id: LeaseOwnerId,
        now_ms: u64,
    ) -> DevicePoolResult<DeviceLease> {
        let device_id = self.lease_device(lease_id).await?;
        let gate = self.gate(&device_id).await?;
        let _exclusive = Arc::clone(&gate).write_owned().await;
        let now = Instant::now();
        let mut state = self.state.lock().await;
        ensure_current_gate(&state, &device_id, &gate)?;
        advance_wall_clock(&mut state, now_ms);
        let record = state
            .devices
            .get(&device_id)
            .ok_or_else(|| DevicePoolError::DeviceNotFound(device_id.clone()))?;
        let Some(lease) = &record.lease else {
            return Err(DevicePoolError::LeaseNotFound);
        };
        if lease.deadline <= now {
            state.take_lease(&device_id);
            return Err(DevicePoolError::LeaseExpired);
        }
        if lease.value.id != lease_id || lease.value.owner_id != owner_id {
            return Err(DevicePoolError::LeaseMismatch);
        }
        Ok(state
            .take_lease(&device_id)
            .expect("validated lease remains installed")
            .value)
    }

    /// Releases all leases for an owner after waiting for every active
    /// operation guard on those devices.
    pub async fn release_owner(&self, owner_id: LeaseOwnerId, now_ms: u64) -> Vec<DeviceLease> {
        let targets = {
            let state = self.state.lock().await;
            state.lease_targets(
                state
                    .owner_leases
                    .get(&owner_id)
                    .into_iter()
                    .flatten()
                    .copied(),
            )
        };
        let mut released = Vec::new();
        for (device_id, gate) in targets {
            let _exclusive = Arc::clone(&gate).write_owned().await;
            let now = Instant::now();
            let mut state = self.state.lock().await;
            if ensure_current_gate(&state, &device_id, &gate).is_err() {
                continue;
            }
            advance_wall_clock(&mut state, now_ms);
            if state
                .devices
                .get(&device_id)
                .and_then(|record| record.lease.as_ref())
                .is_some_and(|lease| lease.value.owner_id == owner_id)
                && let Some(lease) = state.take_lease(&device_id)
                && lease.deadline > now
            {
                released.push(lease.value);
            }
        }
        released
    }

    /// Global-shutdown escape hatch. Waits for operation guards and removes
    /// every remaining lease regardless of owner.
    pub async fn release_all(&self, now_ms: u64) -> Vec<DeviceLease> {
        let targets = {
            let state = self.state.lock().await;
            state.lease_targets(state.lease_devices.keys().copied())
        };
        let mut released = Vec::new();
        for (device_id, gate) in targets {
            let _exclusive = Arc::clone(&gate).write_owned().await;
            let mut state = self.state.lock().await;
            if ensure_current_gate(&state, &device_id, &gate).is_err() {
                continue;
            }
            advance_wall_clock(&mut state, now_ms);
            if let Some(lease) = state.take_lease(&device_id) {
                released.push(lease.value);
            }
        }
        released
    }

    pub async fn reap_expired(&self, now_ms: u64) -> Vec<DeviceLease> {
        let candidate_now = Instant::now();
        let targets = {
            let state = self.state.lock().await;
            state.lease_targets(
                state
                    .lease_deadlines
                    .iter()
                    .take_while(|(deadline, _lease_id)| *deadline <= candidate_now)
                    .map(|(_deadline, lease_id)| *lease_id),
            )
        };
        let mut expired = Vec::new();
        for (device_id, gate) in targets {
            let _exclusive = Arc::clone(&gate).write_owned().await;
            let now = Instant::now();
            let mut state = self.state.lock().await;
            if ensure_current_gate(&state, &device_id, &gate).is_err() {
                continue;
            }
            advance_wall_clock(&mut state, now_ms);
            if let Some(lease) = state.expire_lease(&device_id, now) {
                expired.push(lease);
            }
        }
        expired
    }

    pub async fn entries(&self, now_ms: u64) -> Vec<DevicePoolEntry> {
        let now = Instant::now();
        let mut state = self.state.lock().await;
        advance_wall_clock(&mut state, now_ms);
        state
            .devices
            .values()
            .map(|record| record.public_entry(now))
            .collect()
    }

    async fn gate(&self, device_id: &DeviceId) -> DevicePoolResult<Arc<RwLock<()>>> {
        self.state
            .lock()
            .await
            .devices
            .get(device_id)
            .map(|record| Arc::clone(&record.gate))
            .ok_or_else(|| DevicePoolError::DeviceNotFound(device_id.clone()))
    }

    async fn lease_device(&self, lease_id: LeaseId) -> DevicePoolResult<DeviceId> {
        self.state
            .lock()
            .await
            .lease_devices
            .get(&lease_id)
            .cloned()
            .ok_or(DevicePoolError::LeaseNotFound)
    }
}

fn ensure_current_gate(
    state: &PoolState,
    device_id: &DeviceId,
    gate: &Arc<RwLock<()>>,
) -> DevicePoolResult<()> {
    let record = state
        .devices
        .get(device_id)
        .ok_or_else(|| DevicePoolError::DeviceNotFound(device_id.clone()))?;
    if !Arc::ptr_eq(&record.gate, gate) {
        return Err(DevicePoolError::DeviceRegistrationChanged(
            device_id.clone(),
        ));
    }
    Ok(())
}

fn advance_wall_clock(state: &mut PoolState, supplied_now_ms: u64) -> u64 {
    state.wall_high_water_ms = state.wall_high_water_ms.max(supplied_now_ms);
    state.wall_high_water_ms
}

fn ensure_healthy(record: &PoolRecord, now: Instant, validity: Duration) -> DevicePoolResult<()> {
    let checked_at = record
        .health_checked_at
        .ok_or_else(|| DevicePoolError::HealthUnknown(record.device_id.clone()))?;
    if now.saturating_duration_since(checked_at) > validity {
        return Err(DevicePoolError::HealthStale(record.device_id.clone()));
    }
    match record.health.state {
        PoolHealthState::Healthy | PoolHealthState::Degraded => Ok(()),
        PoolHealthState::Unknown => Err(DevicePoolError::HealthUnknown(record.device_id.clone())),
        PoolHealthState::Unhealthy => Err(DevicePoolError::DeviceUnhealthy {
            device_id: record.device_id.clone(),
            code: record
                .health
                .code
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
        }),
    }
}

fn validate_health(health: &PoolHealth) -> DevicePoolResult<()> {
    match health.state {
        PoolHealthState::Unknown if health.checked_at_ms.is_none() && health.code.is_none() => {
            Ok(())
        }
        PoolHealthState::Healthy if health.checked_at_ms.is_some() && health.code.is_none() => {
            Ok(())
        }
        PoolHealthState::Degraded | PoolHealthState::Unhealthy
            if health.checked_at_ms.is_some()
                && health.code.as_deref().is_some_and(valid_health_code) =>
        {
            Ok(())
        }
        _ => Err(DevicePoolError::InvalidHealthCode),
    }
}

fn valid_health_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_HEALTH_CODE_BYTES
        && code.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use devicerail_protocol::DeviceId;
    use uuid::Uuid;

    use super::{
        DevicePool, DevicePoolConfig, DevicePoolError, DeviceRegistrationToken, LeaseOwnerId,
        PoolHealth, PoolHealthState,
    };

    fn owner(index: u128) -> LeaseOwnerId {
        LeaseOwnerId::new(Uuid::from_u128(index))
    }

    async fn healthy_pool(
        lease_ttl_ms: u64,
        health_validity_ms: u64,
    ) -> (DevicePool, DeviceId, DeviceRegistrationToken) {
        let pool = DevicePool::new(
            DevicePoolConfig::new(lease_ttl_ms, health_validity_ms).expect("config"),
        )
        .expect("pool");
        let id = DeviceId::new("device-1");
        let registration = pool.register(id.clone()).await.expect("register");
        pool.record_health(&registration, PoolHealth::healthy(10), 10)
            .await
            .expect("health");
        (pool, id, registration)
    }

    async fn assert_indexes_are_consistent(pool: &DevicePool) {
        let state = pool.state.lock().await;
        assert!(
            state.indexes_are_consistent(),
            "lease indexes must match authoritative device records"
        );
    }

    #[tokio::test]
    async fn exclusive_lease_is_idempotent_for_owner_and_rejects_others() {
        let (pool, id, _registration) = healthy_pool(100, 100).await;
        let first = pool.acquire(&id, owner(1), 20).await.expect("first");
        let again = pool.acquire(&id, owner(1), 21).await.expect("again");
        assert_eq!(again.id, first.id);
        assert!(again.expires_at_ms > first.expires_at_ms);
        assert_eq!(
            pool.acquire(&id, owner(2), 22).await,
            Err(DevicePoolError::DeviceInUse(id))
        );
    }

    #[tokio::test]
    async fn monotonic_expiration_survives_wall_clock_rollback() {
        let (pool, id, registration) = healthy_pool(15, 100).await;
        let first = pool.acquire(&id, owner(1), 100).await.expect("first");
        tokio::time::sleep(Duration::from_millis(25)).await;
        pool.record_health(&registration, PoolHealth::healthy(50), 50)
            .await
            .expect("fresh health after wall rollback");
        let second = pool.acquire(&id, owner(2), 50).await.expect("second");
        assert_ne!(first.id, second.id);
        assert!(second.issued_at_ms >= first.issued_at_ms);
    }

    #[tokio::test]
    async fn acquisition_requires_fresh_non_unhealthy_health() {
        let pool = DevicePool::new(DevicePoolConfig::new(100, 10).expect("config")).expect("pool");
        let id = DeviceId::new("device-1");
        let registration = pool.register(id.clone()).await.expect("register");
        assert_eq!(
            pool.acquire(&id, owner(1), 1).await,
            Err(DevicePoolError::HealthUnknown(id.clone()))
        );
        pool.record_health(
            &registration,
            PoolHealth::unhealthy(2, "probe_failed").expect("unhealthy"),
            2,
        )
        .await
        .expect("record");
        assert!(matches!(
            pool.acquire(&id, owner(1), 3).await,
            Err(DevicePoolError::DeviceUnhealthy { .. })
        ));
        pool.record_health(&registration, PoolHealth::healthy(4), 4)
            .await
            .expect("healthy");
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert_eq!(
            pool.acquire(&id, owner(1), 5).await,
            Err(DevicePoolError::HealthStale(id))
        );
    }

    #[tokio::test]
    async fn operation_guard_blocks_expired_handoff_until_driver_io_finishes() {
        let (pool, id, registration) = healthy_pool(15, 100).await;
        let lease = pool.acquire(&id, owner(1), 20).await.expect("lease");
        let guard = pool
            .access_with_lease(lease.id, &id, owner(1), 21)
            .await
            .expect("operation guard");
        tokio::time::sleep(Duration::from_millis(20)).await;
        pool.record_health(&registration, PoolHealth::healthy(22), 22)
            .await
            .expect("health");
        let blocked =
            tokio::time::timeout(Duration::from_millis(5), pool.acquire(&id, owner(2), 23)).await;
        assert!(
            blocked.is_err(),
            "handoff must wait for the operation guard"
        );
        drop(guard);
        pool.acquire(&id, owner(2), 24)
            .await
            .expect("handoff after operation");
    }

    #[tokio::test]
    async fn release_is_bound_to_both_opaque_id_and_owner() {
        let (pool, id, _registration) = healthy_pool(100, 100).await;
        let lease = pool.acquire(&id, owner(1), 20).await.expect("lease");
        assert_eq!(
            pool.release(lease.id, owner(2), 21).await,
            Err(DevicePoolError::LeaseMismatch)
        );
        assert_eq!(
            pool.release(lease.id, owner(1), 22).await.expect("release"),
            lease
        );
        assert_eq!(
            pool.release(lease.id, owner(1), 23).await,
            Err(DevicePoolError::LeaseNotFound)
        );
    }

    #[tokio::test]
    async fn lease_indexes_follow_renewal_and_every_release_path() {
        let (pool, first_id, _registration) = healthy_pool(1_000, 1_000).await;
        let first = pool
            .acquire(&first_id, owner(1), 20)
            .await
            .expect("first lease");
        assert_indexes_are_consistent(&pool).await;

        let renewed = pool
            .acquire(&first_id, owner(1), 21)
            .await
            .expect("idempotent renewal");
        assert_eq!(renewed.id, first.id);
        assert_indexes_are_consistent(&pool).await;

        let guard = pool
            .access_with_lease(first.id, &first_id, owner(1), 22)
            .await
            .expect("operation renewal");
        drop(guard);
        assert_indexes_are_consistent(&pool).await;

        assert_eq!(
            pool.release(first.id, owner(2), 23).await,
            Err(DevicePoolError::LeaseMismatch)
        );
        assert_indexes_are_consistent(&pool).await;
        pool.release(first.id, owner(1), 24)
            .await
            .expect("direct release");
        assert_indexes_are_consistent(&pool).await;

        let second_id = DeviceId::new("device-2");
        let second_registration = pool
            .register(second_id.clone())
            .await
            .expect("register second");
        pool.record_health(&second_registration, PoolHealth::healthy(25), 25)
            .await
            .expect("second health");
        pool.acquire(&first_id, owner(1), 26)
            .await
            .expect("owner lease");
        pool.acquire(&second_id, owner(2), 27)
            .await
            .expect("other owner lease");
        assert_indexes_are_consistent(&pool).await;

        assert_eq!(pool.release_owner(owner(1), 28).await.len(), 1);
        assert_indexes_are_consistent(&pool).await;
        assert_eq!(pool.release_all(29).await.len(), 1);
        assert_indexes_are_consistent(&pool).await;
    }

    #[tokio::test]
    async fn reap_expired_waits_only_for_expired_candidates() {
        let pool =
            DevicePool::new(DevicePoolConfig::new(300, 1_000).expect("config")).expect("pool");
        let expired_id = DeviceId::new("a-expired");
        let expired_registration = pool
            .register(expired_id.clone())
            .await
            .expect("register expired candidate");
        pool.record_health(&expired_registration, PoolHealth::healthy(1), 1)
            .await
            .expect("expired candidate health");
        let expired_lease = pool
            .acquire(&expired_id, owner(1), 2)
            .await
            .expect("expired candidate lease");

        tokio::time::sleep(Duration::from_millis(200)).await;
        let live_id = DeviceId::new("z-live");
        let live_registration = pool
            .register(live_id.clone())
            .await
            .expect("register live device");
        pool.record_health(&live_registration, PoolHealth::healthy(3), 3)
            .await
            .expect("live health");
        let live_lease = pool
            .acquire(&live_id, owner(2), 4)
            .await
            .expect("live lease");
        let live_guard = pool
            .access_with_lease(live_lease.id, &live_id, owner(2), 5)
            .await
            .expect("pin live operation");

        tokio::time::sleep(Duration::from_millis(120)).await;
        let expired = tokio::time::timeout(Duration::from_millis(60), pool.reap_expired(6))
            .await
            .expect("an unexpired device gate must not delay expiration reaping");
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, expired_lease.id);
        assert_indexes_are_consistent(&pool).await;

        drop(live_guard);
        pool.release(live_lease.id, owner(2), 7)
            .await
            .expect("release live lease");
        assert_indexes_are_consistent(&pool).await;
    }

    #[tokio::test]
    async fn owner_release_does_not_block_unrelated_lease_operations() {
        let pool =
            DevicePool::new(DevicePoolConfig::new(1_000, 1_000).expect("config")).expect("pool");
        let mut leases = Vec::new();
        for (index, (name, lease_owner)) in [
            ("owner-a", owner(1)),
            ("owner-b", owner(1)),
            ("unrelated", owner(2)),
        ]
        .into_iter()
        .enumerate()
        {
            let id = DeviceId::new(name);
            let registration = pool.register(id.clone()).await.expect("register");
            pool.record_health(
                &registration,
                PoolHealth::healthy(index as u64 + 1),
                index as u64 + 1,
            )
            .await
            .expect("health");
            leases.push(
                pool.acquire(&id, lease_owner, index as u64 + 10)
                    .await
                    .expect("lease"),
            );
        }
        let guard = pool
            .access_with_lease(leases[0].id, &leases[0].device_id, owner(1), 20)
            .await
            .expect("pin owner operation");

        let mut owner_release = Box::pin(pool.release_owner(owner(1), 21));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut owner_release)
                .await
                .is_err(),
            "owner cleanup must wait for its active operation"
        );
        tokio::time::timeout(
            Duration::from_millis(60),
            pool.release(leases[2].id, owner(2), 22),
        )
        .await
        .expect("owner cleanup must not hold global state while waiting")
        .expect("release unrelated lease");

        drop(guard);
        assert_eq!(owner_release.await.len(), 2);
        assert_indexes_are_consistent(&pool).await;
    }

    #[tokio::test]
    async fn leased_devices_cannot_be_removed_and_owner_cleanup_is_bounded() {
        let (pool, id, _registration) = healthy_pool(100, 100).await;
        pool.acquire(&id, owner(1), 20).await.expect("lease");
        assert_eq!(
            pool.remove(&id, 21).await,
            Err(DevicePoolError::DeviceLeased(id.clone()))
        );
        assert_eq!(pool.release_owner(owner(1), 22).await.len(), 1);
        pool.remove(&id, 23).await.expect("remove");
        assert!(pool.entries(24).await.is_empty());
    }

    #[tokio::test]
    async fn stale_remove_gate_cannot_delete_a_reregistered_device() {
        let pool = DevicePool::new(DevicePoolConfig::default()).expect("pool");
        let id = DeviceId::new("device-1");
        let _initial_registration = pool.register(id.clone()).await.expect("initial register");
        let stale_gate = pool.gate(&id).await.expect("initial gate");

        pool.remove(&id, 1).await.expect("remove initial record");
        let current_registration = pool.register(id.clone()).await.expect("reregister");
        pool.record_health(&current_registration, PoolHealth::healthy(2), 2)
            .await
            .expect("new record health");

        assert_eq!(
            pool.remove_with_gate(&id, 3, stale_gate).await,
            Err(DevicePoolError::DeviceRegistrationChanged(id.clone()))
        );
        let entries = pool.entries(4).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].device_id, id);
        assert_eq!(entries[0].health.state, PoolHealthState::Healthy);
    }

    #[tokio::test]
    async fn late_health_probe_cannot_update_a_reregistered_device() {
        let pool = DevicePool::new(DevicePoolConfig::default()).expect("pool");
        let id = DeviceId::new("device-1");
        let stale_registration = pool.register(id.clone()).await.expect("initial register");
        pool.remove(&id, 1).await.expect("remove initial record");
        let current_registration = pool.register(id.clone()).await.expect("reregister");

        assert_eq!(
            pool.record_health(&stale_registration, PoolHealth::healthy(2), 2)
                .await,
            Err(DevicePoolError::DeviceRegistrationChanged(id.clone()))
        );
        assert_eq!(
            pool.entries(3).await[0].health.state,
            PoolHealthState::Unknown,
            "the late probe must not mutate the replacement"
        );
        pool.record_health(&current_registration, PoolHealth::healthy(4), 4)
            .await
            .expect("current probe");
        assert_eq!(
            pool.entries(5).await[0].health.state,
            PoolHealthState::Healthy
        );
    }

    #[tokio::test]
    async fn entries_are_stably_ordered_and_health_codes_are_closed() {
        let pool = DevicePool::new(DevicePoolConfig::default()).expect("pool");
        for id in ["z", "a"] {
            let _registration = pool.register(DeviceId::new(id)).await.expect("register");
        }
        assert_eq!(
            pool.entries(1)
                .await
                .into_iter()
                .map(|entry| entry.device_id.0)
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert!(PoolHealth::unhealthy(1, "INVALID CODE").is_err());
        assert_eq!(PoolHealth::unknown().state, PoolHealthState::Unknown);
    }
}
