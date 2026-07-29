//! Filesystem-backed, content-addressed evidence storage.
//!
//! The configured root is an application-owned directory. DeviceRail creates
//! it with owner-only permissions on Unix, rejects symlinks inside the store,
//! and holds an exclusive process lock for the lifetime of the store. This
//! closes accidental path traversal and concurrent-writer races without
//! pretending to defend against a privileged process that can mutate open
//! files behind DeviceRail's back.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::RwLock as StdRwLock,
};

use async_trait::async_trait;
use devicerail_core::{
    EvidenceError, EvidenceInput, EvidenceMetadata, EvidenceOutput, EvidenceResult, EvidenceStore,
    GcPolicy, GcReport, PutEvidence, ReleaseReport, Sha256Digest, StoredEvidence, now_ms,
};
use devicerail_protocol::{AssetRef, SessionId};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _},
    sync::{Mutex, RwLock, Semaphore},
};
use uuid::Uuid;

const STORE_VERSION: u32 = 1;
const HASH_ALGORITHM: &str = "sha256";
const METADATA_LIMIT_BYTES: u64 = 64 * 1024;
const BUFFER_SIZE: usize = 64 * 1024;
const MUTATION_LOCK_STRIPES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileEvidenceStoreConfig {
    pub max_asset_bytes: u64,
    pub max_references_per_session: u64,
    pub max_concurrent_writes: usize,
}

impl Default for FileEvidenceStoreConfig {
    fn default() -> Self {
        Self {
            max_asset_bytes: 256 * 1024 * 1024,
            max_references_per_session: 10_000,
            max_concurrent_writes: 4,
        }
    }
}

