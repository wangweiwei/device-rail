use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::{collections::BTreeSet, fs, io::Read as _, path::Component};

use devicerail_protocol::ProtocolVersion;
#[cfg(unix)]
use devicerail_protocol::{
    ProtocolOffer, ProtocolRange, negotiate_protocol, supported_protocol_offer,
};
#[cfg(unix)]
use serde_json::Value;
use thiserror::Error;

#[cfg(unix)]
use crate::owner_only::{
    ExtendedAclError, require_no_extended_acl_file, require_no_extended_acl_path,
};
#[cfg(unix)]
use crate::{PLUGIN_ABI_VERSION, PLUGIN_MANIFEST_SCHEMA};
use crate::{PluginManifest, PluginTransportConfig};

#[cfg(unix)]
const MANIFEST_SUFFIX: &str = ".devicerail-plugin.json";
const MAX_DIRECTORIES: usize = 16;
#[cfg(unix)]
const MAX_DIRECTORY_ENTRIES: usize = 256;
#[cfg(unix)]
const MAX_MANIFESTS: usize = 64;
#[cfg(unix)]
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
#[cfg(unix)]
const MAX_PATH_COMPONENTS: usize = 8;

#[derive(Clone, PartialEq, Eq)]
pub struct DiscoveryConfig {
    directories: Vec<PathBuf>,
    transport: PluginTransportConfig,
}

impl std::fmt::Debug for DiscoveryConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DiscoveryConfig")
            .field("directory_count", &self.directories.len())
            .field("directories", &"[VALIDATED AT DISCOVERY]")
            .field("transport", &self.transport)
            .finish()
    }
}

impl DiscoveryConfig {
    pub fn new(directories: Vec<PathBuf>) -> Result<Self, PluginDiscoveryError> {
        if directories.is_empty()
            || directories.len() > MAX_DIRECTORIES
            || directories
                .iter()
                .any(|directory| directory.as_os_str().is_empty() || !directory.is_absolute())
        {
            return Err(PluginDiscoveryError::InvalidConfiguration);
        }
        Ok(Self {
            directories,
            transport: PluginTransportConfig::default(),
        })
    }

    pub fn with_command_timeout(mut self, timeout: Duration) -> Result<Self, PluginDiscoveryError> {
        self.transport = PluginTransportConfig::new(timeout)
            .map_err(|_| PluginDiscoveryError::InvalidConfiguration)?;
        Ok(self)
    }

    pub fn directories(&self) -> &[PathBuf] {
        &self.directories
    }

    pub fn transport(&self) -> PluginTransportConfig {
        self.transport
    }
}

#[derive(Clone)]
pub struct PluginDescriptor {
    manifest: PluginManifest,
    executable: PathBuf,
    selected_protocol: ProtocolVersion,
    transport: PluginTransportConfig,
}

impl PluginDescriptor {
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn selected_protocol(&self) -> ProtocolVersion {
        self.selected_protocol
    }

    pub fn transport(&self) -> PluginTransportConfig {
        self.transport
    }
}

impl std::fmt::Debug for PluginDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginDescriptor")
            .field("plugin_id", &self.manifest.plugin_id)
            .field("plugin_version", &self.manifest.plugin_version)
            .field("selected_protocol", &self.selected_protocol)
            .field("executable", &"[VALIDATED]")
            .finish()
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum PluginDiscoveryError {
    #[error("plugin discovery configuration is invalid")]
    InvalidConfiguration,
    #[error("plugin filesystem permissions cannot be proven on this platform")]
    PermissionsUnsupported,
    #[error("plugin directory is invalid or unsafe")]
    UnsafeDirectory,
    #[error("plugin directory exceeds the bounded entry limit")]
    DirectoryLimit,
    #[error("plugin manifest is invalid or unsafe")]
    InvalidManifest,
    #[error("plugin executable is invalid or unsafe")]
    UnsafeExecutable,
    #[error("plugin ABI is incompatible")]
    AbiIncompatible,
    #[error("plugin protocol is incompatible")]
    ProtocolIncompatible,
    #[error("plugin identity or capability declaration is duplicated")]
    DuplicateDeclaration,
    #[error("plugin discovery found no manifests")]
    NoPlugins,
}

