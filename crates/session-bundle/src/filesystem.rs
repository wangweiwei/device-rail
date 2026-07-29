//! Filesystem primitives for reading and publishing Session Bundle v1.
//!
//! Bundle paths are derived here from fixed format components.  Callers never
//! supply a manifest path or an asset-relative path from bundle contents.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

pub const MANIFEST_FILE_NAME: &str = "manifest.json";
pub const ASSETS_DIRECTORY_NAME: &str = "assets";
pub const SHA256_DIRECTORY_NAME: &str = "sha256";

/// A structural or I/O failure while inspecting or publishing a bundle.
#[derive(Debug)]
pub enum FilesystemError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    InvalidTarget(PathBuf),
    UnexpectedNode {
        path: PathBuf,
        expected: &'static str,
    },
    UnexpectedEntry(PathBuf),
    MissingManifest(PathBuf),
    InvalidAssetDigest(PathBuf),
    EmptyAssetsTree(PathBuf),
    LimitExceeded {
        resource: &'static str,
        limit: usize,
    },
    DifferentParents {
        source: PathBuf,
        target: PathBuf,
    },
    DestinationExists(PathBuf),
    AtomicNoReplaceUnsupported,
    /// The directory was published, but syncing its parent failed.  The
    /// caller must report publication with unknown durability, never cancel.
    PublishedDurabilityUnknown {
        target: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for FilesystemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} {}: {source}",
                path.display()
            ),
            Self::InvalidTarget(path) => {
                write!(formatter, "invalid bundle target {}", path.display())
            }
            Self::UnexpectedNode { path, expected } => {
                write!(formatter, "{} is not {expected}", path.display())
            }
            Self::UnexpectedEntry(path) => {
                write!(formatter, "unexpected bundle entry {}", path.display())
            }
            Self::MissingManifest(path) => {
                write!(
                    formatter,
                    "bundle manifest is missing at {}",
                    path.display()
                )
            }
            Self::InvalidAssetDigest(path) => {
                write!(formatter, "invalid SHA-256 asset name {}", path.display())
            }
            Self::EmptyAssetsTree(path) => write!(
                formatter,
                "empty asset tree must be omitted instead of stored at {}",
                path.display()
            ),
            Self::LimitExceeded { resource, limit } => {
                write!(formatter, "{resource} count exceeds limit {limit}")
            }
            Self::DifferentParents { source, target } => write!(
                formatter,
                "staging directory {} and target {} do not share a parent",
                source.display(),
                target.display()
            ),
            Self::DestinationExists(path) => {
                write!(
                    formatter,
                    "bundle target already exists at {}",
                    path.display()
                )
            }
            Self::AtomicNoReplaceUnsupported => formatter.write_str(
                "this platform has no supported atomic directory rename-without-replacement",
            ),
            Self::PublishedDurabilityUnknown { target, source } => write!(
                formatter,
                "bundle was published at {}, but parent sync failed: {source}",
                target.display()
            ),
        }
    }
}

impl std::error::Error for FilesystemError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::PublishedDurabilityUnknown { source, .. } => {
                Some(source)
            }
            _ => None,
        }
    }
}

/// Paths proven to have the exact Session Bundle v1 directory shape.
#[derive(Debug, Eq, PartialEq)]
pub struct InspectedBundleTree {
    pub root: PathBuf,
    pub manifest: PathBuf,
    /// Lowercase SHA-256 digest to its fixed, derived path.
    pub assets: BTreeMap<String, PathBuf>,
}

/// Return the only valid manifest path for a bundle root.
pub fn manifest_path(root: &Path) -> PathBuf {
    root.join(MANIFEST_FILE_NAME)
}

/// Return the only valid asset path for a digest.
pub fn asset_path(root: &Path, digest: &str) -> Result<PathBuf, FilesystemError> {
    if !is_lowercase_sha256(digest) {
        return Err(FilesystemError::InvalidAssetDigest(
            root.join(ASSETS_DIRECTORY_NAME)
                .join(SHA256_DIRECTORY_NAME)
                .join(digest),
        ));
    }
    Ok(root
        .join(ASSETS_DIRECTORY_NAME)
        .join(SHA256_DIRECTORY_NAME)
        .join(digest))
}