/// A single-writer filesystem Evidence Store.
///
/// One instance owns an exclusive lock for its root. Clone/share the instance
/// through `Arc` instead of opening the same root twice.
pub struct FileEvidenceStore {
    root: PathBuf,
    objects: PathBuf,
    references: PathBuf,
    released_sessions: PathBuf,
    unreferenced: PathBuf,
    staging: PathBuf,
    trash: PathBuf,
    config: FileEvidenceStoreConfig,
    gate: RwLock<()>,
    object_gates: Vec<Mutex<()>>,
    session_gates: Vec<Mutex<()>>,
    reference_index: StdRwLock<Option<ReferenceIndex>>,
    write_slots: Semaphore,
    _lock_file: File,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoreHeader {
    schema_version: u32,
    hash_algorithm: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ObjectMetadata {
    schema_version: u32,
    algorithm: String,
    digest: String,
    media_type: String,
    byte_length: u64,
    created_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReferenceMarker {
    schema_version: u32,
    session_id: SessionId,
    digest: String,
    media_type: String,
    created_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnreferencedMarker {
    schema_version: u32,
    digest: String,
    byte_length: u64,
    released_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleasedSessionMarker {
    schema_version: u32,
    session_id: SessionId,
    released_at_ms: u64,
}

#[derive(Default)]
struct ReferenceIndex {
    by_digest: BTreeMap<Sha256Digest, u64>,
    by_session: BTreeMap<SessionId, u64>,
}

impl FileEvidenceStore {
    pub fn new(root: impl Into<PathBuf>, config: FileEvidenceStoreConfig) -> EvidenceResult<Self> {
        if config.max_asset_bytes == 0
            || config.max_references_per_session == 0
            || config.max_concurrent_writes == 0
            || config.max_concurrent_writes > Semaphore::MAX_PERMITS
        {
            return Err(EvidenceError::InvalidConfiguration(
                "filesystem Evidence Store limits are zero or exceed the runtime maximum"
                    .to_owned(),
            ));
        }

        let requested = root.into();
        let requested = if requested.is_absolute() {
            requested
        } else {
            std::env::current_dir()
                .map_err(|error| EvidenceError::io("resolve current directory", error))?
                .join(requested)
        };
        let requested = normalize_trusted_platform_alias(requested);
        create_root(&requested)?;
        let root = requested
            .canonicalize()
            .map_err(|error| EvidenceError::io("canonicalize evidence root", error))?;
        preflight_existing_store(&root)?;
        let version_root = real_directory(&root, "v1")?;
        let objects = real_directory(&version_root, "objects")?;
        let objects = real_directory(&objects, HASH_ALGORITHM)?;
        let references = real_directory(&version_root, "refs")?;
        let references = real_directory(&references, "sessions")?;
        let released_sessions = real_directory(&version_root, "released-sessions")?;
        let unreferenced = real_directory(&version_root, "unreferenced")?;
        let staging = real_directory(&version_root, "staging")?;
        let trash = real_directory(&version_root, "trash")?;
        let locks = real_directory(&version_root, "locks")?;

        let lock_path = locks.join("store.lock");
        reject_symlink_if_present(&lock_path, "store lock")?;
        let lock_file = open_private_file(&lock_path, false)?;
        fs2::FileExt::try_lock_exclusive(&lock_file).map_err(|error| {
            let contended = fs2::lock_contended_error();
            if error.kind() == contended.kind() || error.raw_os_error() == contended.raw_os_error()
            {
                EvidenceError::StoreBusy
            } else {
                EvidenceError::io("lock evidence store", error)
            }
        })?;

        let max_concurrent_writes = config.max_concurrent_writes;
        let store = Self {
            root,
            objects,
            references,
            released_sessions,
            unreferenced,
            staging,
            trash,
            config,
            gate: RwLock::new(()),
            object_gates: (0..MUTATION_LOCK_STRIPES).map(|_| Mutex::new(())).collect(),
            session_gates: (0..MUTATION_LOCK_STRIPES).map(|_| Mutex::new(())).collect(),
            reference_index: StdRwLock::new(None),
            write_slots: Semaphore::new(max_concurrent_writes),
            _lock_file: lock_file,
        };
        store.cleanup_atomic_temporaries(&version_root)?;
        store.initialize_header(&version_root)?;
        store.cleanup_abandoned_staging()?;
        store.recover_gc_trash()?;
        store.rebuild_reference_index()?;
        store.recover_released_sessions()?;
        store.recover_reference_state()?;
        store.rebuild_reference_index()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validates the Store-owned subset of `AssetRef` without using any
    /// caller-controlled value as a filesystem path.
    pub fn validate_asset_ref(asset: &AssetRef) -> EvidenceResult<Sha256Digest> {
        Sha256Digest::from_asset_ref(asset)
    }

    fn object_gate(&self, digest: &Sha256Digest) -> &Mutex<()> {
        let stripe = u8::from_str_radix(&digest.as_str()[..2], 16)
            .expect("validated SHA-256 digest has a hexadecimal prefix");
        &self.object_gates[usize::from(stripe)]
    }

    fn session_gate(&self, session_id: &SessionId) -> &Mutex<()> {
        &self.session_gates[usize::from(session_id.0.as_bytes()[0])]
    }

    pub async fn open_asset(&self, asset: &AssetRef) -> EvidenceResult<EvidenceOutput> {
        let digest = Self::validate_asset_ref(asset)?;
        let _gate = self.gate.read().await;
        let (metadata, file) = self.verify_object(&digest).await?;
        if metadata.media_type != asset.media_type {
            return Err(EvidenceError::InvalidReference(
                "mediaType does not match stored metadata".to_owned(),
            ));
        }
        Ok(Box::pin(file))
    }

    fn cleanup_atomic_temporaries(&self, version_root: &Path) -> EvidenceResult<()> {
        let known_root_entries = BTreeSet::from([
            "locks",
            "objects",
            "released-sessions",
            "refs",
            "staging",
            "store.json",
            "trash",
            "unreferenced",
        ]);
        let mut root_changed = false;
        for entry in read_directory_sorted(version_root, "list store root")? {
            let name = utf8_file_name(&entry.path(), "store root entry")?;
            if is_atomic_temporary_name(&name) {
                require_regular_file(&entry.path(), "atomic temporary")?;
                fs::remove_file(entry.path())
                    .map_err(|error| EvidenceError::io("remove atomic temporary", error))?;
                root_changed = true;
            } else if !known_root_entries.contains(name.as_str()) {
                return Err(EvidenceError::UnsafePath(
                    "unknown Evidence Store root entry".to_owned(),
                ));
            }
        }
        if root_changed {
            sync_directory(version_root)?;
        }

        let mut removed_empty_session = false;
        for session in read_directory_sorted(&self.references, "list session references")? {
            require_real_directory(&session.path(), "session reference directory")?;
            let name = utf8_file_name(&session.path(), "session reference directory")?;
            parse_canonical_uuid(&name, "session reference directory")?;
            cleanup_atomic_files_in(&session.path())?;
            if fs::read_dir(session.path())
                .map_err(|error| EvidenceError::io("inspect session references", error))?
                .next()
                .is_none()
            {
                fs::remove_dir(session.path())
                    .map_err(|error| EvidenceError::io("remove empty session references", error))?;
                removed_empty_session = true;
            }
        }
        if removed_empty_session {
            sync_directory(&self.references)?;
        }
        cleanup_atomic_files_in(&self.unreferenced)
            .and_then(|_| cleanup_atomic_files_in(&self.released_sessions))
    }

    fn initialize_header(&self, version_root: &Path) -> EvidenceResult<()> {
        let path = version_root.join("store.json");
        let expected = StoreHeader {
            schema_version: STORE_VERSION,
            hash_algorithm: HASH_ALGORITHM.to_owned(),
        };
        if path.exists() {
            let header: StoreHeader = read_store_json(&path, "store header")?;
            if header.schema_version != STORE_VERSION {
                return Err(EvidenceError::UnsupportedStoreVersion(
                    header.schema_version,
                ));
            }
            if header.hash_algorithm != HASH_ALGORITHM {
                return Err(EvidenceError::CorruptStore(
                    "unsupported Evidence Store hash algorithm".to_owned(),
                ));
            }
        } else {
            atomic_write_json(&path, &expected, "write store header")?;
        }
        Ok(())
    }

    fn cleanup_abandoned_staging(&self) -> EvidenceResult<()> {
        for entry in read_directory_sorted(&self.staging, "list staging")? {
            let name = utf8_file_name(&entry.path(), "staging entry")?;
            let Some(identifier) = name.strip_prefix(".part-") else {
                return Err(EvidenceError::UnsafePath(
                    "unknown staging entry".to_owned(),
                ));
            };
            parse_canonical_uuid(identifier, "staging entry")?;
            ensure_tree_has_no_symlinks(&entry.path())?;
            require_real_directory(&entry.path(), "staging entry")?;
            fs::remove_dir_all(entry.path())
                .map_err(|error| EvidenceError::io("remove abandoned staging", error))?;
        }
        sync_directory(&self.staging)
    }

    fn recover_gc_trash(&self) -> EvidenceResult<()> {
        let referenced = self.raw_referenced_digests()?;
        for entry in read_directory_sorted(&self.trash, "list GC trash")? {
            require_real_directory(&entry.path(), "GC trash entry")?;
            let digest = Sha256Digest::parse(utf8_file_name(&entry.path(), "GC trash entry")?)?;
            self.recover_gc_trash_entry(&digest, referenced.contains(&digest))?;
        }
        Ok(())
    }

    fn recover_gc_trash_entry(
        &self,
        digest: &Sha256Digest,
        referenced: bool,
    ) -> EvidenceResult<()> {
        let trash = self.trash.join(digest.as_str());
        if !trash.exists() {
            return Ok(());
        }
        require_real_directory(&trash, "GC trash entry")?;
        ensure_tree_has_no_symlinks(&trash)?;
        let normal = self.object_directory(digest);

        if normal.exists() {
            let live_metadata = verify_object_directory_sync(&normal, digest)?;
            let trash_metadata = verify_object_directory_sync(&trash, digest)?;
            if live_metadata != trash_metadata {
                return Err(EvidenceError::CorruptMetadata {
                    digest: digest.clone(),
                    reason: "live object and GC trash copy differ".to_owned(),
                });
            }
            fs::remove_dir_all(&trash)
                .map_err(|error| EvidenceError::io("remove duplicate GC trash", error))?;
            sync_directory(&self.trash)?;
            if referenced {
                self.remove_unreferenced_marker_if_present(digest)?;
            }
            return Ok(());
        }

        if referenced {
            verify_object_directory_sync(&trash, digest)?;
            let shard = self.prepare_shard(digest)?;
            fs::rename(&trash, &normal)
                .map_err(|error| EvidenceError::io("restore referenced GC object", error))?;
            // For a cross-directory rename, make the destination durable
            // before recording the removal from the source directory.
            sync_directory(&shard)?;
            sync_directory(&self.trash)?;
            self.remove_unreferenced_marker_if_present(digest)?;
        } else {
            self.remove_unreferenced_marker_if_present(digest)?;
            fs::remove_dir_all(&trash)
                .map_err(|error| EvidenceError::io("finish GC trash deletion", error))?;
            sync_directory(&self.trash)?;
        }
        Ok(())
    }

    fn remove_unreferenced_marker_if_present(&self, digest: &Sha256Digest) -> EvidenceResult<()> {
        let marker = self.unreferenced_marker_path(digest);
        if marker.exists() {
            require_regular_file(&marker, "GC marker")?;
            fs::remove_file(marker)
                .map_err(|error| EvidenceError::io("remove GC marker", error))?;
            sync_directory(&self.unreferenced)?;
        }
        Ok(())
    }

    fn recover_reference_state(&self) -> EvidenceResult<()> {
        let references = self.raw_referenced_digests()?;
        for digest in &references {
            if !self.object_directory(digest).exists() {
                return Err(EvidenceError::CorruptMetadata {
                    digest: digest.clone(),
                    reason: "session reference points to a missing object".to_owned(),
                });
            }
            self.read_object_metadata(digest)?;
            let marker = self.unreferenced_marker_path(digest);
            if marker.exists() {
                require_regular_file(&marker, "unreferenced marker")?;
                fs::remove_file(&marker)
                    .map_err(|error| EvidenceError::io("remove stale GC marker", error))?;
            }
        }

        for digest in self.object_digests()? {
            if !references.contains(&digest) {
                let metadata = self.read_object_metadata(&digest)?;
                let marker = self.unreferenced_marker_path(&digest);
                if !marker.exists() {
                    atomic_write_json(
                        &marker,
                        &UnreferencedMarker {
                            schema_version: STORE_VERSION,
                            digest: digest.to_string(),
                            byte_length: metadata.byte_length,
                            released_at_ms: now_ms(),
                        },
                        "recover orphan marker",
                    )?;
                }
            }
        }

        for entry in read_directory_sorted(&self.unreferenced, "list GC markers")? {
            require_regular_file(&entry.path(), "GC marker")?;
            let digest = digest_from_json_file_name(&entry.path())?;
            let marker: UnreferencedMarker =
                read_digest_json(&entry.path(), "read GC marker", &digest, "GC marker")?;
            validate_unreferenced_marker(&marker, &digest)?;
            if !self.object_directory(&digest).exists() {
                // No live reference can point here: that was checked above.
                // A surviving marker with no live/trash object is the final
                // recoverable state of a GC transaction whose deletion won
                // the crash race.
                fs::remove_file(entry.path()).map_err(|error| {
                    EvidenceError::io("finish missing-object GC marker deletion", error)
                })?;
            }
        }
        sync_directory(&self.unreferenced)
    }

    fn shard_directory(&self, digest: &Sha256Digest) -> PathBuf {
        let value = digest.as_str();
        self.objects.join(&value[..2]).join(&value[2..4])
    }

    fn object_directory(&self, digest: &Sha256Digest) -> PathBuf {
        self.shard_directory(digest).join(digest.as_str())
    }

    fn reference_directory(&self, session_id: &SessionId) -> PathBuf {
        self.references.join(session_id.to_string())
    }

    fn reference_path(&self, session_id: &SessionId, digest: &Sha256Digest) -> PathBuf {
        self.reference_directory(session_id)
            .join(format!("{}.json", digest.as_str()))
    }

    fn unreferenced_marker_path(&self, digest: &Sha256Digest) -> PathBuf {
        self.unreferenced.join(format!("{}.json", digest.as_str()))
    }

    fn prepare_shard(&self, digest: &Sha256Digest) -> EvidenceResult<PathBuf> {
        let first = real_directory(&self.objects, &digest.as_str()[..2])?;
        real_directory(&first, &digest.as_str()[2..4])
    }

    fn read_object_metadata(&self, digest: &Sha256Digest) -> EvidenceResult<ObjectMetadata> {
        let directory = self.object_directory(digest);
        match fs::symlink_metadata(&directory) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(EvidenceError::NotFound(digest.clone()));
            }
            Err(error) => return Err(EvidenceError::io("inspect evidence object", error)),
        }
        validate_object_directory(&directory, digest)?;
        let metadata: ObjectMetadata = read_digest_json(
            &directory.join("meta.json"),
            "read object metadata",
            digest,
            "object metadata",
        )?;
        validate_object_metadata(&metadata, digest)?;
        Ok(metadata)
    }

    async fn verify_object(
        &self,
        digest: &Sha256Digest,
    ) -> EvidenceResult<(ObjectMetadata, tokio::fs::File)> {
        let metadata = self.read_object_metadata(digest)?;
        let data_path = self.object_directory(digest).join("data");
        require_regular_file(&data_path, "evidence data")?;
        let std_file = File::open(&data_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                EvidenceError::NotFound(digest.clone())
            } else {
                EvidenceError::io("open evidence data", error)
            }
        })?;
        let actual_size = std_file
            .metadata()
            .map_err(|error| EvidenceError::io("inspect evidence data", error))?
            .len();
        if actual_size != metadata.byte_length {
            return Err(EvidenceError::CorruptMetadata {
                digest: digest.clone(),
                reason: "stored byte length does not match data".to_owned(),
            });
        }

        let mut file = tokio::fs::File::from_std(std_file);
        let actual = hash_reader(&mut file).await?;
        if &actual != digest {
            return Err(EvidenceError::Corrupt {
                expected: digest.clone(),
                actual,
            });
        }
        file.seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|error| EvidenceError::io("rewind verified evidence", error))?;
        Ok((metadata, file))
    }

    fn released_session_marker_path(&self, session_id: &SessionId) -> PathBuf {
        self.released_sessions.join(format!("{}.json", session_id))
    }

    fn ensure_session_open(&self, session_id: &SessionId) -> EvidenceResult<()> {
        let marker_path = self.released_session_marker_path(session_id);
        if !marker_path.exists() {
            return Ok(());
        }
        let marker: ReleasedSessionMarker =
            read_store_json(&marker_path, "read released Session marker")?;
        validate_released_session_marker(&marker, session_id)?;
        Err(EvidenceError::SessionClosed(session_id.clone()))
    }

    fn persist_session_release(
        &self,
        session_id: &SessionId,
        released_at_ms: u64,
    ) -> EvidenceResult<u64> {
        let path = self.released_session_marker_path(session_id);
        if path.exists() {
            let marker: ReleasedSessionMarker =
                read_store_json(&path, "read released Session marker")?;
            validate_released_session_marker(&marker, session_id)?;
            return Ok(marker.released_at_ms);
        }
        atomic_write_json(
            &path,
            &ReleasedSessionMarker {
                schema_version: STORE_VERSION,
                session_id: session_id.clone(),
                released_at_ms,
            },
            "write released Session marker",
        )?;
        Ok(released_at_ms)
    }

    fn recover_released_sessions(&self) -> EvidenceResult<()> {
        for entry in read_directory_sorted(&self.released_sessions, "list released Sessions")? {
            require_regular_file(&entry.path(), "released Session marker")?;
            let name = utf8_file_name(&entry.path(), "released Session marker")?;
            let session_name = name.strip_suffix(".json").ok_or_else(|| {
                EvidenceError::UnsafePath("released Session marker must end in .json".to_owned())
            })?;
            let session_id = SessionId::from(parse_canonical_uuid(
                session_name,
                "released Session marker",
            )?);
            let marker: ReleasedSessionMarker =
                read_store_json(&entry.path(), "read released Session marker")?;
            validate_released_session_marker(&marker, &session_id)?;
            if self.reference_directory(&session_id).exists() {
                self.release_references_locked(&session_id, marker.released_at_ms)?;
            }
        }
        Ok(())
    }

    fn release_references_locked(
        &self,
        session_id: &SessionId,
        released_at_ms: u64,
    ) -> EvidenceResult<ReleaseReport> {
        let directory = self.reference_directory(session_id);
        if !directory.exists() {
            // This can be an idempotent retry after a failure that happened
            // after removing the Session directory but before publishing every
            // unreferenced marker. The slow full recovery is reserved for this
            // exceptional path; ordinary releases use the in-memory index.
            self.recover_reference_state()?;
            self.rebuild_reference_index()?;
            return Ok(ReleaseReport {
                session_id: session_id.clone(),
                released_references: 0,
                newly_unreferenced_assets: 0,
                newly_unreferenced_bytes: 0,
            });
        }
        require_real_directory(&directory, "session references")?;
        let mut released = BTreeMap::<Sha256Digest, ObjectMetadata>::new();
        for entry in read_directory_sorted(&directory, "list session references")? {
            require_regular_file(&entry.path(), "session reference")?;
            let digest = digest_from_json_file_name(&entry.path())?;
            let marker: ReferenceMarker = read_digest_json(
                &entry.path(),
                "read session reference",
                &digest,
                "session reference",
            )?;
            validate_reference_marker(&marker, session_id, &digest, &marker.media_type)?;
            let object = self.read_object_metadata(&digest)?;
            if marker.media_type != object.media_type {
                return Err(EvidenceError::CorruptMetadata {
                    digest,
                    reason: "session reference media type differs from object".to_owned(),
                });
            }
            released.insert(digest, object);
        }
        fs::remove_dir_all(&directory)
            .map_err(|error| EvidenceError::io("remove session references", error))?;
        self.index_session_removed(session_id, released.keys());
        sync_directory(&self.references)?;

        let mut newly_unreferenced_assets = 0_u64;
        let mut newly_unreferenced_bytes = 0_u64;
        for (digest, metadata) in &released {
            if self.indexed_reference_count(digest)? == 0 {
                let created =
                    self.ensure_unreferenced_marker(digest, metadata.byte_length, released_at_ms)?;
                if created {
                    newly_unreferenced_assets = newly_unreferenced_assets.saturating_add(1);
                    newly_unreferenced_bytes =
                        newly_unreferenced_bytes.saturating_add(metadata.byte_length);
                }
            }
        }
        Ok(ReleaseReport {
            session_id: session_id.clone(),
            released_references: released.len() as u64,
            newly_unreferenced_assets,
            newly_unreferenced_bytes,
        })
    }

    fn add_reference(
        &self,
        session_id: &SessionId,
        digest: &Sha256Digest,
        media_type: &str,
        created_at_ms: u64,
    ) -> EvidenceResult<bool> {
        self.ensure_session_open(session_id)?;
        let directory = self.reference_directory(session_id);
        let path = self.reference_path(session_id, digest);
        if path.exists() {
            let marker: ReferenceMarker =
                read_digest_json(&path, "read session reference", digest, "session reference")?;
            validate_reference_marker(&marker, session_id, digest, media_type)?;
            return Ok(false);
        }

        let existing = if directory.exists() {
            self.indexed_session_reference_count(session_id)?
        } else {
            0
        };
        if existing >= self.config.max_references_per_session {
            return Err(EvidenceError::ReferenceLimit {
                maximum: self.config.max_references_per_session,
            });
        }
        let unreferenced = self.unreferenced_marker_path(digest);
        if unreferenced.exists() {
            require_regular_file(&unreferenced, "GC marker")?;
            fs::remove_file(&unreferenced)
                .map_err(|error| EvidenceError::io("remove GC marker", error))?;
            sync_directory(&self.unreferenced)?;
        }
        if !directory.exists() {
            real_directory(&self.references, &session_id.to_string())?;
        } else {
            require_real_directory(&directory, "session references")?;
        }
        atomic_write_json(
            &path,
            &ReferenceMarker {
                schema_version: STORE_VERSION,
                session_id: session_id.clone(),
                digest: digest.to_string(),
                media_type: media_type.to_owned(),
                created_at_ms,
            },
            "write session reference",
        )?;
        self.index_reference_added(session_id, digest);
        Ok(true)
    }

    fn mark_unreferenced_if_unowned(
        &self,
        digest: &Sha256Digest,
        byte_length: u64,
        released_at_ms: u64,
    ) -> EvidenceResult<()> {
        if self.indexed_reference_count(digest)? != 0 {
            return Ok(());
        }
        self.ensure_unreferenced_marker(digest, byte_length, released_at_ms)?;
        Ok(())
    }

    fn ensure_unreferenced_marker(
        &self,
        digest: &Sha256Digest,
        byte_length: u64,
        released_at_ms: u64,
    ) -> EvidenceResult<bool> {
        let marker_path = self.unreferenced_marker_path(digest);
        if marker_path.exists() {
            let marker: UnreferencedMarker =
                read_digest_json(&marker_path, "read GC marker", digest, "GC marker")?;
            validate_unreferenced_marker(&marker, digest)?;
            if marker.byte_length != byte_length {
                return Err(EvidenceError::CorruptMetadata {
                    digest: digest.clone(),
                    reason: "GC marker byte length differs".to_owned(),
                });
            }
            return Ok(false);
        }
        atomic_write_json(
            &marker_path,
            &UnreferencedMarker {
                schema_version: STORE_VERSION,
                digest: digest.to_string(),
                byte_length,
                released_at_ms,
            },
            "write orphan recovery marker",
        )?;
        Ok(true)
    }

    fn session_reference_count(&self, session_id: &SessionId) -> EvidenceResult<u64> {
        let directory = self.reference_directory(session_id);
        if !directory.exists() {
            return Ok(0);
        }
        require_real_directory(&directory, "session references")?;
        let mut count = 0_u64;
        for entry in read_directory_sorted(&directory, "list session references")? {
            require_regular_file(&entry.path(), "session reference")?;
            let digest = digest_from_json_file_name(&entry.path())?;
            let marker: ReferenceMarker = read_digest_json(
                &entry.path(),
                "read session reference",
                &digest,
                "session reference",
            )?;
            validate_reference_marker(&marker, session_id, &digest, &marker.media_type)?;
            let object = self.read_object_metadata(&digest)?;
            if marker.media_type != object.media_type {
                return Err(EvidenceError::CorruptMetadata {
                    digest,
                    reason: "session reference media type differs from object".to_owned(),
                });
            }
            count = count.saturating_add(1);
        }
        Ok(count)
    }

    fn scan_reference_index(&self) -> EvidenceResult<ReferenceIndex> {
        let mut index = ReferenceIndex::default();
        for session_entry in read_directory_sorted(&self.references, "list session references")? {
            require_real_directory(&session_entry.path(), "session reference directory")?;
            let session_name = utf8_file_name(&session_entry.path(), "session directory")?;
            let session_id = SessionId::from(parse_canonical_uuid(
                &session_name,
                "session reference directory",
            )?);
            let mut session_count = 0_u64;
            for entry in read_directory_sorted(&session_entry.path(), "list session references")? {
                require_regular_file(&entry.path(), "session reference")?;
                let digest = digest_from_json_file_name(&entry.path())?;
                let marker: ReferenceMarker = read_digest_json(
                    &entry.path(),
                    "read session reference",
                    &digest,
                    "session reference",
                )?;
                validate_reference_marker(&marker, &session_id, &digest, &marker.media_type)?;
                let object = self.read_object_metadata(&digest)?;
                if marker.media_type != object.media_type {
                    return Err(EvidenceError::CorruptMetadata {
                        digest,
                        reason: "session reference media type differs from object".to_owned(),
                    });
                }
                let count = index.by_digest.entry(digest).or_default();
                *count = count.saturating_add(1);
                session_count = session_count.saturating_add(1);
            }
            if session_count > 0 {
                index.by_session.insert(session_id, session_count);
            }
        }
        Ok(index)
    }

    fn rebuild_reference_index(&self) -> EvidenceResult<()> {
        let rebuilt = self.scan_reference_index()?;
        *self
            .reference_index
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rebuilt);
        Ok(())
    }