impl PluginDiscoveryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "plugin_config_invalid",
            Self::PermissionsUnsupported => "plugin_permissions_unsupported",
            Self::UnsafeDirectory => "plugin_directory_unsafe",
            Self::DirectoryLimit => "plugin_discovery_limit",
            Self::InvalidManifest => "plugin_manifest_invalid",
            Self::UnsafeExecutable => "plugin_executable_unsafe",
            Self::AbiIncompatible => "plugin_abi_incompatible",
            Self::ProtocolIncompatible => "plugin_protocol_incompatible",
            Self::DuplicateDeclaration => "plugin_declaration_duplicate",
            Self::NoPlugins => "plugin_not_found",
        }
    }
}

pub fn discover_plugin_descriptors(
    config: &DiscoveryConfig,
) -> Result<Vec<PluginDescriptor>, PluginDiscoveryError> {
    #[cfg(unix)]
    {
        discover_plugin_descriptors_unix(config)
    }
    #[cfg(not(unix))]
    {
        let _ = config;
        Err(PluginDiscoveryError::PermissionsUnsupported)
    }
}

#[cfg(unix)]
fn discover_plugin_descriptors_unix(
    config: &DiscoveryConfig,
) -> Result<Vec<PluginDescriptor>, PluginDiscoveryError> {
    let manifest_schema: Value = serde_json::from_str(PLUGIN_MANIFEST_SCHEMA)
        .map_err(|_| PluginDiscoveryError::InvalidManifest)?;
    let validator = jsonschema::validator_for(&manifest_schema)
        .map_err(|_| PluginDiscoveryError::InvalidManifest)?;
    let mut manifest_paths = Vec::new();
    let mut seen_directories = BTreeSet::new();

    for configured in &config.directories {
        require_no_extended_acl_path(configured).map_err(map_directory_acl_error)?;
        let metadata =
            fs::symlink_metadata(configured).map_err(|_| PluginDiscoveryError::UnsafeDirectory)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || permissions_are_unsafe(&metadata)
            || !owned_by_current_process(&metadata)
        {
            return Err(PluginDiscoveryError::UnsafeDirectory);
        }
        let directory =
            fs::canonicalize(configured).map_err(|_| PluginDiscoveryError::UnsafeDirectory)?;
        if !seen_directories.insert(directory.clone()) {
            return Err(PluginDiscoveryError::DuplicateDeclaration);
        }
        let mut entries =
            fs::read_dir(&directory).map_err(|_| PluginDiscoveryError::UnsafeDirectory)?;
        for entry_index in 0..=MAX_DIRECTORY_ENTRIES {
            let Some(entry) = entries
                .next()
                .transpose()
                .map_err(|_| PluginDiscoveryError::UnsafeDirectory)?
            else {
                break;
            };
            if entry_index == MAX_DIRECTORY_ENTRIES {
                return Err(PluginDiscoveryError::DirectoryLimit);
            }
            let path = entry.path();
            let is_manifest = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(MANIFEST_SUFFIX));
            if is_manifest {
                manifest_paths.push((directory.clone(), path));
                if manifest_paths.len() > MAX_MANIFESTS {
                    return Err(PluginDiscoveryError::DirectoryLimit);
                }
            }
        }
    }
    if manifest_paths.is_empty() {
        return Err(PluginDiscoveryError::NoPlugins);
    }
    manifest_paths.sort_by(|left, right| left.1.cmp(&right.1));

    let mut plugin_ids = BTreeSet::new();
    let mut device_ids = BTreeSet::new();
    let mut descriptors = Vec::with_capacity(manifest_paths.len());
    for (directory, path) in manifest_paths {
        require_no_extended_acl_path(&path).map_err(map_manifest_acl_error)?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| PluginDiscoveryError::InvalidManifest)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_MANIFEST_BYTES
            || permissions_are_unsafe(&metadata)
            || !same_owner(
                &metadata,
                &fs::metadata(&directory).map_err(|_| PluginDiscoveryError::UnsafeDirectory)?,
            )
        {
            return Err(PluginDiscoveryError::InvalidManifest);
        }
        let source = open_manifest_no_follow(&path)?;
        require_no_extended_acl_file(&source).map_err(map_manifest_acl_error)?;
        let opened = source
            .metadata()
            .map_err(|_| PluginDiscoveryError::InvalidManifest)?;
        if !same_file(&metadata, &opened) || !owned_by_current_process(&opened) {
            return Err(PluginDiscoveryError::InvalidManifest);
        }
        let mut bytes = Vec::with_capacity(opened.len() as usize);
        source
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PluginDiscoveryError::InvalidManifest)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(PluginDiscoveryError::InvalidManifest);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| PluginDiscoveryError::InvalidManifest)?;
        let manifest: PluginManifest = serde_json::from_value(value.clone())
            .map_err(|_| PluginDiscoveryError::InvalidManifest)?;
        validate_manifest(&manifest)?;
        if !validator.is_valid(&value) {
            return Err(PluginDiscoveryError::InvalidManifest);
        }
        if !plugin_ids.insert(manifest.plugin_id.clone()) {
            return Err(PluginDiscoveryError::DuplicateDeclaration);
        }
        let device_id = format!("plugin:{}:{}", manifest.plugin_id, manifest.device.key);
        if !device_ids.insert(device_id) {
            return Err(PluginDiscoveryError::DuplicateDeclaration);
        }
        let selected_protocol = negotiate_manifest_protocol(&manifest)?;
        let executable = validate_executable(&directory, &manifest.executable, &metadata)?;
        descriptors.push(PluginDescriptor {
            manifest,
            executable,
            selected_protocol,
            transport: config.transport,
        });
    }
    Ok(descriptors)
}