/// Inspect a bundle without following symlinks and reject every extra node.
///
/// This validates a filesystem snapshot.  Readers should subsequently use
/// [`open_regular_file_nofollow`] so a replaced final component also fails.
pub fn inspect_bundle_tree(
    root: &Path,
    max_assets: usize,
) -> Result<InspectedBundleTree, FilesystemError> {
    require_real_directory(root)?;

    let expected_manifest = manifest_path(root);
    let expected_assets = root.join(ASSETS_DIRECTORY_NAME);
    let mut saw_manifest = false;
    let mut saw_assets = false;

    for entry in read_directory(root)? {
        let entry = entry.map_err(|source| io_error("read bundle entry", root, source))?;
        let path = entry.path();
        if entry.file_name() == OsStr::new(MANIFEST_FILE_NAME) {
            require_regular_file(&path)?;
            saw_manifest = true;
        } else if entry.file_name() == OsStr::new(ASSETS_DIRECTORY_NAME) {
            require_real_directory(&path)?;
            saw_assets = true;
        } else {
            return Err(FilesystemError::UnexpectedEntry(path));
        }
    }

    if !saw_manifest {
        return Err(FilesystemError::MissingManifest(expected_manifest));
    }

    let assets = if saw_assets {
        inspect_assets_tree(&expected_assets, max_assets)?
    } else {
        BTreeMap::new()
    };

    Ok(InspectedBundleTree {
        root: root.to_path_buf(),
        manifest: expected_manifest,
        assets,
    })
}

fn inspect_assets_tree(
    assets_root: &Path,
    max_assets: usize,
) -> Result<BTreeMap<String, PathBuf>, FilesystemError> {
    let sha256_root = assets_root.join(SHA256_DIRECTORY_NAME);
    let mut saw_sha256 = false;
    for entry in read_directory(assets_root)? {
        let entry = entry.map_err(|source| io_error("read assets entry", assets_root, source))?;
        let path = entry.path();
        if entry.file_name() != OsStr::new(SHA256_DIRECTORY_NAME) {
            return Err(FilesystemError::UnexpectedEntry(path));
        }
        require_real_directory(&path)?;
        saw_sha256 = true;
    }
    if !saw_sha256 {
        return Err(FilesystemError::EmptyAssetsTree(assets_root.to_path_buf()));
    }

    let mut assets = BTreeMap::new();
    for entry in read_directory(&sha256_root)? {
        let entry = entry.map_err(|source| io_error("read SHA-256 entry", &sha256_root, source))?;
        let observed = assets
            .len()
            .checked_add(1)
            .ok_or(FilesystemError::LimitExceeded {
                resource: "asset",
                limit: max_assets,
            })?;
        if observed > max_assets {
            return Err(FilesystemError::LimitExceeded {
                resource: "asset",
                limit: max_assets,
            });
        }
        let path = entry.path();
        let Some(digest) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            return Err(FilesystemError::InvalidAssetDigest(path));
        };
        if !is_lowercase_sha256(&digest) {
            return Err(FilesystemError::InvalidAssetDigest(path));
        }
        require_regular_file(&path)?;
        assets.insert(digest, path);
    }
    if assets.is_empty() {
        return Err(FilesystemError::EmptyAssetsTree(assets_root.to_path_buf()));
    }
    Ok(assets)
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn read_directory(path: &Path) -> Result<fs::ReadDir, FilesystemError> {
    fs::read_dir(path).map_err(|source| io_error("read directory", path, source))
}

fn require_real_directory(path: &Path) -> Result<(), FilesystemError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if metadata_is_link_like(&metadata) || !metadata.is_dir() {
        return Err(FilesystemError::UnexpectedNode {
            path: path.to_path_buf(),
            expected: "a real directory",
        });
    }
    Ok(())
}

fn require_regular_file(path: &Path) -> Result<(), FilesystemError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect file", path, source))?;
    if metadata_is_link_like(&metadata) || !metadata.is_file() {
        return Err(FilesystemError::UnexpectedNode {
            path: path.to_path_buf(),
            expected: "a regular file",
        });
    }
    Ok(())
}

pub fn metadata_is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Open a regular file while refusing a symlink in its final component.
pub fn open_regular_file_nofollow(path: &Path) -> Result<fs::File, FilesystemError> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags, open};

        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|source| {
            io_error(
                "open regular file",
                path,
                io::Error::from_raw_os_error(source.raw_os_error()),
            )
        })?;
        fs::File::from(descriptor)
    };

    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|source| {
                io_error(
                    "open regular file without following reparse point",
                    path,
                    source,
                )
            })?
    };

    #[cfg(all(not(unix), not(windows)))]
    let file = {
        require_regular_file(path)?;
        fs::File::open(path).map_err(|source| io_error("open regular file", path, source))?
    };

    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect open file", path, source))?;
    if metadata_is_link_like(&metadata) || !metadata.is_file() {
        return Err(FilesystemError::UnexpectedNode {
            path: path.to_path_buf(),
            expected: "a regular file",
        });
    }
    Ok(file)
}

/// Flush directory entry changes to stable storage.
pub fn sync_directory(path: &Path) -> Result<(), FilesystemError> {
    sync_directory_raw(path).map_err(|source| io_error("sync directory", path, source))
}