    fn indexed_reference_count(&self, digest: &Sha256Digest) -> EvidenceResult<u64> {
        let index = self
            .reference_index
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match index.as_ref() {
            Some(index) => Ok(index.by_digest.get(digest).copied().unwrap_or(0)),
            None => {
                drop(index);
                self.count_references(digest)
            }
        }
    }

    fn indexed_session_reference_count(&self, session_id: &SessionId) -> EvidenceResult<u64> {
        let index = self
            .reference_index
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match index.as_ref() {
            Some(index) => Ok(index.by_session.get(session_id).copied().unwrap_or(0)),
            None => {
                drop(index);
                self.session_reference_count(session_id)
            }
        }
    }

    fn indexed_referenced_digests(&self) -> EvidenceResult<BTreeSet<Sha256Digest>> {
        let index = self
            .reference_index
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match index.as_ref() {
            Some(index) => Ok(index.by_digest.keys().cloned().collect()),
            None => {
                drop(index);
                self.referenced_digests()
            }
        }
    }

    fn index_reference_added(&self, session_id: &SessionId, digest: &Sha256Digest) {
        let mut index = self
            .reference_index
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = index.as_mut() {
            let digest_count = index.by_digest.entry(digest.clone()).or_default();
            *digest_count = digest_count.saturating_add(1);
            let session_count = index.by_session.entry(session_id.clone()).or_default();
            *session_count = session_count.saturating_add(1);
        }
    }

    fn index_session_removed<'a>(
        &self,
        session_id: &SessionId,
        digests: impl Iterator<Item = &'a Sha256Digest>,
    ) {
        let mut index = self
            .reference_index
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(index) = index.as_mut() else {
            return;
        };
        index.by_session.remove(session_id);
        for digest in digests {
            let remove = match index.by_digest.get_mut(digest) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => true,
                None => false,
            };
            if remove {
                index.by_digest.remove(digest);
            }
        }
    }

    fn referenced_digests(&self) -> EvidenceResult<BTreeSet<Sha256Digest>> {
        let mut digests = BTreeSet::new();
        for session_entry in read_directory_sorted(&self.references, "list session references")? {
            require_real_directory(&session_entry.path(), "session reference directory")?;
            let session_name = utf8_file_name(&session_entry.path(), "session directory")?;
            let session_uuid = parse_canonical_uuid(&session_name, "session reference directory")?;
            let session_id = SessionId::from(session_uuid);
            for entry in read_directory_sorted(&session_entry.path(), "list session references")? {
                require_regular_file(&entry.path(), "session reference")?;
                let digest = digest_from_json_file_name(&entry.path())?;
                let marker: ReferenceMarker = read_digest_json(
                    &entry.path(),
                    "read session reference",
                    &digest,
                    "session reference",
                )?;
                validate_reference_marker(&marker, &session_id, &digest, &marker.media_type)?;
                let object = self.read_object_metadata(&digest)?;
                if marker.media_type != object.media_type {
                    return Err(EvidenceError::CorruptMetadata {
                        digest,
                        reason: "session reference media type differs from object".to_owned(),
                    });
                }
                digests.insert(digest);
            }
        }
        Ok(digests)
    }

    fn raw_referenced_digests(&self) -> EvidenceResult<BTreeSet<Sha256Digest>> {
        let mut digests = BTreeSet::new();
        for session_entry in read_directory_sorted(&self.references, "list session references")? {
            require_real_directory(&session_entry.path(), "session reference directory")?;
            let session_name = utf8_file_name(&session_entry.path(), "session directory")?;
            let session_uuid = parse_canonical_uuid(&session_name, "session reference directory")?;
            let session_id = SessionId::from(session_uuid);
            for entry in read_directory_sorted(&session_entry.path(), "list session references")? {
                require_regular_file(&entry.path(), "session reference")?;
                let digest = digest_from_json_file_name(&entry.path())?;
                let marker: ReferenceMarker = read_digest_json(
                    &entry.path(),
                    "read session reference",
                    &digest,
                    "session reference",
                )?;
                validate_reference_marker(&marker, &session_id, &digest, &marker.media_type)?;
                digests.insert(digest);
            }
        }
        Ok(digests)
    }

    fn referenced_session_ids(&self) -> EvidenceResult<Vec<SessionId>> {
        let mut sessions = Vec::new();
        for entry in read_directory_sorted(&self.references, "list session references")? {
            require_real_directory(&entry.path(), "session reference directory")?;
            let name = utf8_file_name(&entry.path(), "session directory")?;
            let session_id =
                SessionId::from(parse_canonical_uuid(&name, "session reference directory")?);
            if self.session_reference_count(&session_id)? > 0 {
                sessions.push(session_id);
            }
        }
        sessions.sort();
        Ok(sessions)
    }

    fn count_references(&self, target: &Sha256Digest) -> EvidenceResult<u64> {
        let mut count = 0_u64;
        for session_entry in read_directory_sorted(&self.references, "list session references")? {
            require_real_directory(&session_entry.path(), "session reference directory")?;
            let session_name = utf8_file_name(&session_entry.path(), "session directory")?;
            let session_uuid = parse_canonical_uuid(&session_name, "session reference directory")?;
            let session_id = SessionId::from(session_uuid);
            for entry in read_directory_sorted(&session_entry.path(), "list session references")? {
                require_regular_file(&entry.path(), "session reference")?;
                let digest = digest_from_json_file_name(&entry.path())?;
                let marker: ReferenceMarker = read_digest_json(
                    &entry.path(),
                    "read session reference",
                    &digest,
                    "session reference",
                )?;
                validate_reference_marker(&marker, &session_id, &digest, &marker.media_type)?;
                let object = self.read_object_metadata(&digest)?;
                if marker.media_type != object.media_type {
                    return Err(EvidenceError::CorruptMetadata {
                        digest,
                        reason: "session reference media type differs from object".to_owned(),
                    });
                }
                if &digest == target {
                    count = count.saturating_add(1);
                }
            }
        }
        Ok(count)
    }

    fn object_digests(&self) -> EvidenceResult<BTreeSet<Sha256Digest>> {
        let mut output = BTreeSet::new();
        for first in read_directory_sorted(&self.objects, "list object shards")? {
            require_real_directory(&first.path(), "object shard")?;
            let first_name = utf8_file_name(&first.path(), "object shard")?;
            if !is_lower_hex(&first_name, 2) {
                return Err(EvidenceError::UnsafePath("invalid object shard".to_owned()));
            }
            for second in read_directory_sorted(&first.path(), "list object shards")? {
                require_real_directory(&second.path(), "object shard")?;
                let second_name = utf8_file_name(&second.path(), "object shard")?;
                if !is_lower_hex(&second_name, 2) {
                    return Err(EvidenceError::UnsafePath("invalid object shard".to_owned()));
                }
                for object in read_directory_sorted(&second.path(), "list objects")? {
                    require_real_directory(&object.path(), "object directory")?;
                    let digest =
                        Sha256Digest::parse(utf8_file_name(&object.path(), "object directory")?)?;
                    if digest.as_str()[..2] != first_name || digest.as_str()[2..4] != second_name {
                        return Err(EvidenceError::UnsafePath(
                            "object is stored under the wrong shard".to_owned(),
                        ));
                    }
                    validate_object_directory(&object.path(), &digest)?;
                    output.insert(digest);
                }
            }
        }
        Ok(output)
    }
}