#[cfg(unix)]
fn validate_manifest(manifest: &PluginManifest) -> Result<(), PluginDiscoveryError> {
    if manifest.manifest_version != 1 || manifest.abi_version != PLUGIN_ABI_VERSION {
        return Err(PluginDiscoveryError::AbiIncompatible);
    }
    if manifest.protocol.min_minor > manifest.protocol.max_minor {
        return Err(PluginDiscoveryError::ProtocolIncompatible);
    }
    let values = [
        manifest.plugin_id.as_str(),
        manifest.plugin_version.as_str(),
        manifest.device.key.as_str(),
        manifest.device.name.as_str(),
    ];
    if values
        .into_iter()
        .any(|value| value.trim().is_empty() || value.chars().any(char::is_control))
        || manifest
            .device
            .os_version
            .as_deref()
            .is_some_and(|value| value.chars().any(char::is_control))
    {
        return Err(PluginDiscoveryError::InvalidManifest);
    }
    if let devicerail_protocol::Platform::Other(value) = &manifest.device.platform {
        if value.trim().is_empty()
            || value.chars().count() > 64
            || value.chars().any(char::is_control)
        {
            return Err(PluginDiscoveryError::InvalidManifest);
        }
    }
    let mut names = BTreeSet::new();
    for capability in &manifest.capabilities {
        if !names.insert(capability.name.as_str()) {
            return Err(PluginDiscoveryError::DuplicateDeclaration);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn negotiate_manifest_protocol(
    manifest: &PluginManifest,
) -> Result<ProtocolVersion, PluginDiscoveryError> {
    let plugin = ProtocolOffer::new(vec![ProtocolRange::new(
        manifest.protocol.major,
        manifest.protocol.min_minor,
        manifest.protocol.max_minor,
    )]);
    negotiate_protocol(&supported_protocol_offer(), &plugin)
        .map_err(|_| PluginDiscoveryError::ProtocolIncompatible)
}

#[cfg(unix)]
fn validate_executable(
    directory: &Path,
    configured: &str,
    manifest_metadata: &fs::Metadata,
) -> Result<PathBuf, PluginDiscoveryError> {
    if configured.chars().any(char::is_control) {
        return Err(PluginDiscoveryError::UnsafeExecutable);
    }
    let relative = Path::new(configured);
    let components = relative.components().collect::<Vec<_>>();
    if relative.is_absolute()
        || components.is_empty()
        || components.len() > MAX_PATH_COMPONENTS
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PluginDiscoveryError::UnsafeExecutable);
    }
    let component_count = components.len();
    let mut current = directory.to_path_buf();
    for (index, component) in components.into_iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(PluginDiscoveryError::UnsafeExecutable);
        };
        current.push(component);
        require_no_extended_acl_path(&current).map_err(map_executable_acl_error)?;
        let metadata =
            fs::symlink_metadata(&current).map_err(|_| PluginDiscoveryError::UnsafeExecutable)?;
        if metadata.file_type().is_symlink() {
            return Err(PluginDiscoveryError::UnsafeExecutable);
        }
        if index + 1 < component_count
            && (!metadata.is_dir()
                || permissions_are_unsafe(&metadata)
                || !same_owner(&metadata, manifest_metadata))
        {
            return Err(PluginDiscoveryError::UnsafeExecutable);
        }
    }
    let canonical =
        fs::canonicalize(&current).map_err(|_| PluginDiscoveryError::UnsafeExecutable)?;
    let metadata = fs::metadata(&canonical).map_err(|_| PluginDiscoveryError::UnsafeExecutable)?;
    if !canonical.starts_with(directory)
        || !metadata.is_file()
        || permissions_are_unsafe(&metadata)
        || !is_executable(&metadata)
        || !owned_by_current_process(&metadata)
        || !same_owner(&metadata, manifest_metadata)
    {
        return Err(PluginDiscoveryError::UnsafeExecutable);
    }
    Ok(canonical)
}

