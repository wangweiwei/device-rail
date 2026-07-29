use std::{collections::BTreeSet, fs::OpenOptions, io::Read as _, net::SocketAddr, path::Path};

use serde::Deserialize;
use thiserror::Error;

use crate::{
    NodeId, RemoteDriverConfig,
    model::{MAX_SAFE_INTEGER, valid_identifier},
};

pub const EXTERNAL_TUNNEL_SECURITY_MODE: &str = "externalSshOrMtls";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_CONFIGURED_PEERS: usize = 32;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("distributed peer config must be an owner-only regular file")]
    UnsafeFile,
    #[error("distributed peer owner-only permissions cannot be verified")]
    PermissionsUnsupported,
    #[error("distributed peer config exceeds its size limit")]
    TooLarge,
    #[error("distributed peer config could not be read")]
    Read,
    #[error("distributed peer config is invalid or incomplete")]
    Invalid,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConfiguredPeer {
    node_id: NodeId,
    endpoint: SocketAddr,
    tunnel_id: String,
    owner_id: String,
    driver: RemoteDriverConfig,
}

impl std::fmt::Debug for ConfiguredPeer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfiguredPeer")
            .field("node_id", &self.node_id)
            .field("endpoint", &self.endpoint)
            .field("security_mode", &EXTERNAL_TUNNEL_SECURITY_MODE)
            .field("tunnel_id", &"[REDACTED]")
            .field("owner_id", &"[REDACTED]")
            .field("driver", &self.driver)
            .finish()
    }
}