#[async_trait]
impl EvidenceStore for FileEvidenceStore {
    async fn put(
        &self,
        request: PutEvidence,
        mut input: EvidenceInput,
    ) -> EvidenceResult<StoredEvidence> {
        let _write_slot =
            self.write_slots.acquire().await.map_err(|_| {
                EvidenceError::Internal("evidence write limiter is closed".to_owned())
            })?;
        let (session_id, media_type, expected_digest, declared_size) = request.into_parts();
        {
            let _gate = self.gate.read().await;
            self.ensure_session_open(&session_id)?;
        }
        if let Some(declared) = declared_size
            && declared > self.config.max_asset_bytes
        {
            return Err(EvidenceError::TooLarge {
                actual: declared,
                maximum: self.config.max_asset_bytes,
            });
        }

        let staging_path = self.staging.join(format!(".part-{}", Uuid::new_v4()));
        create_private_directory(&staging_path)?;
        let mut staging_guard = StagingGuard::new(staging_path.clone());
        let data_path = staging_path.join("data");
        let std_file = open_private_file(&data_path, true)?;
        let mut file = tokio::fs::File::from_std(std_file);
        let mut hasher = Sha256::new();
        let mut actual_size = 0_u64;
        let mut buffer = vec![0_u8; BUFFER_SIZE];
        loop {
            let read = input
                .as_mut()
                .read(&mut buffer)
                .await
                .map_err(|error| EvidenceError::io("read evidence input", error))?;
            if read == 0 {
                break;
            }
            actual_size = actual_size
                .checked_add(read as u64)
                .ok_or(EvidenceError::TooLarge {
                    actual: u64::MAX,
                    maximum: self.config.max_asset_bytes,
                })?;
            if actual_size > self.config.max_asset_bytes {
                return Err(EvidenceError::TooLarge {
                    actual: actual_size,
                    maximum: self.config.max_asset_bytes,
                });
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .await
                .map_err(|error| EvidenceError::io("write staged evidence", error))?;
        }
        if actual_size == 0 {
            return Err(EvidenceError::EmptyContent);
        }
        if let Some(declared) = declared_size
            && declared != actual_size
        {
            return Err(EvidenceError::DeclaredSizeMismatch {
                declared,
                actual: actual_size,
            });
        }
        file.flush()
            .await
            .map_err(|error| EvidenceError::io("flush staged evidence", error))?;
        file.sync_all()
            .await
            .map_err(|error| EvidenceError::io("sync staged evidence", error))?;
        drop(file);

        let digest = digest_from_hash(hasher.finalize())?;
        if let Some(expected) = expected_digest
            && expected != digest
        {
            return Err(EvidenceError::DigestMismatch {
                expected,
                actual: digest,
            });
        }
        let created_at_ms = now_ms();
        let object_metadata = ObjectMetadata {
            schema_version: STORE_VERSION,
            algorithm: HASH_ALGORITHM.to_owned(),
            digest: digest.to_string(),
            media_type: media_type.clone(),
            byte_length: actual_size,
            created_at_ms,
        };
        write_synced_json_file(
            &staging_path.join("meta.json"),
            &object_metadata,
            "write staged object metadata",
        )?;
        sync_directory(&staging_path)?;

        let final_directory = self.object_directory(&digest);
        // Same-object publication and same-Session reference limits need local
        // serialization. The shared Store gate excludes GC/release without
        // serializing unrelated objects or Sessions behind large-object hash
        // and fsync latency.
        let _object_gate = self.object_gate(&digest).lock().await;
        let _session_gate = self.session_gate(&session_id).lock().await;
        let _gate = self.gate.read().await;
        let referenced = self.indexed_reference_count(&digest)? > 0;
        self.recover_gc_trash_entry(&digest, referenced)?;
        let shard = self.prepare_shard(&digest)?;
        let (metadata, deduplicated) = if final_directory.exists() {
            let (existing, _) = self.verify_object(&digest).await?;
            if existing.media_type != media_type {
                return Err(EvidenceError::MediaTypeConflict {
                    digest,
                    existing: existing.media_type,
                    requested: media_type,
                });
            }
            if existing.byte_length != actual_size {
                return Err(EvidenceError::CorruptMetadata {
                    digest,
                    reason: "deduplicated object size differs".to_owned(),
                });
            }
            (existing, true)
        } else {
            fs::rename(&staging_path, &final_directory)
                .map_err(|error| EvidenceError::io("publish evidence object", error))?;
            staging_guard.published = true;
            sync_directory(&shard)?;
            sync_directory(&self.staging)?;
            (object_metadata, false)
        };
        drop(staging_guard);

        let reference_count_before = match self.indexed_reference_count(&digest) {
            Ok(count) => count,
            Err(error) => {
                let _ =
                    self.mark_unreferenced_if_unowned(&digest, metadata.byte_length, created_at_ms);
                return Err(error);
            }
        };
        let added =
            match self.add_reference(&session_id, &digest, &metadata.media_type, created_at_ms) {
                Ok(added) => added,
                Err(error) => {
                    let _ = self.mark_unreferenced_if_unowned(
                        &digest,
                        metadata.byte_length,
                        created_at_ms,
                    );
                    return Err(error);
                }
            };
        let reference_count = reference_count_before.saturating_add(u64::from(added));
        let metadata = EvidenceMetadata::new(
            digest,
            metadata.media_type,
            metadata.byte_length,
            metadata.created_at_ms,
            reference_count,
        )?;
        Ok(StoredEvidence::new(metadata, deduplicated))
    }

    async fn attach(
        &self,
        session_id: &SessionId,
        asset: &AssetRef,
    ) -> EvidenceResult<StoredEvidence> {
        let digest = Self::validate_asset_ref(asset)?;
        let _object_gate = self.object_gate(&digest).lock().await;
        let _session_gate = self.session_gate(session_id).lock().await;
        let _gate = self.gate.read().await;
        self.ensure_session_open(session_id)?;
        let metadata = self.verify_object(&digest).await?.0;
        if metadata.media_type != asset.media_type {
            return Err(EvidenceError::InvalidReference(
                "mediaType does not match stored metadata".to_owned(),
            ));
        }
        let reference_count_before = self.indexed_reference_count(&digest)?;
        let added = self.add_reference(session_id, &digest, &metadata.media_type, now_ms())?;
        let reference_count = reference_count_before.saturating_add(u64::from(added));
        let metadata = EvidenceMetadata::new(
            digest,
            metadata.media_type,
            metadata.byte_length,
            metadata.created_at_ms,
            reference_count,
        )?;
        Ok(StoredEvidence::new(metadata, true))
    }

    async fn verify_session_reference(
        &self,
        session_id: &SessionId,
        asset: &AssetRef,
    ) -> EvidenceResult<EvidenceMetadata> {
        let digest = Self::validate_asset_ref(asset)?;
        let _gate = self.gate.read().await;
        self.ensure_session_open(session_id)?;

        // Check the Session-owned marker before consulting the global object
        // namespace. This both proves ownership without mutating it and avoids
        // exposing whether an unattached digest happens to exist elsewhere.
        let path = self.reference_path(session_id, &digest);
        match fs::symlink_metadata(&path) {
            Ok(_) => require_regular_file(&path, "session reference")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(EvidenceError::NotAttached {
                    session_id: session_id.clone(),
                    digest,
                });
            }
            Err(error) => {
                return Err(EvidenceError::io("inspect session reference", error));
            }
        }
        let marker: ReferenceMarker = read_digest_json(
            &path,
            "read session reference",
            &digest,
            "session reference",
        )?;
        validate_reference_marker(&marker, session_id, &digest, &marker.media_type)?;
        if marker.media_type != asset.media_type {
            return Err(EvidenceError::InvalidReference(
                "mediaType does not match session reference".to_owned(),
            ));
        }

        let (metadata, _) = self.verify_object(&digest).await?;
        if metadata.media_type != marker.media_type {
            return Err(EvidenceError::CorruptMetadata {
                digest,
                reason: "session reference media type differs from object".to_owned(),
            });
        }
        let reference_count = self.indexed_reference_count(&digest)?;
        EvidenceMetadata::new(
            digest,
            metadata.media_type,
            metadata.byte_length,
            metadata.created_at_ms,
            reference_count,
        )
    }

    async fn open(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
        let _gate = self.gate.read().await;
        let (_, file) = self.verify_object(digest).await?;
        Ok(Box::pin(file))
    }

    async fn metadata(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
        // This is the explicit full reference-audit surface. Exclude reference
        // publication so directory scans cannot observe atomic temporaries.
        let _gate = self.gate.write().await;
        let (metadata, _) = self.verify_object(digest).await?;
        // Metadata is also the explicit online integrity-audit surface. Keep
        // validating all reference markers here so an out-of-band mutation is
        // reported immediately; write/release hot paths use the trusted index
        // maintained under the process-exclusive Store lock.
        let reference_count = self.count_references(digest)?;
        EvidenceMetadata::new(
            digest.clone(),
            metadata.media_type,
            metadata.byte_length,
            metadata.created_at_ms,
            reference_count,
        )
    }

    async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
        let _gate = self.gate.write().await;
        self.referenced_session_ids()
    }

    async fn release_session(
        &self,
        session_id: &SessionId,
        released_at_ms: u64,
    ) -> EvidenceResult<ReleaseReport> {
        let _gate = self.gate.write().await;
        let released_at_ms = self.persist_session_release(session_id, released_at_ms)?;
        self.release_references_locked(session_id, released_at_ms)
    }

    async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
        let _gate = self.gate.write().await;
        self.recover_gc_trash()?;
        self.recover_reference_state()?;
        self.rebuild_reference_index()?;
        let referenced = self.indexed_referenced_digests()?;
        let mut markers = BTreeMap::<Sha256Digest, UnreferencedMarker>::new();
        for entry in read_directory_sorted(&self.unreferenced, "list GC markers")? {
            require_regular_file(&entry.path(), "GC marker")?;
            let digest = digest_from_json_file_name(&entry.path())?;
            let marker: UnreferencedMarker =
                read_digest_json(&entry.path(), "read GC marker", &digest, "GC marker")?;
            validate_unreferenced_marker(&marker, &digest)?;
            if referenced.contains(&digest) {
                return Err(EvidenceError::Internal(
                    "GC marker exists for a referenced object".to_owned(),
                ));
            }
            markers.insert(digest, marker);
        }

        let mut report = GcReport {
            examined_assets: markers.len() as u64,
            dry_run: policy.dry_run,
            ..GcReport::default()
        };
        for (digest, marker) in markers {
            if marker.released_at_ms > policy.unreferenced_before_ms {
                continue;
            }
            let (metadata, _) = self.verify_object(&digest).await?;
            if metadata.byte_length != marker.byte_length {
                return Err(EvidenceError::CorruptMetadata {
                    digest,
                    reason: "GC marker byte length differs".to_owned(),
                });
            }
            if policy
                .max_assets
                .is_some_and(|maximum| report.candidate_assets >= maximum)
            {
                break;
            }
            if policy.max_bytes.is_some_and(|maximum| {
                report.candidate_bytes.saturating_add(metadata.byte_length) > maximum
            }) {
                continue;
            }
            report.candidate_assets = report.candidate_assets.saturating_add(1);
            report.candidate_bytes = report.candidate_bytes.saturating_add(metadata.byte_length);
            if !policy.dry_run {
                let object = self.object_directory(&digest);
                validate_object_directory(&object, &digest)?;
                let trash = self.trash.join(digest.as_str());
                if trash.exists() {
                    return Err(EvidenceError::CorruptMetadata {
                        digest,
                        reason: "GC trash target already exists".to_owned(),
                    });
                }
                fs::rename(&object, &trash).map_err(|error| {
                    EvidenceError::io("move evidence object to GC trash", error)
                })?;
                // Persist the destination before the source deletion. If the
                // process stops between these fsyncs, recovery sees at least
                // one complete copy instead of losing both directory entries.
                sync_directory(&self.trash)?;
                sync_directory(&self.shard_directory(&digest))?;
                let marker_path = self.unreferenced_marker_path(&digest);
                require_regular_file(&marker_path, "GC marker")?;
                fs::remove_file(marker_path)
                    .map_err(|error| EvidenceError::io("delete GC marker", error))?;
                sync_directory(&self.unreferenced)?;
                ensure_tree_has_no_symlinks(&trash)?;
                fs::remove_dir_all(&trash)
                    .map_err(|error| EvidenceError::io("delete GC trash", error))?;
                sync_directory(&self.trash)?;
                report.deleted_assets = report.deleted_assets.saturating_add(1);
                report.deleted_bytes = report.deleted_bytes.saturating_add(metadata.byte_length);
            }
        }
        Ok(report)
    }
}

