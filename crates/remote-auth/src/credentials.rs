use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::Read as _,
    path::Path,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::Permission;

const MAX_CREDENTIAL_BYTES: u64 = 128 * 1024;
const MAX_PRINCIPALS: usize = 64;
const MIN_SECRET_BYTES: usize = 32;
const MAX_SECRET_BYTES: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("credential store must be an owner-only regular file")]
    UnsafeFile,
    #[error("credential store owner-only permissions cannot be verified on this platform")]
    PermissionsUnsupported,
    #[error("credential store exceeds its size limit")]
    TooLarge,
    #[error("credential store could not be read")]
    Read,
    #[error("credential store does not match schema version 1")]
    Invalid,
}

impl CredentialError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsafeFile => "credential_file_unsafe",
            Self::PermissionsUnsupported => "credential_permissions_unsupported",
            Self::TooLarge => "credential_store_limit",
            Self::Read => "credential_read_failed",
            Self::Invalid => "credential_store_invalid",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialFile {
    schema_version: u8,
    principals: Vec<CredentialEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialEntry {
    principal_id: String,
    key_id: String,
    secret_base64: SecretString,
    permissions: Vec<Permission>,
}

/// Keeps the reversible on-disk representation out of freed heap pages just
/// like the decoded credential. Deserializing directly into this wrapper
/// avoids an additional non-zeroizing copy of the secret string.
struct SecretString(Zeroizing<String>);

impl SecretString {
    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(|value| Self(Zeroizing::new(value)))
    }
}

pub(crate) struct Credential {
    pub(crate) principal_id: String,
    pub(crate) secret: Zeroizing<Vec<u8>>,
    pub(crate) permissions: BTreeSet<Permission>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Credential")
            .field("principal_id", &self.principal_id)
            .field("secret", &"[REDACTED]")
            .field("permissions", &self.permissions)
            .finish()
    }
}

pub struct CredentialStore {
    credentials: BTreeMap<(String, String), Credential>,
}

impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialStore")
            .field("credential_count", &self.credentials.len())
            .finish()
    }
}

impl CredentialStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CredentialError> {
        // The JSON contains a reversible base64url credential. Wipe the raw
        // file buffer on both success and every decode error path.
        let bytes = Zeroizing::new(read_owner_only(path.as_ref())?);
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, CredentialError> {
        let file: CredentialFile =
            serde_json::from_slice(bytes).map_err(|_| CredentialError::Invalid)?;
        if file.schema_version != 1
            || file.principals.is_empty()
            || file.principals.len() > MAX_PRINCIPALS
        {
            return Err(CredentialError::Invalid);
        }
        let mut credentials = BTreeMap::new();
        let mut principal_permissions = BTreeMap::<String, BTreeSet<Permission>>::new();
        for entry in file.principals {
            if !valid_identifier(&entry.principal_id)
                || !valid_identifier(&entry.key_id)
                || entry.permissions.is_empty()
                || entry.permissions.len() > 3
            {
                return Err(CredentialError::Invalid);
            }
            let permission_count = entry.permissions.len();
            let permissions = entry.permissions.into_iter().collect::<BTreeSet<_>>();
            if permissions.len() != permission_count
                || principal_permissions
                    .get(&entry.principal_id)
                    .is_some_and(|known| known != &permissions)
            {
                return Err(CredentialError::Invalid);
            }
            principal_permissions.insert(entry.principal_id.clone(), permissions.clone());
            let decoded = Zeroizing::new(
                URL_SAFE_NO_PAD
                    .decode(entry.secret_base64.as_str().as_bytes())
                    .map_err(|_| CredentialError::Invalid)?,
            );
            // Canonicalization is also a reversible representation of the
            // credential, so wipe this comparison buffer when the entry exits.
            let canonical = Zeroizing::new(URL_SAFE_NO_PAD.encode(decoded.as_slice()));
            if decoded.len() < MIN_SECRET_BYTES
                || decoded.len() > MAX_SECRET_BYTES
                || canonical.as_str() != entry.secret_base64.as_str()
            {
                return Err(CredentialError::Invalid);
            }
            let key = (entry.principal_id.clone(), entry.key_id);
            if credentials
                .insert(
                    key,
                    Credential {
                        principal_id: entry.principal_id,
                        secret: decoded,
                        permissions,
                    },
                )
                .is_some()
            {
                return Err(CredentialError::Invalid);
            }
        }
        Ok(Self { credentials })
    }

