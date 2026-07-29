use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read as _},
    path::{Path, PathBuf},
};

use serde::de::{DeserializeOwned, Error as _, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use thiserror::Error;

use devicerail_core::{EvidenceError, ExecutionControl};
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_session_bundle::{
    BundleError, BundleLimits, BundleSource, BundleSummary, export_directory, validate_directory,
    validate_source,
};

pub const SOURCE_MAX_BYTES: u64 = 8 * 1024 * 1024;

const USAGE: &str = "usage: devicerail-bundle export --source FILE --evidence-dir DIR --output DIR\n       devicerail-bundle validate DIR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Export {
        source: PathBuf,
        evidence_dir: PathBuf,
        output: PathBuf,
    },
    Validate {
        bundle: PathBuf,
    },
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{USAGE}")]
    Usage,
    #[error("source must be an existing regular file with the required platform protections")]
    UnsafeSource,
    #[error("source exceeds the 8 MiB BundleSource limit")]
    SourceTooLarge,
    #[error("source could not be read")]
    SourceRead,
    #[error("source JSON does not match the strict BundleSource schema")]
    InvalidSource,
    #[error("Evidence Store root must be an existing real directory")]
    UnsafeEvidenceRoot,
    #[error("Evidence Store is busy; stop the daemon before exporting")]
    EvidenceStoreBusy,
    #[error("Evidence Store could not be opened")]
    EvidenceStoreOpen,
    #[error("output parent must be an existing real directory")]
    UnsafeOutputParent,
    #[error("output target already exists")]
    OutputExists,
    #[error("output target must be outside the Evidence Store root")]
    OutputInsideEvidenceRoot,
    #[error("Bundle root must be an existing real directory")]
    UnsafeBundleRoot,
    #[error("Bundle operation was interrupted")]
    Interrupted,
    #[error(
        "Bundle was published, but parent-directory durability is unknown; validate the target before deciding whether to retry"
    )]
    PublishedDurabilityUnknown,
    #[error("failed to install the SIGINT handler")]
    Signal,
    #[error("Bundle export failed")]
    BundleExport,
    #[error("Bundle validation failed")]
    BundleValidation,
    #[error("failed to serialize the command summary")]
    Summary,
    #[error("failed to write the command summary")]
    SummaryWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSummary {
    pub ok: bool,
    pub operation: &'static str,
    #[serde(flatten)]
    pub bundle: BundleSummary,
}

pub fn parse_args<I>(arguments: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [
            command,
            source_flag,
            source,
            evidence_flag,
            evidence_dir,
            output_flag,
            output,
        ] if command == "export"
            && source_flag == "--source"
            && evidence_flag == "--evidence-dir"
            && output_flag == "--output" =>
        {
            Ok(Command::Export {
                source: PathBuf::from(source),
                evidence_dir: PathBuf::from(evidence_dir),
                output: PathBuf::from(output),
            })
        }
        [command, bundle] if command == "validate" => Ok(Command::Validate {
            bundle: PathBuf::from(bundle),
        }),
        _ => Err(CliError::Usage),
    }
}

pub fn read_source<T>(path: &Path) -> Result<T, CliError>
where
    T: DeserializeOwned + Serialize,
{
    let mut file = open_source(path)?;
    let metadata = file.metadata().map_err(|_| CliError::UnsafeSource)?;
    if !metadata.is_file() {
        return Err(CliError::UnsafeSource);
    }
    require_owner_only(&metadata)?;
    if metadata.len() > SOURCE_MAX_BYTES {
        return Err(CliError::SourceTooLarge);
    }

    let mut bytes = Vec::with_capacity(metadata.len().min(SOURCE_MAX_BYTES) as usize);
    file.by_ref()
        .take(SOURCE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CliError::SourceRead)?;
    if bytes.len() as u64 > SOURCE_MAX_BYTES {
        return Err(CliError::SourceTooLarge);
    }
    let value = serde_json::from_slice::<UniqueJsonValue>(&bytes)
        .map_err(|_| CliError::InvalidSource)?
        .0;
    let source: T = serde_json::from_value(value.clone()).map_err(|_| CliError::InvalidSource)?;
    let round_trip = serde_json::to_value(&source).map_err(|_| CliError::InvalidSource)?;
    if !json_values_equivalent(&round_trip, &value) {
        return Err(CliError::InvalidSource);
    }
    Ok(source)
}

