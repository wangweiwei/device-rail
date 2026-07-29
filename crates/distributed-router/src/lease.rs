use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    PeerLease,
    model::{MAX_DEVICE_KEY_BYTES, MAX_SAFE_INTEGER, valid_identifier, valid_lease_ttl},
};

const MAX_LEDGER_CALLS: usize = 1_024;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum LeaseError {
    #[error("lease request is invalid")]
    Invalid,
    #[error("device lease is held by another owner")]
    Held,
    #[error("lease is stale, expired, replayed, or belongs to another node epoch")]
    Stale,
    #[error("lease device is not in this node inventory")]
    DeviceNotFound,
}

impl LeaseError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "lease_invalid",
            Self::Held => "lease_held",
            Self::Stale => "lease_stale",
            Self::DeviceNotFound => "lease_device_not_found",
        }
    }
}

/// Node-side lease authority for one epoch.
///
/// This provides at-most-one active owner inside one node process. It is not a
/// cross-node consensus protocol and does not claim atomicity across network
/// partitions. A transport failure during a mutation remains outcome-unknown.
pub struct LeaseTable {
    node_epoch: u64,
    known_devices: std::collections::BTreeSet<String>,
    active: Mutex<BTreeMap<String, ActiveLease>>,
}

#[derive(Clone)]
struct ActiveLease {
    token: PeerLease,
    deadline: Instant,
}

impl ActiveLease {
    fn is_active(&self, now_ms: u64) -> bool {
        self.token.expires_at_ms > now_ms && Instant::now() < self.deadline
    }
}

impl std::fmt::Debug for LeaseTable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LeaseTable")
            .field("node_epoch", &self.node_epoch)
            .field("known_device_count", &self.known_devices.len())
            .field(
                "active_lease_count",
                &self
                    .active
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len(),
            )
            .finish()
    }
}