    pub(crate) fn lookup(&self, principal_id: &str, key_id: &str) -> Option<&Credential> {
        self.credentials
            .get(&(principal_id.to_owned(), key_id.to_owned()))
    }
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
}

fn read_owner_only(path: &Path) -> Result<Vec<u8>, CredentialError> {
    let path_metadata = path
        .symlink_metadata()
        .map_err(|_| CredentialError::UnsafeFile)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(CredentialError::UnsafeFile);
    }
    require_owner_only(&path_metadata)?;
    require_acl_path(path)?;
    if path_metadata.len() > MAX_CREDENTIAL_BYTES {
        return Err(CredentialError::TooLarge);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let mut file = options
        .open(path)
        .map_err(|_| CredentialError::UnsafeFile)?;
    require_same_file(&path_metadata, &file)?;
    let mut bytes = Vec::with_capacity(path_metadata.len() as usize);
    file.by_ref()
        .take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CredentialError::Read)?;
    let opened = file.metadata().map_err(|_| CredentialError::UnsafeFile)?;
    require_same_file(&path_metadata, &file)?;
    if opened.len() != bytes.len() as u64 {
        return Err(CredentialError::UnsafeFile);
    }
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(CredentialError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn require_owner_only(metadata: &std::fs::Metadata) -> Result<(), CredentialError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(CredentialError::UnsafeFile);
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_owner_only(_metadata: &std::fs::Metadata) -> Result<(), CredentialError> {
    Err(CredentialError::PermissionsUnsupported)
}

#[cfg(unix)]
fn require_same_file(before: &std::fs::Metadata, file: &File) -> Result<(), CredentialError> {
    use std::os::unix::fs::MetadataExt as _;

    let opened = file.metadata().map_err(|_| CredentialError::UnsafeFile)?;
    // chmod/chown do not change mtime. Re-check the live descriptor so a
    // concurrent permission change cannot pass the inode/length comparison.
    require_owner_only(&opened)?;
    require_acl_file(file)?;
    if opened.dev() != before.dev()
        || opened.ino() != before.ino()
        || opened.len() != before.len()
        || opened.mtime() != before.mtime()
        || opened.mtime_nsec() != before.mtime_nsec()
    {
        return Err(CredentialError::UnsafeFile);
    }
    Ok(())
}

#[cfg(unix)]
fn require_acl_path(path: &Path) -> Result<(), CredentialError> {
    crate::owner_only::require_no_extended_acl_path(path).map_err(|error| match error {
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Present => CredentialError::UnsafeFile,
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Unavailable => CredentialError::PermissionsUnsupported,
    })
}

#[cfg(not(unix))]
fn require_acl_path(_path: &Path) -> Result<(), CredentialError> {
    Err(CredentialError::PermissionsUnsupported)
}

#[cfg(unix)]
fn require_acl_file(file: &File) -> Result<(), CredentialError> {
    crate::owner_only::require_no_extended_acl_file(file).map_err(|error| match error {
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Present => CredentialError::UnsafeFile,
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Unavailable => CredentialError::PermissionsUnsupported,
    })
}

#[cfg(not(unix))]
fn require_same_file(_before: &std::fs::Metadata, _file: &File) -> Result<(), CredentialError> {
    Err(CredentialError::PermissionsUnsupported)
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        os::unix::fs::PermissionsExt as _,
    };

    use super::{CredentialError, require_same_file};

    #[test]
    fn descriptor_permission_change_is_rejected_after_open() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("credentials.json");
        fs::write(&path, b"credential bytes").expect("write fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("owner-only mode");
        let before = path.symlink_metadata().expect("metadata");
        let file = OpenOptions::new().read(true).open(&path).expect("open");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("unsafe mode");
        assert_eq!(
            require_same_file(&before, &file),
            Err(CredentialError::UnsafeFile)
        );
    }
}