fn json_values_equivalent(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Number(left), Value::Number(right)) => {
            left == right
                || left
                    .as_f64()
                    .zip(right.as_f64())
                    .is_some_and(|(left, right)| left == right)
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_values_equivalent(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| json_values_equivalent(left, right))
                })
        }
        _ => false,
    }
}

/// JSON value decoder that rejects duplicate object keys at every depth.
/// `serde_json::Value` alone keeps the last duplicate, which could otherwise
/// make a signed-off local source differ from the exported replay.
struct UniqueJsonValue(Value);

impl<'de> Deserialize<'de> for UniqueJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct UniqueValueVisitor;

        impl<'de> Visitor<'de> for UniqueValueVisitor {
            type Value = UniqueJsonValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Bool(value)))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Number(Number::from(value))))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Number(Number::from(value))))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Number::from_f64(value)
                    .map(Value::Number)
                    .map(UniqueJsonValue)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::String(value.to_owned())))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::String(value)))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Null))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Null))
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                UniqueJsonValue::deserialize(deserializer)
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<UniqueJsonValue>()? {
                    values.push(value.0);
                }
                Ok(UniqueJsonValue(Value::Array(values)))
            }

            fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut seen = BTreeSet::new();
                let mut values = Map::new();
                while let Some(key) = object.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(A::Error::custom("duplicate JSON object key"));
                    }
                    let value = object.next_value::<UniqueJsonValue>()?;
                    values.insert(key, value.0);
                }
                Ok(UniqueJsonValue(Value::Object(values)))
            }
        }

        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

fn open_source(path: &Path) -> Result<File, CliError> {
    let path_metadata = fs::symlink_metadata(path).map_err(|_| CliError::UnsafeSource)?;
    if metadata_is_link_like(&path_metadata) || !path_metadata.is_file() {
        return Err(CliError::UnsafeSource);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = options.open(path).map_err(|_| CliError::UnsafeSource)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let opened = file.metadata().map_err(|_| CliError::UnsafeSource)?;
        if opened.dev() != path_metadata.dev() || opened.ino() != path_metadata.ino() {
            return Err(CliError::UnsafeSource);
        }
    }
    Ok(file)
}

#[cfg(unix)]
fn require_owner_only(metadata: &fs::Metadata) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt as _;

    if metadata.permissions().mode() & 0o077 == 0 {
        Ok(())
    } else {
        Err(CliError::UnsafeSource)
    }
}

#[cfg(not(unix))]
fn require_owner_only(_metadata: &fs::Metadata) -> Result<(), CliError> {
    Ok(())
}

pub fn real_directory(path: &Path, kind: DirectoryKind) -> Result<PathBuf, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| kind.error())?;
    if metadata_is_link_like(&metadata) || !metadata.is_dir() {
        return Err(kind.error());
    }
    path.canonicalize().map_err(|_| kind.error())
}