fn sync_directory_raw(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, open};

        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|source| io::Error::from_raw_os_error(source.raw_os_error()))?;
        fs::File::from(descriptor).sync_all()
    }

    // Windows has no direct equivalent of POSIX directory fsync. Every file
    // is flushed individually and publication uses MoveFileExW with
    // MOVEFILE_WRITE_THROUGH below.
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

/// Private staging directory next to its final target.
///
/// Unix creates it as `0700`; other platforms inherit the chosen parent's
/// access policy, so callers must select a private output parent there.
///
/// Dropping an uncommitted guard recursively removes the staging directory.
#[derive(Debug)]
pub struct StagingDir {
    path: PathBuf,
    target: PathBuf,
    parent: PathBuf,
    published: bool,
}

impl StagingDir {
    pub fn create(target: &Path) -> Result<Self, FilesystemError> {
        if target.file_name().is_none() {
            return Err(FilesystemError::InvalidTarget(target.to_path_buf()));
        }
        let parent = normalized_parent(target)
            .ok_or_else(|| FilesystemError::InvalidTarget(target.to_path_buf()))?;
        require_real_directory(&parent)?;

        for _ in 0..16 {
            let path = parent.join(format!(
                ".devicerail-session-bundle-staging-{}",
                Uuid::new_v4()
            ));
            match create_owner_only_directory(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        target: target.to_path_buf(),
                        parent,
                        published: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(io_error("create staging directory", &path, source)),
            }
        }

        Err(io_error(
            "create staging directory",
            &parent,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a unique staging directory",
            ),
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Publish with no replacement.  Once rename succeeds the guard is
    /// disarmed before syncing the parent; a sync failure therefore cannot
    /// delete or misreport the published target.
    pub fn commit(mut self) -> Result<(), FilesystemError> {
        sync_directory(&self.path)?;
        no_replace_directory_commit(&self.path, &self.target)?;
        self.published = true;

        if let Err(source) = sync_directory_raw(&self.parent) {
            return Err(FilesystemError::PublishedDurabilityUnknown {
                target: self.target.clone(),
                source,
            });
        }
        Ok(())
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = remove_staging_path(&self.path);
        }
    }
}

#[cfg(unix)]
fn create_owner_only_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_owner_only_directory(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

fn remove_staging_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata_is_link_like(&metadata) && metadata.is_dir() => {
            fs::remove_dir(path)
        }
        Ok(metadata) if metadata_is_link_like(&metadata) || !metadata.is_dir() => {
            fs::remove_file(path)
        }
        Ok(_) => fs::remove_dir_all(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Atomically rename a directory without replacing an existing target.
pub fn no_replace_directory_commit(source: &Path, target: &Path) -> Result<(), FilesystemError> {
    if normalized_parent(source) != normalized_parent(target) {
        return Err(FilesystemError::DifferentParents {
            source: source.to_path_buf(),
            target: target.to_path_buf(),
        });
    }

    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "redox"))]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};

        renameat_with(CWD, source, CWD, target, RenameFlags::NOREPLACE).map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                FilesystemError::DestinationExists(target.to_path_buf())
            } else {
                io_error(
                    "publish staging directory",
                    target,
                    io::Error::from_raw_os_error(error.raw_os_error()),
                )
            }
        })
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

        let source_wide = source
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let target_wide = target
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: both arguments are owned, NUL-terminated UTF-16 buffers
        // that remain alive for the call. Omitting MOVEFILE_REPLACE_EXISTING
        // is the no-clobber contract; WRITE_THROUGH is the Windows durability
        // boundary for the same-volume directory move.
        let moved = unsafe {
            MoveFileExW(
                source_wide.as_ptr(),
                target_wide.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved != 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if matches!(source.raw_os_error(), Some(80 | 183)) {
            Err(FilesystemError::DestinationExists(target.to_path_buf()))
        } else {
            Err(io_error("publish staging directory", target, source))
        }
    }

    // Fail closed on platforms for which this crate has no proven atomic
    // rename-without-replacement primitive. A preflight followed by plain
    // `rename` would allow a racing target to be replaced on some Unix hosts.
    #[cfg(not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "redox",
        windows
    )))]
    {
        Err(FilesystemError::AtomicNoReplaceUnsupported)
    }
}