#[cfg(unix)]
fn open_manifest_no_follow(path: &Path) -> Result<fs::File, PluginDiscoveryError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| PluginDiscoveryError::InvalidManifest)
}

#[cfg(unix)]
fn permissions_are_unsafe(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o022 != 0
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(unix)]
fn same_owner(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    left.uid() == right.uid()
}

#[cfg(unix)]
fn owned_by_current_process(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    // SAFETY: geteuid has no preconditions and does not retain pointers.
    metadata.uid() == unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(unix)]
fn map_directory_acl_error(error: ExtendedAclError) -> PluginDiscoveryError {
    match error {
        #[cfg(target_os = "macos")]
        ExtendedAclError::Present => PluginDiscoveryError::UnsafeDirectory,
        #[cfg(target_os = "macos")]
        ExtendedAclError::Unavailable => PluginDiscoveryError::PermissionsUnsupported,
    }
}

#[cfg(unix)]
fn map_manifest_acl_error(error: ExtendedAclError) -> PluginDiscoveryError {
    match error {
        #[cfg(target_os = "macos")]
        ExtendedAclError::Present => PluginDiscoveryError::InvalidManifest,
        #[cfg(target_os = "macos")]
        ExtendedAclError::Unavailable => PluginDiscoveryError::PermissionsUnsupported,
    }
}

#[cfg(unix)]
fn map_executable_acl_error(error: ExtendedAclError) -> PluginDiscoveryError {
    match error {
        #[cfg(target_os = "macos")]
        ExtendedAclError::Present => PluginDiscoveryError::UnsafeExecutable,
        #[cfg(target_os = "macos")]
        ExtendedAclError::Unavailable => PluginDiscoveryError::PermissionsUnsupported,
    }
}

#[cfg(all(test, not(unix)))]
mod tests {
    use super::*;

    #[test]
    fn discovery_fails_closed_when_owner_acl_cannot_be_proven() {
        let directory = std::env::current_dir().expect("current directory");
        let config = DiscoveryConfig::new(vec![directory]).expect("bounded absolute directory");
        let error = discover_plugin_descriptors(&config)
            .expect_err("non-Unix plugin discovery must fail closed");
        assert_eq!(error, PluginDiscoveryError::PermissionsUnsupported);
        assert_eq!(error.code(), "plugin_permissions_unsupported");
    }
}