struct StagingGuard {
    path: PathBuf,
    published: bool,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

async fn hash_reader(file: &mut tokio::fs::File) -> EvidenceResult<Sha256Digest> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; BUFFER_SIZE];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| EvidenceError::io("read evidence data", error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    digest_from_hash(hasher.finalize())
}

fn digest_from_hash(hash: impl AsRef<[u8]>) -> EvidenceResult<Sha256Digest> {
    let mut value = String::with_capacity(64);
    for byte in hash.as_ref() {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}")
            .map_err(|error| EvidenceError::Internal(error.to_string()))?;
    }
    Sha256Digest::parse(value)
}

fn validate_object_metadata(
    metadata: &ObjectMetadata,
    digest: &Sha256Digest,
) -> EvidenceResult<()> {
    if metadata.schema_version != STORE_VERSION {
        return Err(EvidenceError::UnsupportedStoreVersion(
            metadata.schema_version,
        ));
    }
    if metadata.algorithm != HASH_ALGORITHM || metadata.digest != digest.as_str() {
        return Err(EvidenceError::CorruptMetadata {
            digest: digest.clone(),
            reason: "algorithm or digest mismatch".to_owned(),
        });
    }
    if metadata.byte_length == 0 {
        return Err(EvidenceError::CorruptMetadata {
            digest: digest.clone(),
            reason: "stored object has zero length".to_owned(),
        });
    }
    EvidenceMetadata::new(
        digest.clone(),
        metadata.media_type.clone(),
        metadata.byte_length,
        metadata.created_at_ms,
        0,
    )
    .map_err(|_| EvidenceError::CorruptMetadata {
        digest: digest.clone(),
        reason: "stored object metadata is invalid".to_owned(),
    })?;
    Ok(())
}

fn validate_reference_marker(
    marker: &ReferenceMarker,
    session_id: &SessionId,
    digest: &Sha256Digest,
    media_type: &str,
) -> EvidenceResult<()> {
    if marker.schema_version != STORE_VERSION {
        return Err(EvidenceError::UnsupportedStoreVersion(
            marker.schema_version,
        ));
    }
    if &marker.session_id != session_id
        || marker.digest != digest.as_str()
        || marker.media_type != media_type
    {
        return Err(EvidenceError::CorruptMetadata {
            digest: digest.clone(),
            reason: "session reference fields do not match its path".to_owned(),
        });
    }
    Ok(())
}

fn validate_unreferenced_marker(
    marker: &UnreferencedMarker,
    digest: &Sha256Digest,
) -> EvidenceResult<()> {
    if marker.schema_version != STORE_VERSION {
        return Err(EvidenceError::UnsupportedStoreVersion(
            marker.schema_version,
        ));
    }
    if marker.digest != digest.as_str() {
        return Err(EvidenceError::CorruptMetadata {
            digest: digest.clone(),
            reason: "GC marker digest does not match its path".to_owned(),
        });
    }
    Ok(())
}

fn validate_released_session_marker(
    marker: &ReleasedSessionMarker,
    session_id: &SessionId,
) -> EvidenceResult<()> {
    if marker.schema_version != STORE_VERSION {
        return Err(EvidenceError::UnsupportedStoreVersion(
            marker.schema_version,
        ));
    }
    if &marker.session_id != session_id {
        return Err(EvidenceError::UnsafePath(
            "released Session marker does not match its path".to_owned(),
        ));
    }
    Ok(())
}

fn validate_object_directory(path: &Path, digest: &Sha256Digest) -> EvidenceResult<()> {
    require_real_directory(path, "object directory")?;
    let entries = read_directory_sorted(path, "list object directory")?;
    let names = entries
        .iter()
        .map(|entry| utf8_file_name(&entry.path(), "object entry"))
        .collect::<EvidenceResult<BTreeSet<_>>>()?;
    if names != BTreeSet::from(["data".to_owned(), "meta.json".to_owned()]) {
        return Err(EvidenceError::CorruptMetadata {
            digest: digest.clone(),
            reason: "object directory has unknown or missing entries".to_owned(),
        });
    }
    require_regular_file(&path.join("data"), "evidence data")?;
    require_regular_file(&path.join("meta.json"), "object metadata")
}

fn verify_object_directory_sync(
    path: &Path,
    digest: &Sha256Digest,
) -> EvidenceResult<ObjectMetadata> {
    validate_object_directory(path, digest)?;
    let metadata: ObjectMetadata = read_digest_json(
        &path.join("meta.json"),
        "read object metadata during recovery",
        digest,
        "object metadata",
    )?;
    validate_object_metadata(&metadata, digest)?;

    let data_path = path.join("data");
    let mut file = File::open(&data_path)
        .map_err(|error| EvidenceError::io("open object data during recovery", error))?;
    let actual_size = file
        .metadata()
        .map_err(|error| EvidenceError::io("inspect object data during recovery", error))?
        .len();
    if actual_size != metadata.byte_length {
        return Err(EvidenceError::CorruptMetadata {
            digest: digest.clone(),
            reason: "recovery object byte length differs from metadata".to_owned(),
        });
    }

    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; BUFFER_SIZE];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| EvidenceError::io("hash object data during recovery", error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = digest_from_hash(hasher.finalize())?;
    if &actual != digest {
        return Err(EvidenceError::Corrupt {
            expected: digest.clone(),
            actual,
        });
    }
    Ok(metadata)
}

fn create_root(path: &Path) -> EvidenceResult<()> {
    let mut missing = Vec::new();
    let mut cursor = Some(path);
    while let Some(current) = cursor {
        match fs::symlink_metadata(current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(EvidenceError::UnsafePath(
                    "store root and its ancestors must be real directories".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
            }
            Err(error) => return Err(EvidenceError::io("inspect evidence root", error)),
        }
        cursor = current.parent();
    }

    for directory in missing.into_iter().rev() {
        let parent = directory.parent().ok_or_else(|| {
            EvidenceError::UnsafePath("evidence root directory has no parent".to_owned())
        })?;
        require_real_directory(parent, "evidence root ancestor")?;
        fs::create_dir(&directory)
            .map_err(|error| EvidenceError::io("create evidence root", error))?;
        set_private_directory_permissions(&directory)?;
        sync_directory(parent)?;
    }
    Ok(())
}

/// macOS exposes several root-owned compatibility aliases (`/var`, `/tmp`,
/// `/etc`) as symlinks into `/private`. Resolve only those fixed aliases before
/// enforcing the no-symlink rule on the caller-controlled remainder. This
/// keeps ordinary `TMPDIR` paths usable without accepting arbitrary ancestor
/// symlinks.
fn normalize_trusted_platform_alias(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        for (alias, target) in [
            (Path::new("/var"), Path::new("/private/var")),
            (Path::new("/tmp"), Path::new("/private/tmp")),
            (Path::new("/etc"), Path::new("/private/etc")),
        ] {
            let Ok(remainder) = path.strip_prefix(alias) else {
                continue;
            };
            let trusted = fs::symlink_metadata(alias)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
                && matches!(
                    (alias.canonicalize(), target.canonicalize()),
                    (Ok(resolved_alias), Ok(resolved_target)) if resolved_alias == resolved_target
                );
            if trusted {
                return target.join(remainder);
            }
        }
    }
    path
}

fn preflight_existing_store(root: &Path) -> EvidenceResult<()> {
    for entry in fs::read_dir(root)
        .map_err(|error| EvidenceError::io("inspect Evidence Store root", error))?
    {
        let entry = entry.map_err(|error| EvidenceError::io("inspect store entry", error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(version) = name.strip_prefix('v')
            && name != "v1"
            && version.bytes().all(|byte| byte.is_ascii_digit())
        {
            let version = version.parse::<u32>().unwrap_or(u32::MAX);
            return Err(EvidenceError::UnsupportedStoreVersion(version));
        }
    }

    let version_root = root.join("v1");
    if !version_root.exists() {
        return Ok(());
    }
    require_real_directory(&version_root, "store version directory")?;
    let header_path = version_root.join("store.json");
    if !header_path.exists() {
        return Ok(());
    }
    let header: StoreHeader = read_store_json(&header_path, "preflight store header")?;
    if header.schema_version != STORE_VERSION {
        return Err(EvidenceError::UnsupportedStoreVersion(
            header.schema_version,
        ));
    }
    if header.hash_algorithm != HASH_ALGORITHM {
        return Err(EvidenceError::CorruptStore(
            "unsupported Evidence Store hash algorithm".to_owned(),
        ));
    }
    Ok(())
}

fn real_directory(parent: &Path, component: &str) -> EvidenceResult<PathBuf> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains('/')
        || component.contains('\\')
    {
        return Err(EvidenceError::UnsafePath(
            "invalid store directory component".to_owned(),
        ));
    }
    let path = parent.join(component);
    let mut created = false;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(EvidenceError::UnsafePath(
                "store path component must be a real directory".to_owned(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&path)
                .map_err(|error| EvidenceError::io("create store directory", error))?;
            created = true;
        }
        Err(error) => return Err(EvidenceError::io("inspect store directory", error)),
    }
    set_private_directory_permissions(&path)?;
    if created {
        sync_directory(parent)?;
    }
    Ok(path)
}

fn create_private_directory(path: &Path) -> EvidenceResult<()> {
    fs::create_dir(path).map_err(|error| EvidenceError::io("create staging directory", error))?;
    set_private_directory_permissions(path)
}

fn set_private_directory_permissions(path: &Path) -> EvidenceResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| EvidenceError::io("set directory permissions", error))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn open_private_file(path: &Path, create_new: bool) -> EvidenceResult<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| EvidenceError::io("open private store file", error))
}

fn reject_symlink_if_present(path: &Path, label: &str) -> EvidenceResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(EvidenceError::UnsafePath(
            format!("{label} must not be a symlink"),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EvidenceError::io("inspect store path", error)),
    }
}

fn require_real_directory(path: &Path, label: &str) -> EvidenceResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| EvidenceError::io("inspect store directory", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(EvidenceError::UnsafePath(format!(
            "{label} must be a real directory"
        )));
    }
    Ok(())
}

fn require_regular_file(path: &Path, label: &str) -> EvidenceResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| EvidenceError::io("inspect store file", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(EvidenceError::UnsafePath(format!(
            "{label} must be a regular file"
        )));
    }
    Ok(())
}