impl LeaseTable {
    pub fn new(
        node_epoch: u64,
        known_devices: impl IntoIterator<Item = String>,
    ) -> Result<Self, LeaseError> {
        let known_devices = known_devices
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if node_epoch == 0
            || node_epoch > MAX_SAFE_INTEGER
            || known_devices.is_empty()
            || known_devices.len() > 256
            || known_devices
                .iter()
                .any(|key| !valid_identifier(key, MAX_DEVICE_KEY_BYTES))
        {
            return Err(LeaseError::Invalid);
        }
        Ok(Self {
            node_epoch,
            known_devices,
            active: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn acquire(
        &self,
        device_key: &str,
        owner_id: &str,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<PeerLease, LeaseError> {
        self.validate_input(device_key, owner_id, ttl_ms, now_ms)?;
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = active.get(device_key)
            && existing.is_active(now_ms)
        {
            return if existing.token.owner_id == owner_id {
                Ok(existing.token.clone())
            } else {
                Err(LeaseError::Held)
            };
        }
        let lease = PeerLease {
            lease_id: Uuid::new_v4(),
            device_key: device_key.to_owned(),
            owner_id: owner_id.to_owned(),
            node_epoch: self.node_epoch,
            expires_at_ms: now_ms
                .checked_add(ttl_ms)
                .filter(|value| *value <= MAX_SAFE_INTEGER)
                .ok_or(LeaseError::Invalid)?,
        };
        active.insert(
            device_key.to_owned(),
            ActiveLease {
                token: lease.clone(),
                deadline: Instant::now()
                    .checked_add(Duration::from_millis(ttl_ms))
                    .ok_or(LeaseError::Invalid)?,
            },
        );
        Ok(lease)
    }

    pub fn authorize(&self, lease: &PeerLease, now_ms: u64) -> Result<(), LeaseError> {
        self.validate_lease(lease, now_ms)?;
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match active.get(&lease.device_key) {
            Some(current) if current.token == *lease && current.is_active(now_ms) => Ok(()),
            _ => Err(LeaseError::Stale),
        }
    }

    pub fn renew(
        &self,
        lease: &PeerLease,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<PeerLease, LeaseError> {
        if !valid_lease_ttl(ttl_ms) {
            return Err(LeaseError::Invalid);
        }
        self.validate_lease(lease, now_ms)?;
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match active.get(&lease.device_key) {
            Some(current) if current.token == *lease && current.is_active(now_ms) => {}
            _ => return Err(LeaseError::Stale),
        }
        let mut renewed = lease.clone();
        renewed.expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .filter(|value| *value <= MAX_SAFE_INTEGER)
            .ok_or(LeaseError::Invalid)?;
        active.insert(
            lease.device_key.clone(),
            ActiveLease {
                token: renewed.clone(),
                deadline: Instant::now()
                    .checked_add(Duration::from_millis(ttl_ms))
                    .ok_or(LeaseError::Invalid)?,
            },
        );
        Ok(renewed)
    }

    pub fn release(&self, lease: &PeerLease, now_ms: u64) -> Result<(), LeaseError> {
        self.validate_lease(lease, now_ms)?;
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match active.get(&lease.device_key) {
            Some(current) if current.token == *lease && current.is_active(now_ms) => {}
            _ => return Err(LeaseError::Stale),
        }
        active.remove(&lease.device_key);
        Ok(())
    }

    /// Revokes an exact token during authenticated connection cleanup. Unlike
    /// normal release this also accepts an already-expired token, but never a
    /// mismatched/replayed token.
    pub fn revoke(&self, lease: &PeerLease) -> Result<(), LeaseError> {
        lease.validate().map_err(|_| LeaseError::Invalid)?;
        if lease.node_epoch != self.node_epoch {
            return Err(LeaseError::Stale);
        }
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match active.get(&lease.device_key) {
            Some(current) if current.token == *lease => {
                active.remove(&lease.device_key);
                Ok(())
            }
            _ => Err(LeaseError::Stale),
        }
    }

    pub fn invalidate_all(&self) {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    fn validate_input(
        &self,
        device_key: &str,
        owner_id: &str,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<(), LeaseError> {
        if !valid_identifier(device_key, MAX_DEVICE_KEY_BYTES)
            || !valid_identifier(owner_id, 64)
            || !valid_lease_ttl(ttl_ms)
            || now_ms == 0
            || now_ms > MAX_SAFE_INTEGER
        {
            return Err(LeaseError::Invalid);
        }
        if !self.known_devices.contains(device_key) {
            return Err(LeaseError::DeviceNotFound);
        }
        Ok(())
    }

    fn validate_lease(&self, lease: &PeerLease, now_ms: u64) -> Result<(), LeaseError> {
        lease.validate().map_err(|_| LeaseError::Invalid)?;
        if lease.node_epoch != self.node_epoch || now_ms == 0 || now_ms > MAX_SAFE_INTEGER {
            return Err(LeaseError::Stale);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallLedgerDecision {
    FirstSeen,
    Duplicate,
    Conflict,
}

#[derive(Default)]
struct LedgerState {
    order: VecDeque<Uuid>,
    fingerprints: BTreeMap<Uuid, [u8; 32]>,
}

/// Bounded call-id deduplication window for node-side execute handlers.
/// Only SHA-256 request fingerprints are retained; arguments and secrets are
/// never kept by the ledger or exposed through Debug.
#[derive(Default)]
pub struct CallLedger {
    state: Mutex<LedgerState>,
}

impl std::fmt::Debug for CallLedger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CallLedger")
            .field(
                "entry_count",
                &self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .fingerprints
                    .len(),
            )
            .finish()
    }
}

impl CallLedger {
    pub fn register(&self, call_id: Uuid, canonical_request: &[u8]) -> CallLedgerDecision {
        if call_id.is_nil() {
            return CallLedgerDecision::Conflict;
        }
        let fingerprint: [u8; 32] = Sha256::digest(canonical_request).into();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = state.fingerprints.get(&call_id) {
            return if existing == &fingerprint {
                CallLedgerDecision::Duplicate
            } else {
                CallLedgerDecision::Conflict
            };
        }
        if state.order.len() == MAX_LEDGER_CALLS
            && let Some(evicted) = state.order.pop_front()
        {
            state.fingerprints.remove(&evicted);
        }
        state.order.push_back(call_id);
        state.fingerprints.insert(call_id, fingerprint);
        CallLedgerDecision::FirstSeen
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{CallLedger, CallLedgerDecision, LeaseError, LeaseTable};

    #[test]
    fn lease_prevents_steal_and_rejects_stale_or_replayed_token() {
        let table = LeaseTable::new(9, ["phone-1".to_owned()]).expect("table");
        let first = table
            .acquire("phone-1", "owner-a", 1_000, 10)
            .expect("first lease");
        assert_eq!(
            table.acquire("phone-1", "owner-b", 1_000, 11),
            Err(LeaseError::Held)
        );
        table.authorize(&first, 12).expect("authorize");
        table.release(&first, 12).expect("release");
        assert_eq!(table.authorize(&first, 13), Err(LeaseError::Stale));

        let replacement = table
            .acquire("phone-1", "owner-b", 1_000, 13)
            .expect("replacement");
        assert_ne!(replacement.lease_id, first.lease_id);
        assert_eq!(table.authorize(&first, 14), Err(LeaseError::Stale));
    }

    #[test]
    fn epoch_and_expiry_are_enforced() {
        let table = LeaseTable::new(9, ["phone-1".to_owned()]).expect("table");
        let lease = table.acquire("phone-1", "owner", 1_000, 10).expect("lease");
        assert_eq!(table.authorize(&lease, 1_010), Err(LeaseError::Stale));
        let other_epoch = LeaseTable::new(10, ["phone-1".to_owned()]).expect("other");
        assert_eq!(other_epoch.authorize(&lease, 11), Err(LeaseError::Stale));
    }

    #[test]
    fn monotonic_deadline_expires_even_if_wall_clock_moves_backwards() {
        let table = LeaseTable::new(9, ["phone-1".to_owned()]).expect("table");
        let lease = table.acquire("phone-1", "owner", 1_000, 10).expect("lease");
        table
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut("phone-1")
            .expect("active lease")
            .deadline = Instant::now() - Duration::from_millis(1);

        assert_eq!(table.authorize(&lease, 11), Err(LeaseError::Stale));
    }

    #[test]
    fn call_id_contract_detects_duplicate_and_conflict_without_logging_payload() {
        let ledger = CallLedger::default();
        let id = uuid::Uuid::new_v4();
        assert_eq!(
            ledger.register(id, br#"{"name":"tap","secret":"ONE"}"#),
            CallLedgerDecision::FirstSeen
        );
        assert_eq!(
            ledger.register(id, br#"{"name":"tap","secret":"ONE"}"#),
            CallLedgerDecision::Duplicate
        );
        assert_eq!(
            ledger.register(id, br#"{"name":"tap","secret":"TWO"}"#),
            CallLedgerDecision::Conflict
        );
        let rendered = format!("{ledger:?}");
        assert!(!rendered.contains("ONE"));
        assert!(!rendered.contains("TWO"));
    }
}