impl ConfiguredPeer {
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    pub fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }

    pub fn tunnel_id(&self) -> &str {
        &self.tunnel_id
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn driver_config(&self) -> RemoteDriverConfig {
        self.driver
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConfiguredPeers {
    peers: Vec<ConfiguredPeer>,
}

impl std::fmt::Debug for ConfiguredPeers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfiguredPeers")
            .field("peer_count", &self.peers.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConfiguredPeerServer {
    node_id: NodeId,
    listen: SocketAddr,
    tunnel_id: String,
    node_epoch: u64,
    inventory_revision: u64,
}

impl std::fmt::Debug for ConfiguredPeerServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfiguredPeerServer")
            .field("node_id", &self.node_id)
            .field("listen", &self.listen)
            .field("security_mode", &EXTERNAL_TUNNEL_SECURITY_MODE)
            .field("tunnel_id", &"[REDACTED]")
            .field("node_epoch", &self.node_epoch)
            .field("inventory_revision", &self.inventory_revision)
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfigFile {
    schema_version: u8,
    peers: Vec<PeerFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PeerServerFile {
    schema_version: u8,
    node_id: String,
    listen: String,
    security_mode: String,
    tunnel_id: String,
    node_epoch: u64,
    inventory_revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PeerFile {
    node_id: String,
    endpoint: String,
    security_mode: String,
    tunnel_id: String,
    owner_id: String,
    lease_ttl_ms: u64,
    renew_before_ms: u64,
}

impl ConfiguredPeers {
    /// Loads fail-closed daemon peer declarations. Endpoints must be loopback:
    /// an operator connects them to another host through a separately managed
    /// SSH/mTLS tunnel. This crate does not open a public listener or claim
    /// built-in transport encryption.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let bytes = read_owner_only(path.as_ref())?;
        let file: ConfigFile = serde_json::from_slice(&bytes).map_err(|_| ConfigError::Invalid)?;
        if file.schema_version != 1
            || file.peers.is_empty()
            || file.peers.len() > MAX_CONFIGURED_PEERS
        {
            return Err(ConfigError::Invalid);
        }
        let mut node_ids = BTreeSet::new();
        let mut endpoints = BTreeSet::new();
        let mut peers = Vec::with_capacity(file.peers.len());
        for peer in file.peers {
            let node_id = NodeId::parse(peer.node_id).map_err(|_| ConfigError::Invalid)?;
            let endpoint = peer
                .endpoint
                .parse::<SocketAddr>()
                .map_err(|_| ConfigError::Invalid)?;
            if !endpoint.ip().is_loopback()
                || endpoint.port() == 0
                || peer.security_mode != EXTERNAL_TUNNEL_SECURITY_MODE
                || !valid_identifier(&peer.tunnel_id, 64)
                || !valid_identifier(&peer.owner_id, 64)
                || peer.owner_id != peer.tunnel_id
                || !node_ids.insert(node_id.clone())
                || !endpoints.insert(endpoint)
            {
                return Err(ConfigError::Invalid);
            }
            let driver = RemoteDriverConfig::new(peer.lease_ttl_ms, peer.renew_before_ms)
                .map_err(|_| ConfigError::Invalid)?;
            peers.push(ConfiguredPeer {
                node_id,
                endpoint,
                tunnel_id: peer.tunnel_id,
                owner_id: peer.owner_id,
                driver,
            });
        }
        peers.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        Ok(Self { peers })
    }

    pub fn peers(&self) -> &[ConfiguredPeer] {
        &self.peers
    }
}

impl ConfiguredPeerServer {
    /// Loads one fail-closed node-side peer listener declaration. The listener
    /// is always numeric loopback and represents one separately authenticated
    /// SSH/mTLS tunnel subject; this configuration does not provide TLS itself.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let bytes = read_owner_only(path.as_ref())?;
        let file: PeerServerFile =
            serde_json::from_slice(&bytes).map_err(|_| ConfigError::Invalid)?;
        let node_id = NodeId::parse(file.node_id).map_err(|_| ConfigError::Invalid)?;
        let listen = file
            .listen
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::Invalid)?;
        if file.schema_version != 1
            || !listen.ip().is_loopback()
            || listen.port() == 0
            || file.security_mode != EXTERNAL_TUNNEL_SECURITY_MODE
            || !valid_identifier(&file.tunnel_id, 64)
            || file.node_epoch == 0
            || file.node_epoch > MAX_SAFE_INTEGER
            || file.inventory_revision == 0
            || file.inventory_revision > MAX_SAFE_INTEGER
        {
            return Err(ConfigError::Invalid);
        }
        Ok(Self {
            node_id,
            listen,
            tunnel_id: file.tunnel_id,
            node_epoch: file.node_epoch,
            inventory_revision: file.inventory_revision,
        })
    }

    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    pub fn listen(&self) -> SocketAddr {
        self.listen
    }

    pub fn tunnel_id(&self) -> &str {
        &self.tunnel_id
    }

    pub fn node_epoch(&self) -> u64 {
        self.node_epoch
    }

    pub fn inventory_revision(&self) -> u64 {
        self.inventory_revision
    }
}

fn read_owner_only(path: &Path) -> Result<Vec<u8>, ConfigError> {
    let before = path
        .symlink_metadata()
        .map_err(|_| ConfigError::UnsafeFile)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(ConfigError::UnsafeFile);
    }
    require_owner_only(&before)?;
    require_acl_path(path)?;
    if before.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let mut file = options.open(path).map_err(|_| ConfigError::UnsafeFile)?;
    require_same_file(&before, &file)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.by_ref()
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigError::Read)?;
    require_same_file(&before, &file)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn require_owner_only(metadata: &std::fs::Metadata) -> Result<(), ConfigError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(ConfigError::UnsafeFile);
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_owner_only(_: &std::fs::Metadata) -> Result<(), ConfigError> {
    Err(ConfigError::PermissionsUnsupported)
}

#[cfg(unix)]
fn require_same_file(before: &std::fs::Metadata, file: &std::fs::File) -> Result<(), ConfigError> {
    use std::os::unix::fs::MetadataExt as _;

    let after = file.metadata().map_err(|_| ConfigError::UnsafeFile)?;
    require_owner_only(&after)?;
    require_acl_file(file)?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
    {
        return Err(ConfigError::UnsafeFile);
    }
    Ok(())
}

#[cfg(unix)]
fn require_acl_path(path: &Path) -> Result<(), ConfigError> {
    extended_acl::require_path(path).map_err(|error| match error {
        #[cfg(target_os = "macos")]
        extended_acl::Error::Present => ConfigError::UnsafeFile,
        #[cfg(target_os = "macos")]
        extended_acl::Error::Unavailable => ConfigError::PermissionsUnsupported,
    })
}

#[cfg(not(unix))]
fn require_acl_path(_path: &Path) -> Result<(), ConfigError> {
    Err(ConfigError::PermissionsUnsupported)
}

#[cfg(unix)]
fn require_acl_file(file: &std::fs::File) -> Result<(), ConfigError> {
    extended_acl::require_file(file).map_err(|error| match error {
        #[cfg(target_os = "macos")]
        extended_acl::Error::Present => ConfigError::UnsafeFile,
        #[cfg(target_os = "macos")]
        extended_acl::Error::Unavailable => ConfigError::PermissionsUnsupported,
    })
}

#[cfg(unix)]
mod extended_acl {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Error {
        #[cfg(target_os = "macos")]
        Present,
        #[cfg(target_os = "macos")]
        Unavailable,
    }

    #[cfg(target_os = "macos")]
    mod darwin {
        use std::{
            ffi::{c_int, c_void},
            fs::File,
            os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
            path::Path,
        };

        use super::Error;

        const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
        const ACL_FIRST_ENTRY: c_int = 0;

        unsafe extern "C" {
            fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> *mut c_void;
            fn acl_get_file(path: *const i8, acl_type: c_int) -> *mut c_void;
            fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
            fn acl_free(object: *mut c_void) -> c_int;
        }

        struct Acl(*mut c_void);

        impl Drop for Acl {
            fn drop(&mut self) {
                // SAFETY: this guard uniquely owns the non-null ACL pointer.
                let _ = unsafe { acl_free(self.0) };
            }
        }

        fn require_empty(acl: *mut c_void) -> Result<(), Error> {
            if acl.is_null() {
                return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                    // Darwin reports ENOENT when the object has no extended ACL.
                    Ok(())
                } else {
                    Err(Error::Unavailable)
                };
            }
            let acl = Acl(acl);
            let mut entry = std::ptr::null_mut();
            // SAFETY: the ACL and output pointer are valid for this call.
            match unsafe { acl_get_entry(acl.0, ACL_FIRST_ENTRY, &mut entry) } {
                0 => Err(Error::Present),
                _ => Err(Error::Unavailable),
            }
        }

        pub(super) fn require_path(path: &Path) -> Result<(), Error> {
            let path = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| Error::Unavailable)?;
            // SAFETY: `path` is NUL-terminated and live for the call.
            require_empty(unsafe { acl_get_file(path.as_ptr(), ACL_TYPE_EXTENDED) })
        }

        pub(super) fn require_file(file: &File) -> Result<(), Error> {
            // SAFETY: the descriptor is valid and remains borrowed.
            require_empty(unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) })
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn require_path(path: &std::path::Path) -> Result<(), Error> {
        darwin::require_path(path)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn require_file(file: &std::fs::File) -> Result<(), Error> {
        darwin::require_file(file)
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn require_path(_path: &std::path::Path) -> Result<(), Error> {
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn require_file(_file: &std::fs::File) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(not(unix))]
fn require_same_file(_: &std::fs::Metadata, _: &std::fs::File) -> Result<(), ConfigError> {
    Err(ConfigError::PermissionsUnsupported)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn config_is_owner_only_loopback_and_complete() {
        use std::{fs, os::unix::fs::PermissionsExt as _};

        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("peers.json");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"peers":[{"nodeId":"lab-a","endpoint":"127.0.0.1:7443","securityMode":"externalSshOrMtls","tunnelId":"ssh-lab","ownerId":"ssh-lab","leaseTtlMs":30000,"renewBeforeMs":5000}]}"#,
        )
        .expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        let config = super::ConfiguredPeers::load(&path).expect("load");
        assert_eq!(config.peers().len(), 1);
        assert!(config.peers()[0].endpoint().ip().is_loopback());
        let peer_debug = format!("{:?}", config.peers()[0]);
        let peers_debug = format!("{config:?}");
        assert!(!peer_debug.contains("ssh-lab"));
        assert!(!peers_debug.contains("ssh-lab"));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("unsafe permissions");
        assert_eq!(
            super::ConfiguredPeers::load(&path),
            Err(super::ConfigError::UnsafeFile)
        );
    }

    #[cfg(unix)]
    #[test]
    fn peer_server_config_is_owner_only_bounded_loopback_and_redacted() {
        use std::{fs, os::unix::fs::PermissionsExt as _};

        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("peer-server-path-marker.json");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"nodeId":"node-a","listen":"127.0.0.1:7443","securityMode":"externalSshOrMtls","tunnelId":"sensitive-tunnel-marker","nodeEpoch":17,"inventoryRevision":3}"#,
        )
        .expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");

        let config = super::ConfiguredPeerServer::load(&path).expect("load peer server");
        assert_eq!(config.node_id().as_str(), "node-a");
        assert_eq!(config.listen(), "127.0.0.1:7443".parse().expect("address"));
        assert_eq!(config.tunnel_id(), "sensitive-tunnel-marker");
        assert_eq!(config.node_epoch(), 17);
        assert_eq!(config.inventory_revision(), 3);
        let debug = format!("{config:?}");
        assert!(!debug.contains("sensitive-tunnel-marker"));
        assert!(!debug.contains("peer-server-path-marker"));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("unsafe permissions");
        assert_eq!(
            super::ConfiguredPeerServer::load(&path),
            Err(super::ConfigError::UnsafeFile)
        );
    }

    #[cfg(unix)]
    #[test]
    fn peer_server_config_rejects_ambiguous_or_unbounded_fields() {
        use std::{fs, os::unix::fs::PermissionsExt as _};

        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("peer-server.json");
        let valid = serde_json::json!({
            "schemaVersion": 1,
            "nodeId": "node-a",
            "listen": "[::1]:7443",
            "securityMode": "externalSshOrMtls",
            "tunnelId": "ssh-node-a",
            "nodeEpoch": 17,
            "inventoryRevision": 3
        });
        let mut invalid = Vec::new();
        for (field, value) in [
            ("listen", serde_json::json!("203.0.113.7:7443")),
            ("listen", serde_json::json!("127.0.0.1:0")),
            ("listen", serde_json::json!("localhost:7443")),
            ("securityMode", serde_json::json!("rawTcp")),
            ("tunnelId", serde_json::json!("")),
            ("nodeEpoch", serde_json::json!(0)),
            ("nodeEpoch", serde_json::json!(9_007_199_254_740_992_u64)),
            ("inventoryRevision", serde_json::json!(0)),
            (
                "inventoryRevision",
                serde_json::json!(9_007_199_254_740_992_u64),
            ),
        ] {
            let mut candidate = valid.clone();
            candidate[field] = value;
            invalid.push(candidate);
        }
        let mut unknown = valid.clone();
        unknown["unexpected"] = serde_json::json!(true);
        invalid.push(unknown);
        let mut incomplete = valid;
        incomplete
            .as_object_mut()
            .expect("object")
            .remove("tunnelId");
        invalid.push(incomplete);

        for candidate in invalid {
            fs::write(&path, serde_json::to_vec(&candidate).expect("serialize")).expect("write");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
            assert_eq!(
                super::ConfiguredPeerServer::load(&path),
                Err(super::ConfigError::Invalid),
                "candidate unexpectedly accepted: {candidate}"
            );
        }

        fs::write(&path, vec![b' '; (super::MAX_CONFIG_BYTES + 1) as usize])
            .expect("write oversized config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        assert_eq!(
            super::ConfiguredPeerServer::load(&path),
            Err(super::ConfigError::TooLarge)
        );
    }

    #[cfg(unix)]
    #[test]
    fn public_or_incomplete_peer_config_fails_closed() {
        use std::{fs, os::unix::fs::PermissionsExt as _};

        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("peers.json");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"peers":[{"nodeId":"lab-a","endpoint":"203.0.113.7:7443","securityMode":"externalSshOrMtls","tunnelId":"ssh-lab","ownerId":"ssh-lab","leaseTtlMs":30000,"renewBeforeMs":5000}]}"#,
        )
        .expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        assert_eq!(
            super::ConfiguredPeers::load(&path),
            Err(super::ConfigError::Invalid)
        );
    }

    #[cfg(unix)]
    #[test]
    fn permission_change_after_open_fails_closed() {
        use std::{fs, fs::OpenOptions, os::unix::fs::PermissionsExt as _};

        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("peers.json");
        fs::write(&path, b"{}").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private");
        let before = path.symlink_metadata().expect("metadata");
        let file = OpenOptions::new().read(true).open(&path).expect("open");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("public");
        assert_eq!(
            super::require_same_file(&before, &file),
            Err(super::ConfigError::UnsafeFile)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn extended_acl_on_private_config_fails_closed() {
        use std::{fs, os::unix::fs::PermissionsExt as _, process::Command};

        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("peers.json");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"peers":[{"nodeId":"lab-a","endpoint":"127.0.0.1:7443","securityMode":"externalSshOrMtls","tunnelId":"ssh-lab","ownerId":"ssh-lab","leaseTtlMs":30000,"renewBeforeMs":5000}]}"#,
        )
        .expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private mode");
        let status = Command::new("chmod")
            .args(["+a", "everyone allow read"])
            .arg(&path)
            .status()
            .expect("chmod ACL");
        assert!(status.success());
        assert_eq!(
            super::ConfiguredPeers::load(&path),
            Err(super::ConfigError::UnsafeFile)
        );
    }
}