fn ensure_tree_has_no_symlinks(path: &Path) -> EvidenceResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| EvidenceError::io("inspect staging tree", error))?;
    if metadata.file_type().is_symlink() {
        return Err(EvidenceError::UnsafePath(
            "staging tree contains a symlink".to_owned(),
        ));
    }
    if metadata.is_dir() {
        for entry in
            fs::read_dir(path).map_err(|error| EvidenceError::io("list staging tree", error))?
        {
            ensure_tree_has_no_symlinks(
                &entry
                    .map_err(|error| EvidenceError::io("read staging entry", error))?
                    .path(),
            )?;
        }
    }
    Ok(())
}

fn read_directory_sorted(path: &Path, operation: &str) -> EvidenceResult<Vec<fs::DirEntry>> {
    require_real_directory(path, operation)?;
    let mut entries = fs::read_dir(path)
        .map_err(|error| EvidenceError::io(operation, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| EvidenceError::io(operation, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn utf8_file_name(path: &Path, label: &str) -> EvidenceResult<String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| EvidenceError::UnsafePath(format!("{label} is not UTF-8")))
}

fn digest_from_json_file_name(path: &Path) -> EvidenceResult<Sha256Digest> {
    let name = utf8_file_name(path, "digest marker")?;
    let digest = name
        .strip_suffix(".json")
        .ok_or_else(|| EvidenceError::UnsafePath("digest marker must end in .json".to_owned()))?;
    Sha256Digest::parse(digest.to_owned())
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_atomic_temporary_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some((target, identifier)) = rest.rsplit_once(".tmp-") else {
        return false;
    };
    !target.is_empty()
        && Uuid::parse_str(identifier).is_ok_and(|uuid| uuid.to_string() == identifier)
}

fn parse_canonical_uuid(value: &str, label: &str) -> EvidenceResult<Uuid> {
    let uuid = Uuid::parse_str(value)
        .map_err(|_| EvidenceError::UnsafePath(format!("invalid {label}")))?;
    if uuid.to_string() != value {
        return Err(EvidenceError::UnsafePath(format!("non-canonical {label}")));
    }
    Ok(uuid)
}

fn cleanup_atomic_files_in(directory: &Path) -> EvidenceResult<()> {
    let mut changed = false;
    for entry in read_directory_sorted(directory, "list metadata directory")? {
        let name = utf8_file_name(&entry.path(), "metadata entry")?;
        if is_atomic_temporary_name(&name) {
            require_regular_file(&entry.path(), "atomic temporary")?;
            fs::remove_file(entry.path())
                .map_err(|error| EvidenceError::io("remove atomic temporary", error))?;
            changed = true;
        }
    }
    if changed {
        sync_directory(directory)?;
    }
    Ok(())
}

fn read_json_file<T: DeserializeOwned>(path: &Path, operation: &str) -> EvidenceResult<T> {
    require_regular_file(path, operation)?;
    let metadata = fs::metadata(path).map_err(|error| EvidenceError::io(operation, error))?;
    if metadata.len() > METADATA_LIMIT_BYTES {
        return Err(EvidenceError::UnsafePath(
            "store metadata exceeds its size limit".to_owned(),
        ));
    }
    let bytes = fs::read(path).map_err(|error| EvidenceError::io(operation, error))?;
    serde_json::from_slice(&bytes).map_err(|error| EvidenceError::Internal(error.to_string()))
}

fn read_digest_json<T: DeserializeOwned>(
    path: &Path,
    operation: &str,
    digest: &Sha256Digest,
    kind: &str,
) -> EvidenceResult<T> {
    read_json_file(path, operation).map_err(|error| match error {
        EvidenceError::Internal(_) => EvidenceError::CorruptMetadata {
            digest: digest.clone(),
            reason: format!("{kind} is not valid JSON"),
        },
        other => other,
    })
}

fn read_store_json<T: DeserializeOwned>(path: &Path, operation: &str) -> EvidenceResult<T> {
    read_json_file(path, operation).map_err(|error| match error {
        EvidenceError::Internal(_) => {
            EvidenceError::CorruptStore(format!("{operation} is not valid JSON"))
        }
        other => other,
    })
}

fn write_synced_json_file<T: Serialize>(
    path: &Path,
    value: &T,
    operation: &str,
) -> EvidenceResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| EvidenceError::Internal(error.to_string()))?;
    let mut file = open_private_file(path, true)?;
    file.write_all(&bytes)
        .map_err(|error| EvidenceError::io(operation, error))?;
    file.sync_all()
        .map_err(|error| EvidenceError::io(operation, error))
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T, operation: &str) -> EvidenceResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| EvidenceError::UnsafePath("metadata has no parent".to_owned()))?;
    require_real_directory(parent, "metadata parent")?;
    reject_symlink_if_present(path, "metadata target")?;
    if path.exists() {
        return Ok(());
    }
    let name = utf8_file_name(path, "metadata target")?;
    let temporary = parent.join(format!(".{name}.tmp-{}", Uuid::new_v4()));
    let mut guard = TemporaryFileGuard::new(temporary.clone());
    write_synced_json_file(&temporary, value, operation)?;
    fs::rename(&temporary, path).map_err(|error| EvidenceError::io(operation, error))?;
    guard.published = true;
    sync_directory(parent)
}

struct TemporaryFileGuard {
    path: PathBuf,
    published: bool,
}

impl TemporaryFileGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }
}