fn normalized_parent(path: &Path) -> Option<PathBuf> {
    path.parent().map(|parent| {
        if parent.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            parent.to_path_buf()
        }
    })
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> FilesystemError {
    FilesystemError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn create_valid_bundle() -> TempDir {
        let temporary = TempDir::new().expect("temporary directory");
        fs::write(manifest_path(temporary.path()), b"{}\n").expect("manifest");
        let asset = asset_path(temporary.path(), DIGEST).expect("asset path");
        fs::create_dir_all(asset.parent().expect("asset parent")).expect("asset tree");
        fs::write(asset, b"asset").expect("asset");
        temporary
    }

    #[test]
    fn exact_bundle_tree_is_accepted() {
        let temporary = create_valid_bundle();
        let inspected = inspect_bundle_tree(temporary.path(), 1).expect("inspect bundle");

        assert_eq!(inspected.manifest, manifest_path(temporary.path()));
        assert_eq!(
            inspected.assets.get(DIGEST),
            Some(&asset_path(temporary.path(), DIGEST).expect("asset path"))
        );
    }

    #[test]
    fn asset_limit_is_enforced_before_collecting_entries() {
        let temporary = create_valid_bundle();

        assert!(matches!(
            inspect_bundle_tree(temporary.path(), 0),
            Err(FilesystemError::LimitExceeded {
                resource: "asset",
                limit: 0
            })
        ));
    }

    #[test]
    fn extra_entry_is_rejected() {
        let temporary = create_valid_bundle();
        fs::write(temporary.path().join("extra"), b"no").expect("extra entry");

        assert!(matches!(
            inspect_bundle_tree(temporary.path(), 1),
            Err(FilesystemError::UnexpectedEntry(_))
        ));
    }

    #[test]
    fn empty_asset_tree_is_rejected() {
        let temporary = TempDir::new().expect("temporary directory");
        fs::write(manifest_path(temporary.path()), b"{}\n").expect("manifest");
        fs::create_dir_all(
            temporary
                .path()
                .join(ASSETS_DIRECTORY_NAME)
                .join(SHA256_DIRECTORY_NAME),
        )
        .expect("asset tree");

        assert!(matches!(
            inspect_bundle_tree(temporary.path(), 1),
            Err(FilesystemError::EmptyAssetsTree(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_manifest_and_asset_are_rejected() {
        use std::os::unix::fs::symlink;

        let temporary = create_valid_bundle();
        let external_root = TempDir::new().expect("external temporary directory");
        let external = external_root.path().join("external");
        fs::write(&external, b"external").expect("external file");

        fs::remove_file(manifest_path(temporary.path())).expect("remove manifest");
        symlink(&external, manifest_path(temporary.path())).expect("manifest symlink");
        assert!(matches!(
            inspect_bundle_tree(temporary.path(), 1),
            Err(FilesystemError::UnexpectedNode { .. })
        ));

        fs::remove_file(manifest_path(temporary.path())).expect("remove manifest symlink");
        fs::write(manifest_path(temporary.path()), b"{}\n").expect("manifest");
        let asset = asset_path(temporary.path(), DIGEST).expect("asset path");
        fs::remove_file(&asset).expect("remove asset");
        symlink(&external, &asset).expect("asset symlink");
        assert!(matches!(
            inspect_bundle_tree(temporary.path(), 1),
            Err(FilesystemError::UnexpectedNode { .. })
        ));
    }

    #[test]
    fn uncommitted_staging_guard_cleans_up() {
        let temporary = TempDir::new().expect("temporary directory");
        let target = temporary.path().join("bundle");
        let guard = StagingDir::create(&target).expect("staging guard");
        let staging_path = guard.path().to_path_buf();
        fs::File::create(staging_path.join("partial"))
            .expect("partial file")
            .write_all(b"partial")
            .expect("write partial file");

        drop(guard);
        assert!(!staging_path.exists());
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn staging_directory_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TempDir::new().expect("temporary directory");
        let guard = StagingDir::create(&temporary.path().join("bundle")).expect("staging guard");
        let mode = fs::metadata(guard.path())
            .expect("staging metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn existing_target_is_preserved_and_staging_is_removed() {
        let temporary = TempDir::new().expect("temporary directory");
        let target = temporary.path().join("bundle");
        fs::create_dir(&target).expect("target");
        fs::write(target.join("marker"), b"existing").expect("target marker");

        let guard = StagingDir::create(&target).expect("staging guard");
        let staging_path = guard.path().to_path_buf();
        fs::write(guard.path().join("marker"), b"new").expect("staging marker");

        assert!(matches!(
            guard.commit(),
            Err(FilesystemError::DestinationExists(path)) if path == target
        ));
        assert_eq!(
            fs::read(target.join("marker")).expect("target marker"),
            b"existing"
        );
        assert!(!staging_path.exists());
    }

    #[test]
    fn commit_publishes_staging_directory() {
        let temporary = TempDir::new().expect("temporary directory");
        let target = temporary.path().join("bundle");
        let guard = StagingDir::create(&target).expect("staging guard");
        fs::write(guard.path().join("marker"), b"published").expect("staging marker");

        guard.commit().expect("commit staging directory");

        assert_eq!(
            fs::read(target.join("marker")).expect("target marker"),
            b"published"
        );
    }
}