fn metadata_is_link_like(metadata: &fs::Metadata) -> bool {
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

#[derive(Clone, Copy)]
pub enum DirectoryKind {
    EvidenceRoot,
    OutputParent,
    BundleRoot,
}

impl DirectoryKind {
    fn error(self) -> CliError {
        match self {
            Self::EvidenceRoot => CliError::UnsafeEvidenceRoot,
            Self::OutputParent => CliError::UnsafeOutputParent,
            Self::BundleRoot => CliError::UnsafeBundleRoot,
        }
    }
}

pub fn preflight_export_paths(
    evidence_dir: &Path,
    output: &Path,
) -> Result<(PathBuf, PathBuf), CliError> {
    let evidence_root = real_directory(evidence_dir, DirectoryKind::EvidenceRoot)?;
    let output_name = output.file_name().ok_or(CliError::UnsafeOutputParent)?;
    if output_name.is_empty() {
        return Err(CliError::UnsafeOutputParent);
    }
    let output_parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let output_parent = real_directory(output_parent, DirectoryKind::OutputParent)?;
    let output = output_parent.join(output_name);

    match fs::symlink_metadata(&output) {
        Ok(_) => return Err(CliError::OutputExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(CliError::UnsafeOutputParent),
    }
    if directory_is_within(&output_parent, &evidence_root)? {
        return Err(CliError::OutputInsideEvidenceRoot);
    }
    Ok((evidence_root, output))
}

#[cfg(unix)]
fn directory_is_within(candidate: &Path, root: &Path) -> Result<bool, CliError> {
    use std::os::unix::fs::MetadataExt as _;

    let root = fs::metadata(root).map_err(|_| CliError::UnsafeEvidenceRoot)?;
    for ancestor in candidate.ancestors() {
        let metadata = fs::metadata(ancestor).map_err(|_| CliError::UnsafeOutputParent)?;
        if metadata.dev() == root.dev() && metadata.ino() == root.ino() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(not(unix))]
fn directory_is_within(candidate: &Path, root: &Path) -> Result<bool, CliError> {
    Ok(candidate.starts_with(root))
}

pub fn preflight_validate_path(bundle: &Path) -> Result<PathBuf, CliError> {
    real_directory(bundle, DirectoryKind::BundleRoot)
}

pub fn open_evidence_store(root: &Path) -> Result<FileEvidenceStore, CliError> {
    FileEvidenceStore::new(root, FileEvidenceStoreConfig::default()).map_err(|error| match error {
        EvidenceError::StoreBusy => CliError::EvidenceStoreBusy,
        _ => CliError::EvidenceStoreOpen,
    })
}

pub async fn execute(
    command: Command,
    control: &ExecutionControl,
) -> Result<CommandSummary, CliError> {
    match command {
        Command::Export {
            source,
            evidence_dir,
            output,
        } => {
            // Source and path preflight deliberately finish before the Store
            // is opened and before the exporter is allowed to create staging.
            let source = read_source::<BundleSource>(&source)?;
            let limits = BundleLimits::default();
            validate_source(&source, &limits).map_err(|_| CliError::InvalidSource)?;
            let (evidence_root, output) = preflight_export_paths(&evidence_dir, &output)?;
            let evidence = open_evidence_store(&evidence_root)?;
            let bundle = export_directory(&source, &evidence, &output, &limits, control)
                .await
                .map_err(|error| map_bundle_error(error, CliError::BundleExport))?;
            Ok(CommandSummary {
                ok: true,
                operation: "export",
                bundle,
            })
        }
        Command::Validate { bundle } => {
            let bundle = preflight_validate_path(&bundle)?;
            let validated = validate_directory(&bundle, &BundleLimits::default(), control)
                .await
                .map_err(|error| map_bundle_error(error, CliError::BundleValidation))?;
            Ok(CommandSummary {
                ok: true,
                operation: "validate",
                bundle: validated.summary,
            })
        }
    }
}

fn map_bundle_error(error: BundleError, fallback: CliError) -> CliError {
    match error {
        BundleError::Cancelled { .. } | BundleError::TimedOut { .. } => CliError::Interrupted,
        BundleError::PublishedDurabilityUnknown => CliError::PublishedDurabilityUnknown,
        BundleError::TargetExists => CliError::OutputExists,
        BundleError::Model(_) if matches!(fallback, CliError::BundleExport) => {
            CliError::InvalidSource
        }
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs};

    use devicerail_core::{CancellationReason, ExecutionControl, TimeoutScope};
    use devicerail_session_bundle::{BundleError, BundleSource};
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use tempfile::TempDir;

    use super::{
        CliError, Command, SOURCE_MAX_BYTES, execute, map_bundle_error, open_evidence_store,
        parse_args, preflight_export_paths, read_source,
    };

    #[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    struct StrictSource {
        value: u8,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    struct LargeSource {
        value: String,
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_only_the_two_exact_command_shapes() {
        assert_eq!(
            parse_args(args(&[
                "export",
                "--source",
                "source.json",
                "--evidence-dir",
                "evidence",
                "--output",
                "bundle",
            ]))
            .expect("export"),
            Command::Export {
                source: "source.json".into(),
                evidence_dir: "evidence".into(),
                output: "bundle".into(),
            }
        );
        assert_eq!(
            parse_args(args(&["validate", "bundle"])).expect("validate"),
            Command::Validate {
                bundle: "bundle".into(),
            }
        );

        for invalid in [
            args(&[]),
            args(&["export"]),
            args(&[
                "export",
                "--output",
                "bundle",
                "--source",
                "source.json",
                "--evidence-dir",
                "evidence",
            ]),
            args(&["validate"]),
            args(&["validate", "bundle", "extra"]),
            args(&["--help"]),
        ] {
            assert!(matches!(parse_args(invalid), Err(CliError::Usage)));
        }
    }

    #[test]
    fn cancellation_and_post_publish_durability_have_distinct_cli_results() {
        for error in [
            BundleError::Cancelled {
                reason: CancellationReason::Requested,
            },
            BundleError::TimedOut {
                scope: TimeoutScope::Request,
                timeout_ms: 1,
            },
        ] {
            assert!(matches!(
                map_bundle_error(error, CliError::BundleExport),
                CliError::Interrupted
            ));
        }
        assert!(matches!(
            map_bundle_error(
                BundleError::PublishedDurabilityUnknown,
                CliError::BundleExport
            ),
            CliError::PublishedDurabilityUnknown
        ));
        assert!(matches!(
            map_bundle_error(BundleError::TargetExists, CliError::BundleExport),
            CliError::OutputExists
        ));
    }

    #[cfg(unix)]
    #[test]
    fn source_must_be_owner_only_and_is_never_echoed_in_errors() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source.json");
        fs::write(&source, br#"{"value":7}"#).expect("source");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).expect("permissions");
        assert_eq!(
            read_source::<StrictSource>(&source).expect("strict source"),
            StrictSource { value: 7 }
        );

        fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).expect("permissions");
        let error = read_source::<StrictSource>(&source).expect_err("group-readable source");
        assert!(matches!(error, CliError::UnsafeSource));
        assert!(!error.to_string().contains("value"));
    }

    #[test]
    fn source_is_strict_and_bounded() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source.json");
        fs::write(&source, br#"{"value":7,"secret":"do-not-echo"}"#).expect("source");
        set_owner_only(&source);
        let error = read_source::<StrictSource>(&source).expect_err("unknown source field");
        assert!(matches!(error, CliError::InvalidSource));
        assert!(!error.to_string().contains("do-not-echo"));

        let large = LargeSource {
            value: "x".repeat(2 * 1024 * 1024),
        };
        fs::write(
            &source,
            serde_json::to_vec(&large).expect("serialize large source"),
        )
        .expect("large valid source");
        set_owner_only(&source);
        assert_eq!(
            read_source::<LargeSource>(&source).expect("large source below the hard limit"),
            large
        );

        fs::write(&source, vec![b'x'; SOURCE_MAX_BYTES as usize + 1]).expect("large source");
        set_owner_only(&source);
        assert!(matches!(
            read_source::<StrictSource>(&source),
            Err(CliError::SourceTooLarge)
        ));

        fs::write(&source, br#"{"value":7,"value":8}"#).expect("duplicate source");
        set_owner_only(&source);
        assert!(matches!(
            read_source::<StrictSource>(&source),
            Err(CliError::InvalidSource)
        ));
    }

    #[test]
    fn nested_unknown_dto_fields_cannot_be_silently_dropped() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source.json");
        let value = json!({
            "eventProtocolVersion": { "major": 1, "minor": 2 },
            "sessionExport": {
                "session": {
                    "id": "33333333-3333-4333-8333-333333333333",
                    "state": "ended",
                    "startedAtMs": 100,
                    "endedAtMs": 300,
                    "eventCount": 3,
                    "lastSequence": 3
                },
                "events": [
                    {
                        "eventId": "66666666-6666-4666-8666-666666666661",
                        "sessionId": "33333333-3333-4333-8333-333333333333",
                        "sequence": 1,
                        "atMs": 100,
                        "payload": { "type": "sessionStarted" }
                    },
                    {
                        "eventId": "66666666-6666-4666-8666-666666666662",
                        "sessionId": "33333333-3333-4333-8333-333333333333",
                        "sequence": 2,
                        "atMs": 200,
                        "payload": {
                            "type": "observationCaptured",
                            "observation": {
                                "id": "77777777-7777-4777-8777-777777777777",
                                "deviceId": "mock-1",
                                "capturedAtMs": 200,
                                "viewport": {
                                    "width": 1,
                                    "height": 1,
                                    "scaleFactor": 1,
                                    "futureSecret": "must-not-disappear"
                                },
                                "screenshot": null,
                                "metadata": {}
                            }
                        }
                    },
                    {
                        "eventId": "66666666-6666-4666-8666-666666666663",
                        "sessionId": "33333333-3333-4333-8333-333333333333",
                        "sequence": 3,
                        "atMs": 300,
                        "payload": {
                            "type": "sessionEnded",
                            "outcome": "completed",
                            "reason": null
                        }
                    }
                ]
            }
        });
        let mut clean = value.clone();
        clean["sessionExport"]["events"][1]["payload"]["observation"]["viewport"]
            .as_object_mut()
            .expect("viewport")
            .remove("futureSecret");
        fs::write(
            &source,
            serde_json::to_vec(&clean).expect("clean source JSON"),
        )
        .expect("clean source file");
        set_owner_only(&source);
        read_source::<BundleSource>(&source).expect("integer spelling of an f64 stays valid");

        fs::write(&source, serde_json::to_vec(&value).expect("source JSON")).expect("source file");
        set_owner_only(&source);

        assert!(matches!(
            read_source::<BundleSource>(&source),
            Err(CliError::InvalidSource)
        ));
    }

    #[test]
    fn output_must_not_exist_or_be_inside_evidence_root() {
        let temp = TempDir::new().expect("tempdir");
        let evidence = temp.path().join("evidence");
        fs::create_dir(&evidence).expect("evidence");

        assert!(matches!(
            preflight_export_paths(&evidence, &evidence.join("bundle")),
            Err(CliError::OutputInsideEvidenceRoot)
        ));

        let existing = temp.path().join("existing");
        fs::write(&existing, b"not a bundle").expect("existing");
        assert!(matches!(
            preflight_export_paths(&evidence, &existing),
            Err(CliError::OutputExists)
        ));

        let existing_directory = temp.path().join("existing-directory");
        fs::create_dir(&existing_directory).expect("existing directory");
        assert!(matches!(
            preflight_export_paths(&evidence, &existing_directory),
            Err(CliError::OutputExists)
        ));
    }

    #[test]
    fn busy_evidence_store_fails_before_any_output_is_created() {
        let temp = TempDir::new().expect("tempdir");
        let evidence = temp.path().join("evidence");
        fs::create_dir(&evidence).expect("evidence");
        let held = open_evidence_store(&evidence).expect("held Evidence Store");
        let output = temp.path().join("bundle");

        let (canonical_evidence, canonical_output) =
            preflight_export_paths(&evidence, &output).expect("path preflight");
        assert!(matches!(
            open_evidence_store(&canonical_evidence),
            Err(CliError::EvidenceStoreBusy)
        ));
        assert!(!canonical_output.exists());
        assert!(
            fs::read_dir(temp.path())
                .expect("output parent")
                .all(|entry| !entry
                    .expect("parent entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".devicerail-bundle-"))
        );

        drop(held);
        assert!(open_evidence_store(&canonical_evidence).is_ok());
    }

    #[tokio::test]
    async fn busy_store_via_export_never_creates_output_staging() {
        let temp = TempDir::new().expect("tempdir");
        let evidence = temp.path().join("evidence");
        fs::create_dir(&evidence).expect("evidence");
        let held = open_evidence_store(&evidence).expect("held Evidence Store");
        let source = temp.path().join("source.json");
        write_ended_source(&source);
        let output = temp.path().join("bundle");
        let before = directory_names(temp.path());

        let error = execute(
            Command::Export {
                source,
                evidence_dir: evidence,
                output: output.clone(),
            },
            &ExecutionControl::unbounded(),
        )
        .await
        .expect_err("busy Store");
        assert!(matches!(error, CliError::EvidenceStoreBusy));
        assert!(!output.exists());
        assert_eq!(directory_names(temp.path()), before);
        drop(held);
    }

    #[tokio::test]
    async fn closed_file_store_exports_then_validates_offline() {
        let temp = TempDir::new().expect("tempdir");
        let evidence = temp.path().join("evidence");
        fs::create_dir(&evidence).expect("evidence");
        let held = open_evidence_store(&evidence).expect("initialize Evidence Store");
        drop(held);

        let source = temp.path().join("source.json");
        write_ended_source(&source);
        let output = temp.path().join("bundle");
        let exported = execute(
            Command::Export {
                source,
                evidence_dir: evidence,
                output: output.clone(),
            },
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("export");
        assert_eq!(exported.operation, "export");
        assert!(output.join("manifest.json").is_file());

        let validated = execute(
            Command::Validate {
                bundle: output.clone(),
            },
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("validate");
        assert_eq!(validated.operation, "validate");

        // Validation is offline: the Evidence Store may be busy again.
        let _held = open_evidence_store(&temp.path().join("evidence")).expect("reopen Store");
        execute(
            Command::Validate { bundle: output },
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("offline validation while Store is busy");
    }

    #[cfg(unix)]
    #[test]
    fn source_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        let real = temp.path().join("real.json");
        let link = temp.path().join("link.json");
        fs::write(&real, br#"{"value":7}"#).expect("real source");
        set_owner_only(&real);
        symlink(&real, &link).expect("symlink");
        assert!(matches!(
            read_source::<StrictSource>(&link),
            Err(CliError::UnsafeSource)
        ));

        let dangling = temp.path().join("dangling-output");
        symlink(temp.path().join("missing"), &dangling).expect("dangling symlink");
        let evidence = temp.path().join("evidence");
        fs::create_dir(&evidence).expect("evidence");
        assert!(matches!(
            preflight_export_paths(&evidence, &dangling),
            Err(CliError::OutputExists)
        ));

        let evidence_link = temp.path().join("evidence-link");
        symlink(&evidence, &evidence_link).expect("evidence symlink");
        assert!(matches!(
            preflight_export_paths(&evidence_link, &temp.path().join("bundle")),
            Err(CliError::UnsafeEvidenceRoot)
        ));

        let output_parent_link = temp.path().join("output-parent-link");
        let output_parent = temp.path().join("output-parent");
        fs::create_dir(&output_parent).expect("output parent");
        symlink(&output_parent, &output_parent_link).expect("output parent symlink");
        assert!(matches!(
            preflight_export_paths(&evidence, &output_parent_link.join("bundle")),
            Err(CliError::UnsafeOutputParent)
        ));
    }

    fn set_owner_only(_path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(_path, fs::Permissions::from_mode(0o600)).expect("permissions");
        }
    }

    fn write_ended_source(path: &std::path::Path) {
        let source = json!({
            "eventProtocolVersion": { "major": 1, "minor": 2 },
            "sessionExport": {
                "session": {
                    "id": "33333333-3333-4333-8333-333333333333",
                    "state": "ended",
                    "startedAtMs": 1720000000000_u64,
                    "endedAtMs": 1720000002000_u64,
                    "eventCount": 2,
                    "lastSequence": 2
                },
                "events": [
                    {
                        "eventId": "66666666-6666-4666-8666-666666666661",
                        "sessionId": "33333333-3333-4333-8333-333333333333",
                        "sequence": 1,
                        "requestId": "session-start-1",
                        "atMs": 1720000000000_u64,
                        "payload": { "type": "sessionStarted" }
                    },
                    {
                        "eventId": "66666666-6666-4666-8666-666666666662",
                        "sessionId": "33333333-3333-4333-8333-333333333333",
                        "sequence": 2,
                        "requestId": "session-end-1",
                        "atMs": 1720000002000_u64,
                        "payload": {
                            "type": "sessionEnded",
                            "outcome": "completed",
                            "reason": "CLI integration fixture"
                        }
                    }
                ]
            }
        });
        fs::write(path, serde_json::to_vec(&source).expect("source JSON")).expect("source file");
        set_owner_only(path);
    }

    fn directory_names(path: &std::path::Path) -> Vec<std::ffi::OsString> {
        let mut names = fs::read_dir(path)
            .expect("directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
}