impl Drop for TemporaryFileGuard {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> EvidenceResult<()> {
    let directory = File::open(path).map_err(|error| EvidenceError::io("open directory", error))?;
    directory
        .sync_all()
        .map_err(|error| EvidenceError::io("sync directory", error))
}

#[cfg(windows)]
fn sync_directory(path: &Path) -> EvidenceResult<()> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let directory = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|error| EvidenceError::io("open directory", error))?;
    directory
        .sync_all()
        .map_err(|error| EvidenceError::io("sync directory", error))
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(path: &Path) -> EvidenceResult<()> {
    let _ = path;
    Err(EvidenceError::Internal(
        "directory durability is unsupported on this platform".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };

    #[cfg(target_os = "macos")]
    use std::path::Path;

    use devicerail_core::{EvidenceError, EvidenceStore as _, GcPolicy, PutEvidence, Sha256Digest};
    use devicerail_protocol::SessionId;
    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt as _;
    use tokio::{
        io::{AsyncRead, AsyncReadExt as _, ReadBuf},
        sync::Notify,
    };
    use uuid::Uuid;

    use super::{FileEvidenceStore, FileEvidenceStoreConfig};

    fn store(root: &TempDir, max_asset_bytes: u64) -> FileEvidenceStore {
        FileEvidenceStore::new(
            root.path(),
            FileEvidenceStoreConfig {
                max_asset_bytes,
                max_references_per_session: 100,
                max_concurrent_writes: 4,
            },
        )
        .expect("open store")
    }

    async fn put(
        store: &FileEvidenceStore,
        session_id: SessionId,
        bytes: &[u8],
    ) -> devicerail_core::StoredEvidence {
        store
            .put(
                PutEvidence::new(session_id, "image/png").expect("request"),
                Box::pin(Cursor::new(bytes.to_vec())),
            )
            .await
            .expect("put evidence")
    }

    #[tokio::test]
    async fn put_is_content_addressed_deduplicated_and_verified() {
        let root = TempDir::new().expect("temp root");
        let store = store(&root, 1024);
        let first_session = SessionId::new();
        let second_session = SessionId::new();
        let first = put(&store, first_session, b"same bytes").await;
        let second = store
            .attach(&second_session, &first.asset_ref())
            .await
            .expect("attach existing evidence");
        assert!(!first.deduplicated());
        assert!(second.deduplicated());
        assert_eq!(first.asset_ref().uri, second.asset_ref().uri);

        let metadata = store
            .metadata(first.metadata().digest())
            .await
            .expect("metadata");
        assert_eq!(metadata.reference_count(), 2);
        assert_eq!(store.object_digests().expect("objects").len(), 1);
        let mut output = store.open(first.metadata().digest()).await.expect("open");
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).await.expect("read");
        assert_eq!(bytes, b"same bytes");
    }

    #[tokio::test]
    async fn session_reference_verification_is_read_only_and_session_scoped() {
        let root = TempDir::new().expect("temp root");
        let store = store(&root, 1024);
        let owner = SessionId::new();
        let other = SessionId::new();
        let evidence = put(&store, owner.clone(), b"owned bytes").await;
        let asset = evidence.asset_ref();

        let metadata = store
            .verify_session_reference(&owner, &asset)
            .await
            .expect("owner reference");
        assert_eq!(metadata.digest(), evidence.metadata().digest());

        let error = store
            .verify_session_reference(&other, &asset)
            .await
            .expect_err("another Session must not inherit ownership");
        assert!(matches!(
            error,
            EvidenceError::NotAttached {
                session_id,
                digest: _
            } if session_id == other
        ));
        assert_eq!(
            store.referenced_sessions().await.expect("references"),
            vec![owner]
        );
    }

    #[tokio::test]
    async fn media_type_conflicts_and_corrupt_bytes_are_explicit() {
        let root = TempDir::new().expect("temp root");
        let store = store(&root, 1024);
        let evidence = put(&store, SessionId::new(), b"original").await;
        let digest = evidence.metadata().digest().clone();
        let conflict = store
            .put(
                PutEvidence::new(SessionId::new(), "text/plain").expect("request"),
                Box::pin(Cursor::new(b"original".to_vec())),
            )
            .await
            .expect_err("same object cannot change media type");
        assert!(matches!(conflict, EvidenceError::MediaTypeConflict { .. }));

        std::fs::write(store.object_directory(&digest).join("data"), b"tampered")
            .expect("corrupt bytes");
        assert!(matches!(
            store.open(&digest).await,
            Err(EvidenceError::Corrupt { .. })
        ));

        std::fs::write(store.object_directory(&digest).join("data"), b"original")
            .expect("restore bytes");
        std::fs::remove_file(store.object_directory(&digest).join("data")).expect("remove data");
        assert!(matches!(
            store.open(&digest).await,
            Err(EvidenceError::CorruptMetadata { .. })
        ));
    }

    #[tokio::test]
    async fn size_digest_and_empty_failures_leave_no_visible_object() {
        let root = TempDir::new().expect("temp root");
        let store = store(&root, 4);
        let session = SessionId::new();
        assert!(
            store
                .put(
                    PutEvidence::new(session.clone(), "image/png").expect("request"),
                    Box::pin(Cursor::new(Vec::<u8>::new())),
                )
                .await
                .is_err()
        );
        assert!(matches!(
            store
                .put(
                    PutEvidence::new(session.clone(), "image/png")
                        .expect("request")
                        .with_declared_size_bytes(3),
                    Box::pin(Cursor::new(b"1234".to_vec())),
                )
                .await,
            Err(EvidenceError::DeclaredSizeMismatch {
                declared: 3,
                actual: 4
            })
        ));
        assert!(
            store
                .put(
                    PutEvidence::new(session.clone(), "image/png").expect("request"),
                    Box::pin(Cursor::new(b"12345".to_vec())),
                )
                .await
                .is_err()
        );
        let wrong = Sha256Digest::parse("0".repeat(64)).expect("digest");
        assert!(
            store
                .put(
                    PutEvidence::new(session, "image/png")
                        .expect("request")
                        .with_expected_sha256(wrong),
                    Box::pin(Cursor::new(b"1234".to_vec())),
                )
                .await
                .is_err()
        );
        assert!(
            std::fs::read_dir(root.path().join("v1/staging"))
                .expect("staging")
                .next()
                .is_none()
        );
        let missing = Sha256Digest::parse("f".repeat(64)).expect("missing digest");
        assert!(matches!(
            store.open(&missing).await,
            Err(EvidenceError::NotFound(digest)) if digest == missing
        ));
    }

    #[tokio::test]
    async fn shared_references_are_released_before_gc() {
        let root = TempDir::new().expect("temp root");
        let store = store(&root, 1024);
        let first_session = SessionId::new();
        let second_session = SessionId::new();
        let evidence = put(&store, first_session.clone(), b"shared").await;
        put(&store, second_session.clone(), b"shared").await;
        let digest = evidence.metadata().digest().clone();

        let first = store
            .release_session(&first_session, 10)
            .await
            .expect("release first");
        assert_eq!(first.newly_unreferenced_assets, 0);
        assert!(store.open(&digest).await.is_ok());
        let second = store
            .release_session(&second_session, 20)
            .await
            .expect("release second");
        assert_eq!(second.newly_unreferenced_assets, 1);
        let retry = store
            .release_session(&second_session, 999)
            .await
            .expect("release is idempotent");
        assert_eq!(retry.released_references, 0);

        let dry_run = store.gc(GcPolicy::dry_run(20)).await.expect("dry run");
        assert_eq!(dry_run.candidate_assets, 1);
        assert_eq!(dry_run.deleted_assets, 0);
        let deleted = store.gc(GcPolicy::delete(20)).await.expect("gc");
        assert_eq!(deleted.deleted_assets, 1);
        assert!(store.open(&digest).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_identical_puts_publish_one_object() {
        let root = TempDir::new().expect("temp root");
        let store = Arc::new(store(&root, 1024));
        let session = SessionId::new();
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let store = Arc::clone(&store);
            let session = session.clone();
            tasks.push(tokio::spawn(async move {
                put(&store, session, b"parallel").await.asset_ref()
            }));
        }
        let mut references = Vec::new();
        for task in tasks {
            references.push(task.await.expect("join"));
        }
        assert!(
            references
                .iter()
                .all(|value| value.uri == references[0].uri)
        );
        let digest = FileEvidenceStore::validate_asset_ref(&references[0]).expect("reference");
        assert_eq!(
            store
                .metadata(&digest)
                .await
                .expect("metadata")
                .reference_count(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn blocked_object_publication_does_not_stall_an_unrelated_session() {
        let root = TempDir::new().expect("temp root");
        let store = Arc::new(store(&root, 1024));
        let blocked_bytes = b"blocked-object".to_vec();
        let blocked_digest =
            super::digest_from_hash(Sha256::digest(&blocked_bytes)).expect("blocked digest");
        let (independent_bytes, independent_digest) = (0_u16..=u16::MAX)
            .map(|seed| seed.to_be_bytes().to_vec())
            .find_map(|bytes| {
                let digest = super::digest_from_hash(Sha256::digest(&bytes)).ok()?;
                (digest.as_str()[..2] != blocked_digest.as_str()[..2]).then_some((bytes, digest))
            })
            .expect("independent object stripe");
        assert_ne!(
            &blocked_digest.as_str()[..2],
            &independent_digest.as_str()[..2]
        );
        let blocked_session = SessionId::from(Uuid::from_bytes([1; 16]));
        let independent_session = SessionId::from(Uuid::from_bytes([2; 16]));
        let blocked_gate = store.object_gate(&blocked_digest).lock().await;
        let blocked_task = {
            let store = Arc::clone(&store);
            tokio::spawn(async move { put(&store, blocked_session, &blocked_bytes).await })
        };
        let staging = root.path().join("v1/staging");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if std::fs::read_dir(&staging)
                    .expect("staging")
                    .next()
                    .is_some()
                {
                    break;
                }
                assert!(
                    !blocked_task.is_finished(),
                    "blocked put exited before finishing staging"
                );
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("blocked put must finish staging");
        assert!(
            !blocked_task.is_finished(),
            "object stripe must block publication"
        );

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            put(&store, independent_session, &independent_bytes),
        )
        .await
        .expect("unrelated put must not wait for the blocked object stripe");
        drop(blocked_gate);
        blocked_task.await.expect("blocked put task");
    }

    struct OneChunkThenPending {
        emitted: bool,
        notify: Arc<Notify>,
    }

    impl AsyncRead for OneChunkThenPending {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.emitted {
                return Poll::Pending;
            }
            self.emitted = true;
            buffer.put_slice(b"partial");
            self.notify.notify_one();
            Poll::Ready(Ok(()))
        }
    }

    struct OneChunkThenError(bool);

    impl AsyncRead for OneChunkThenError {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.0 {
                return Poll::Ready(Err(std::io::Error::other("injected read failure")));
            }
            self.0 = true;
            buffer.put_slice(b"partial");
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn input_io_failure_is_explicit_and_cleans_staging() {
        let root = TempDir::new().expect("temp root");
        let store = store(&root, 1024);
        assert!(matches!(
            store
                .put(
                    PutEvidence::new(SessionId::new(), "image/png").expect("request"),
                    Box::pin(OneChunkThenError(false)),
                )
                .await,
            Err(EvidenceError::Io { .. })
        ));
        assert!(
            std::fs::read_dir(root.path().join("v1/staging"))
                .expect("staging")
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn cancelling_a_streaming_put_cleans_its_staging_directory() {
        let root = TempDir::new().expect("temp root");
        let store = Arc::new(store(&root, 1024));
        let notify = Arc::new(Notify::new());
        let observed = notify.notified();
        let task = {
            let store = Arc::clone(&store);
            let notify = Arc::clone(&notify);
            tokio::spawn(async move {
                store
                    .put(
                        PutEvidence::new(SessionId::new(), "image/png").expect("request"),
                        Box::pin(OneChunkThenPending {
                            emitted: false,
                            notify,
                        }),
                    )
                    .await
            })
        };
        observed.await;
        task.abort();
        let _ = task.await;

        assert!(
            std::fs::read_dir(root.path().join("v1/staging"))
                .expect("staging")
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn released_session_tombstone_rejects_a_slow_put_after_staging() {
        let root = TempDir::new().expect("temp root");
        let store = Arc::new(store(&root, 1024));
        let baseline = put(&store, SessionId::new(), b"readable during upload").await;
        let session = SessionId::new();
        let (mut writer, reader) = tokio::io::duplex(64);
        let task = {
            let store = Arc::clone(&store);
            let session = session.clone();
            tokio::spawn(async move {
                store
                    .put(
                        PutEvidence::new(session, "video/mp4").expect("request"),
                        Box::pin(reader),
                    )
                    .await
            })
        };
        writer.write_all(b"video-").await.expect("partial stream");

        let staging = root.path().join("v1/staging");
        let mut observed_staging = false;
        for _ in 0..100 {
            observed_staging = std::fs::read_dir(&staging)
                .expect("staging")
                .next()
                .is_some();
            if observed_staging {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(observed_staging, "put must reach staging before release");
        assert!(store.open(baseline.metadata().digest()).await.is_ok());

        store
            .release_session(&session, 10)
            .await
            .expect("persist release intent");
        writer.write_all(b"payload").await.expect("finish stream");
        writer.shutdown().await.expect("close stream");
        let error = task
            .await
            .expect("join put")
            .expect_err("closed Session rejects late publication");
        assert!(matches!(error, EvidenceError::SessionClosed(id) if id == session));
        let referenced_sessions = store
            .referenced_sessions()
            .await
            .expect("referenced Sessions");
        assert!(!referenced_sessions.contains(&session));
        assert_eq!(
            store
                .gc(GcPolicy::delete(u64::MAX))
                .await
                .expect("collect rejected upload")
                .deleted_assets,
            1
        );
    }

    #[test]
    fn startup_cleans_owned_atomic_temporaries_and_empty_ref_directories() {
        let root = TempDir::new().expect("temp root");
        let session = SessionId::new();
        let reference_directory;
        {
            let store = store(&root, 1024);
            reference_directory = store.reference_directory(&session);
            std::fs::create_dir(&reference_directory).expect("empty ref directory");
            let temporary = reference_directory.join(format!(
                ".{}.json.tmp-{}",
                "0".repeat(64),
                Uuid::new_v4()
            ));
            std::fs::write(temporary, b"partial").expect("atomic temporary");
            std::fs::write(
                root.path()
                    .join("v1")
                    .join(format!(".store.json.tmp-{}", Uuid::new_v4())),
                b"partial",
            )
            .expect("header temporary");
        }

        let _reopened = store(&root, 1024);
        assert!(!reference_directory.exists());
        assert!(
            std::fs::read_dir(root.path().join("v1"))
                .expect("store root")
                .all(|entry| !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp-"))
        );
    }

    #[tokio::test]
    async fn startup_finishes_each_gc_trash_intermediate_state() {
        for remove_marker in [false, true] {
            let root = TempDir::new().expect("temp root");
            let session = SessionId::new();
            let digest;
            {
                let store = store(&root, 1024);
                let evidence = put(&store, session.clone(), b"trash recovery").await;
                digest = evidence.metadata().digest().clone();
                store.release_session(&session, 1).await.expect("release");
                std::fs::rename(
                    store.object_directory(&digest),
                    store.trash.join(digest.as_str()),
                )
                .expect("simulate GC rename");
                if remove_marker {
                    std::fs::remove_file(store.unreferenced_marker_path(&digest))
                        .expect("simulate marker commit");
                }
            }

            let reopened = store(&root, 1024);
            assert!(!reopened.trash.join(digest.as_str()).exists());
            assert!(!reopened.object_directory(&digest).exists());
            assert!(!reopened.unreferenced_marker_path(&digest).exists());
        }

        let root = TempDir::new().expect("temp root");
        let session = SessionId::new();
        let marker;
        {
            let store = store(&root, 1024);
            let evidence = put(&store, session.clone(), b"deletion already won").await;
            let digest = evidence.metadata().digest().clone();
            marker = store.unreferenced_marker_path(&digest);
            store.release_session(&session, 1).await.expect("release");
            std::fs::remove_dir_all(store.object_directory(&digest))
                .expect("simulate deletion durable before marker removal");
        }
        let _reopened = store(&root, 1024);
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn startup_restores_a_referenced_object_found_in_gc_trash() {
        let root = TempDir::new().expect("temp root");
        let digest;
        {
            let store = store(&root, 1024);
            let evidence = put(&store, SessionId::new(), b"still referenced").await;
            digest = evidence.metadata().digest().clone();
            std::fs::rename(
                store.object_directory(&digest),
                store.trash.join(digest.as_str()),
            )
            .expect("simulate unexpected trash state");
        }

        let reopened = store(&root, 1024);
        assert!(reopened.object_directory(&digest).exists());
        assert!(reopened.open(&digest).await.is_ok());
    }

    #[tokio::test]
    async fn startup_converges_identical_live_and_gc_trash_copies() {
        let root = TempDir::new().expect("temp root");
        let digest;
        {
            let store = store(&root, 1024);
            let evidence = put(&store, SessionId::new(), b"duplicate crash state").await;
            digest = evidence.metadata().digest().clone();
            let live = store.object_directory(&digest);
            let trash = store.trash.join(digest.as_str());
            std::fs::create_dir(&trash).expect("trash copy directory");
            std::fs::copy(live.join("data"), trash.join("data")).expect("copy data");
            std::fs::copy(live.join("meta.json"), trash.join("meta.json")).expect("copy metadata");
        }

        let reopened = store(&root, 1024);
        assert!(reopened.object_directory(&digest).exists());
        assert!(!reopened.trash.join(digest.as_str()).exists());
        assert!(reopened.open(&digest).await.is_ok());
    }

    #[tokio::test]
    async fn malformed_reference_and_gc_markers_abort_conservatively() {
        let root = TempDir::new().expect("temp root");
        let primary = store(&root, 1024);
        let session = SessionId::new();
        let evidence = put(&primary, session.clone(), b"protected").await;
        let digest = evidence.metadata().digest().clone();
        std::fs::write(primary.reference_path(&session, &digest), b"{}")
            .expect("corrupt reference marker");
        assert!(matches!(
            primary.metadata(&digest).await,
            Err(EvidenceError::CorruptMetadata { .. })
        ));

        // Restore by reopening a separate root so a malformed GC marker can
        // be tested without asking recovery to guess what the broken ref meant.
        let other_root = TempDir::new().expect("other root");
        let other = store(&other_root, 1024);
        let other_session = SessionId::new();
        let other_evidence = put(&other, other_session.clone(), b"collectable").await;
        let other_digest = other_evidence.metadata().digest().clone();
        other
            .release_session(&other_session, 1)
            .await
            .expect("release");
        std::fs::write(other.unreferenced_marker_path(&other_digest), b"{}")
            .expect("corrupt GC marker");
        assert!(matches!(
            other.gc(GcPolicy::delete(u64::MAX)).await,
            Err(EvidenceError::CorruptMetadata { .. })
        ));
        assert!(other.object_directory(&other_digest).exists());
    }

    #[tokio::test]
    async fn reference_limit_leaves_a_recoverable_orphan_not_a_dangling_ref() {
        let root = TempDir::new().expect("temp root");
        let store = FileEvidenceStore::new(
            root.path(),
            FileEvidenceStoreConfig {
                max_asset_bytes: 1024,
                max_references_per_session: 1,
                max_concurrent_writes: 4,
            },
        )
        .expect("open store");
        let session = SessionId::new();
        let retained = put(&store, session.clone(), b"retained").await;
        let error = store
            .put(
                PutEvidence::new(session, "image/png").expect("request"),
                Box::pin(Cursor::new(b"orphan".to_vec())),
            )
            .await
            .expect_err("reference limit");
        assert!(matches!(error, EvidenceError::ReferenceLimit { .. }));

        let report = store
            .gc(GcPolicy::delete(u64::MAX))
            .await
            .expect("collect orphan");
        assert_eq!(report.deleted_assets, 1);
        assert!(store.open(retained.metadata().digest()).await.is_ok());
    }

    #[test]
    fn startup_cleans_abandoned_staging_and_recovers_orphans() {
        let root = TempDir::new().expect("temp root");
        let session = SessionId::new();
        let digest = {
            let store = store(&root, 1024);
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            let evidence = runtime.block_on(put(&store, session.clone(), b"orphaned"));
            evidence.metadata().digest().clone()
        };

        std::fs::remove_dir_all(
            root.path()
                .join("v1/refs/sessions")
                .join(session.to_string()),
        )
        .expect("simulate crash after log cleanup");
        let abandoned = root
            .path()
            .join("v1/staging")
            .join(format!(".part-{}", Uuid::new_v4()));
        std::fs::create_dir(&abandoned).expect("abandoned staging");
        std::fs::write(abandoned.join("data"), b"partial").expect("partial data");

        let store = store(&root, 1024);
        assert!(!abandoned.exists());
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let report = runtime
            .block_on(store.gc(GcPolicy::dry_run(u64::MAX)))
            .expect("orphan is recoverable");
        assert_eq!(report.candidate_assets, 1);
        assert!(runtime.block_on(store.open(&digest)).is_ok());
    }

    #[tokio::test]
    async fn startup_completes_a_persisted_release_intent() {
        let root = TempDir::new().expect("temp root");
        let session = SessionId::new();
        let asset;
        {
            let store = store(&root, 1024);
            asset = put(&store, session.clone(), b"release recovery")
                .await
                .asset_ref();
            store
                .persist_session_release(&session, 42)
                .expect("simulate durable intent before crash");
        }

        let reopened = store(&root, 1024);
        assert!(
            reopened
                .referenced_sessions()
                .await
                .expect("references")
                .is_empty()
        );
        assert!(matches!(
            reopened.attach(&session, &asset).await,
            Err(EvidenceError::SessionClosed(id)) if id == session
        ));
        assert_eq!(
            reopened
                .gc(GcPolicy::dry_run(u64::MAX))
                .await
                .expect("recovered orphan")
                .candidate_assets,
            1
        );
    }

    #[tokio::test]
    async fn gc_byte_limit_skips_large_object_without_starving_small_object() {
        let root = TempDir::new().expect("temp root");
        let store = store(&root, 1024);
        let mut pair = None;
        'outer: for large_seed in 0_u8..=u8::MAX {
            let large = vec![large_seed; 101];
            let large_digest =
                super::digest_from_hash(Sha256::digest(&large)).expect("large digest");
            for small_seed in 0_u8..=u8::MAX {
                let small = vec![small_seed];
                let small_digest =
                    super::digest_from_hash(Sha256::digest(&small)).expect("small digest");
                if large_digest < small_digest {
                    pair = Some((large, large_digest, small, small_digest));
                    break 'outer;
                }
            }
        }
        let (large, large_digest, small, small_digest) = pair.expect("ordered digest pair");
        let large_session = SessionId::new();
        let small_session = SessionId::new();
        put(&store, large_session.clone(), &large).await;
        put(&store, small_session.clone(), &small).await;
        store
            .release_session(&large_session, 1)
            .await
            .expect("release large");
        store
            .release_session(&small_session, 1)
            .await
            .expect("release small");

        let report = store
            .gc(GcPolicy {
                unreferenced_before_ms: 1,
                max_assets: None,
                max_bytes: Some(1),
                dry_run: false,
            })
            .await
            .expect("bounded GC");
        assert_eq!(report.deleted_assets, 1);
        assert!(store.object_directory(&large_digest).exists());
        assert!(!store.object_directory(&small_digest).exists());
    }

    #[test]
    fn invalid_write_limit_returns_an_error_instead_of_panicking() {
        let root = TempDir::new().expect("temp root");
        let result = std::panic::catch_unwind(|| {
            FileEvidenceStore::new(
                root.path(),
                FileEvidenceStoreConfig {
                    max_asset_bytes: 1,
                    max_references_per_session: 1,
                    max_concurrent_writes: usize::MAX,
                },
            )
        });
        assert!(matches!(
            result,
            Ok(Err(EvidenceError::InvalidConfiguration(_)))
        ));
    }

    #[test]
    fn creates_and_reopens_each_missing_root_ancestor() {
        let parent = TempDir::new().expect("temp parent");
        let root = parent.path().join("one/two/three/evidence");
        let store = FileEvidenceStore::new(&root, FileEvidenceStoreConfig::default())
            .expect("create nested Store root");
        assert_eq!(store.root(), root.canonicalize().expect("canonical root"));
        drop(store);
        FileEvidenceStore::new(&root, FileEvidenceStoreConfig::default())
            .expect("reopen nested Store root");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn accepts_the_root_owned_macos_var_compatibility_alias() {
        let parent = TempDir::new().expect("temp parent");
        let canonical_parent = parent.path().canonicalize().expect("canonical temp parent");
        let remainder = canonical_parent
            .strip_prefix("/private/var")
            .expect("macOS temp directory lives under /private/var");
        let requested = Path::new("/var").join(remainder).join("evidence");

        let store = FileEvidenceStore::new(&requested, FileEvidenceStoreConfig::default())
            .expect("create store through /var compatibility alias");
        assert_eq!(
            store.root(),
            requested.canonicalize().expect("canonical evidence root")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlinked_missing_root_ancestor() {
        use std::os::unix::fs::symlink;

        let parent = TempDir::new().expect("temp parent");
        let target = parent.path().join("target");
        std::fs::create_dir(&target).expect("target directory");
        let link = parent.path().join("link");
        symlink(&target, &link).expect("ancestor symlink");

        assert!(matches!(
            FileEvidenceStore::new(
                link.join("nested/evidence"),
                FileEvidenceStoreConfig::default()
            ),
            Err(EvidenceError::UnsafePath(_))
        ));
        assert!(!target.join("nested").exists());

        std::fs::create_dir(target.join("existing")).expect("existing target child");
        assert!(matches!(
            FileEvidenceStore::new(
                link.join("existing/evidence"),
                FileEvidenceStoreConfig::default()
            ),
            Err(EvidenceError::UnsafePath(_))
        ));
        assert!(!target.join("existing/evidence").exists());
    }

    #[test]
    fn unsupported_store_version_is_rejected_without_layout_mutation() {
        let root = TempDir::new().expect("temp root");
        let version = root.path().join("v1");
        std::fs::create_dir(&version).expect("version directory");
        std::fs::write(
            version.join("store.json"),
            br#"{"schemaVersion":2,"hashAlgorithm":"sha256"}"#,
        )
        .expect("future header");
        std::fs::write(root.path().join("sentinel"), b"unchanged").expect("sentinel");

        assert!(matches!(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default()),
            Err(EvidenceError::UnsupportedStoreVersion(2))
        ));
        assert!(!version.join("objects").exists());
        assert_eq!(
            std::fs::read(root.path().join("sentinel")).expect("sentinel remains"),
            b"unchanged"
        );
    }

    #[tokio::test]
    async fn noncanonical_session_reference_directory_is_rejected() {
        let root = TempDir::new().expect("temp root");
        let session = SessionId::new();
        {
            let store = store(&root, 1024);
            put(&store, session.clone(), b"canonical directory").await;
        }
        let canonical = root
            .path()
            .join("v1/refs/sessions")
            .join(session.to_string());
        let alias = canonical
            .parent()
            .expect("references")
            .join(session.0.simple().to_string());
        std::fs::rename(&canonical, &alias).expect("create UUID alias directory");
        assert!(matches!(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default()),
            Err(EvidenceError::UnsafePath(_))
        ));
        assert!(alias.exists());
    }

    #[test]
    fn canonical_reference_parser_rejects_uri_tricks() {
        let digest =
            Sha256Digest::parse("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .expect("digest");
        let metadata = devicerail_core::EvidenceMetadata::new(digest.clone(), "image/png", 1, 1, 1)
            .expect("metadata");
        let canonical = devicerail_core::StoredEvidence::new(metadata, false).asset_ref();
        assert_eq!(
            FileEvidenceStore::validate_asset_ref(&canonical).expect("canonical"),
            digest
        );

        for uri in [
            "file:///etc/passwd",
            "devicerail://assets/sha256/../escape",
            "devicerail://assets/sha256/%2e%2e",
            "devicerail://evil/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "devicerail://assets/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef?x=1",
        ] {
            let mut invalid = canonical.clone();
            invalid.uri = uri.to_owned();
            assert!(FileEvidenceStore::validate_asset_ref(&invalid).is_err());
        }
    }

    #[test]
    fn root_lock_rejects_a_second_store_instance() {
        let root = TempDir::new().expect("temp root");
        let first = store(&root, 1024);
        assert!(FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default()).is_err());
        drop(first);
        assert!(FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default()).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_object_data_is_rejected_without_touching_target() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().expect("temp root");
        let store = store(&root, 1024);
        let evidence = put(&store, SessionId::new(), b"safe data").await;
        let digest = evidence.metadata().digest().clone();
        let data = store.object_directory(&digest).join("data");
        let external = root.path().join("external");
        std::fs::write(&external, b"must remain unchanged").expect("external");
        std::fs::remove_file(&data).expect("replace data");
        symlink(&external, &data).expect("data symlink");

        assert!(matches!(
            store.open(&digest).await,
            Err(EvidenceError::UnsafePath(_))
        ));
        assert_eq!(
            std::fs::read(external).expect("external remains"),
            b"must remain unchanged"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_is_rejected_without_touching_target() {
        use std::os::unix::fs::symlink;

        let parent = TempDir::new().expect("temp parent");
        let target = parent.path().join("target");
        let link = parent.path().join("link");
        std::fs::create_dir(&target).expect("target");
        symlink(&target, &link).expect("symlink");
        assert!(FileEvidenceStore::new(&link, FileEvidenceStoreConfig::default()).is_err());
        assert!(
            std::fs::read_dir(target)
                .expect("target remains")
                .next()
                .is_none()
        );
    }
}
