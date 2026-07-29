#![cfg(unix)]

use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use devicerail_core::{
    CancellationReason, DeviceDriver, DeviceOperationResult, DeviceRuntime, DriverError,
    DriverOperationContext, DriverResult, EvidenceStore, ExecutionControl, ExecutionController,
    MemoryEventStore, OperationContext, SessionEventStore, StartSession, TimeoutScope, now_ms,
};
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_plugin_driver::{
    DiscoveryConfig, PLUGIN_ABI_SCHEMA, PLUGIN_ABI_VERSION, PLUGIN_MANIFEST_SCHEMA,
    PluginCapabilityDeclaration, PluginDescriptor, PluginDiscoveryError, PluginDriver, PluginFrame,
    PluginHello, PluginManifest, PluginManifestDevice, PluginManifestProtocol, PluginOperation,
    PluginRequest, PluginResponse, PluginResponseResult, discover_plugin_descriptors,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, DeviceId, DeviceInfo,
    Observation, Platform, ProtocolVersion, Viewport,
};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

const GATED_HEALTH_EXECUTABLE: &str = "gated-health-plugin";
const FIRST_HEALTH_STARTED: &str = ".first-health-started";
const RELEASE_FIRST_HEALTH: &str = ".release-first-health";
const SECOND_HEALTH_STARTED: &str = ".second-health-started";
const RELEASE_SECOND_HEALTH: &str = ".release-second-health";

struct HealthGate {
    first_started: PathBuf,
    release_first: PathBuf,
    second_started: PathBuf,
    release_second: PathBuf,
}

impl HealthGate {
    fn new(root: &std::path::Path) -> Self {
        Self {
            first_started: root.join(FIRST_HEALTH_STARTED),
            release_first: root.join(RELEASE_FIRST_HEALTH),
            second_started: root.join(SECOND_HEALTH_STARTED),
            release_second: root.join(RELEASE_SECOND_HEALTH),
        }
    }

    fn release_first(&self) {
        fs::write(&self.release_first, b"release").expect("release first gated health request");
    }

    fn release_second(&self) {
        fs::write(&self.release_second, b"release").expect("release second gated health request");
    }
}

struct InstalledFixtureDriver {
    id: DeviceId,
    descriptor: PluginDescriptor,
    inner: tokio::sync::OnceCell<PluginDriver>,
    _installation: TempDir,
}

impl InstalledFixtureDriver {
    async fn inner(&self, control: &ExecutionControl) -> DriverResult<&PluginDriver> {
        self.inner
            .get_or_try_init(|| PluginDriver::load(self.descriptor.clone(), control))
            .await
    }
}

#[async_trait]
impl DeviceDriver for InstalledFixtureDriver {
    fn id(&self) -> &DeviceId {
        &self.id
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        self.inner(control).await?.connect(control).await
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        match self.inner.get() {
            Some(inner) => inner.disconnect(control).await,
            None => Ok(()),
        }
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        self.inner(control).await?.capabilities(control).await
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        self.inner(control).await?.health_check(control).await
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.inner.get().map_or_else(
            || {
                self.descriptor
                    .manifest()
                    .capabilities
                    .iter()
                    .find(|capability| capability.name == name)
                    .map(|capability| capability.protection)
            },
            |inner| inner.action_protection(name),
        )
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        self.inner(context.control()).await?.observe(context).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        self.inner(context.control())
            .await?
            .execute(context, call)
            .await
    }
}

fn fixture_manifest() -> PluginManifest {
    PluginManifest {
        manifest_version: 1,
        abi_version: PLUGIN_ABI_VERSION,
        plugin_id: "fixture-plugin".to_owned(),
        plugin_version: "1.0.0".to_owned(),
        executable: fixture_executable_name(),
        protocol: PluginManifestProtocol {
            major: 1,
            min_minor: 0,
            max_minor: 10,
        },
        device: PluginManifestDevice {
            key: "fixture-device".to_owned(),
            name: "Fixture plugin device".to_owned(),
            platform: Platform::Other("fixture".to_owned()),
            os_version: Some("1".to_owned()),
        },
        capabilities: vec![
            PluginCapabilityDeclaration {
                name: "tap".to_owned(),
                protection: ActionProtection::Standard,
            },
            PluginCapabilityDeclaration {
                name: "inputSecret".to_owned(),
                protection: ActionProtection::Protected,
            },
            PluginCapabilityDeclaration {
                name: "wait".to_owned(),
                protection: ActionProtection::Standard,
            },
        ],
    }
}

fn fixture_executable_name() -> String {
    if cfg!(windows) {
        "fixture-plugin.exe".to_owned()
    } else {
        "fixture-plugin".to_owned()
    }
}

fn install_fixture(manifest: &PluginManifest) -> (TempDir, DiscoveryConfig) {
    let directory = TempDir::new().expect("temporary plugin directory");
    let executable = directory.path().join(&manifest.executable);
    fs::copy(env!("CARGO_BIN_EXE_devicerail-plugin-fixture"), &executable)
        .expect("copy fixture executable");
    set_mode(&executable, 0o700);
    let manifest_path = directory.path().join("fixture.devicerail-plugin.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(manifest).expect("serialize fixture manifest"),
    )
    .expect("write fixture manifest");
    set_mode(&manifest_path, 0o600);
    let config =
        DiscoveryConfig::new(vec![directory.path().to_path_buf()]).expect("valid discovery config");
    (directory, config)
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set fixture permissions");
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) {}

fn fixture_driver() -> InstalledFixtureDriver {
    let (installation, config) = install_fixture(&fixture_manifest());
    let descriptor = discover_plugin_descriptors(&config)
        .expect("discover fixture")
        .pop()
        .expect("fixture descriptor");
    let id = DeviceId::new(format!(
        "plugin:{}:{}",
        descriptor.manifest().plugin_id,
        descriptor.manifest().device.key
    ));
    InstalledFixtureDriver {
        id,
        descriptor,
        inner: tokio::sync::OnceCell::new(),
        _installation: installation,
    }
}

async fn gated_health_driver(
    command_timeout: Duration,
) -> (TempDir, Arc<PluginDriver>, HealthGate) {
    let mut manifest = fixture_manifest();
    manifest.executable = GATED_HEALTH_EXECUTABLE.to_owned();
    let (installation, config) = install_fixture(&manifest);
    let gate = HealthGate::new(installation.path());
    let config = config
        .with_command_timeout(command_timeout)
        .expect("valid gated fixture command timeout");
    let descriptor = discover_plugin_descriptors(&config)
        .expect("discover gated health fixture")
        .pop()
        .expect("gated health fixture descriptor");
    let driver = PluginDriver::load(descriptor, &ExecutionControl::unbounded())
        .await
        .expect("load gated health fixture");
    (installation, Arc::new(driver), gate)
}

async fn wait_for_health_delivery(marker: &std::path::Path, phase: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{phase} health request reaches the fixture"));
}

async fn wait_until_remaining_at_most(control: &ExecutionControl, threshold: Duration) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if control
                .remaining()
                .is_some_and(|remaining| remaining <= threshold)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("caller deadline approaches the controlled release point");
}

fn conformance_call(action: &ActionDefinition) -> Result<ActionCall, String> {
    let arguments = match action.name.as_str() {
        "tap" => json!({ "x": 0, "y": 0 }),
        "inputSecret" => json!({ "text": "conformance-secret" }),
        "wait" => json!({ "milliseconds": 1 }),
        name => return Err(format!("no fixture call for {name}")),
    };
    Ok(ActionCall {
        id: Uuid::new_v4(),
        name: action.name.clone(),
        arguments,
    })
}

fn evidence_store() -> Arc<dyn EvidenceStore> {
    let root: PathBuf = TempDir::new().expect("temporary evidence directory").keep();
    Arc::new(
        FileEvidenceStore::new(&root, FileEvidenceStoreConfig::default())
            .expect("fixture evidence Store"),
    )
}

devicerail_core::driver_conformance_test!(
    process_plugin_conforms_to_shared_driver_contract,
    fixture_driver,
    conformance_call,
    evidence_store(),
);

#[test]
fn manifest_and_abi_fixtures_validate_against_published_schemas() {
    let manifest_schema: serde_json::Value =
        serde_json::from_str(PLUGIN_MANIFEST_SCHEMA).expect("manifest schema JSON");
    let manifest = serde_json::to_value(fixture_manifest()).expect("manifest JSON");
    assert!(
        jsonschema::validator_for(&manifest_schema)
            .expect("manifest schema")
            .is_valid(&manifest)
    );

    let abi_schema: serde_json::Value =
        serde_json::from_str(PLUGIN_ABI_SCHEMA).expect("ABI schema JSON");
    let validator = jsonschema::validator_for(&abi_schema).expect("ABI schema");
    let request = PluginRequest::new(ProtocolVersion::new(1, 3), PluginOperation::Health);
    let request_id = request.request_id;
    assert!(validator.is_valid(&serde_json::to_value(request).expect("request JSON")));
    let response = PluginResponse::success(request_id, PluginResponseResult::Ack);
    assert!(validator.is_valid(&serde_json::to_value(response).expect("response JSON")));

    let protocol = ProtocolVersion::new(1, 4);
    let operations = [
        PluginOperation::Hello {
            plugin_id: "fixture-plugin".to_owned(),
        },
        PluginOperation::Health,
        PluginOperation::Connect,
        PluginOperation::Disconnect,
        PluginOperation::Observe {
            capture_screenshot: false,
        },
        PluginOperation::Execute {
            call_id: Uuid::nil(),
            name: "tap".to_owned(),
            arguments: json!({ "x": 0, "y": 0 }),
        },
    ];
    for operation in operations {
        let request = PluginRequest::new(protocol, operation);
        assert!(
            validator.is_valid(&serde_json::to_value(request).expect("operation request JSON"))
        );
    }
    let results = [
        PluginResponseResult::Hello {
            hello: PluginHello {
                plugin_id: "fixture-plugin".to_owned(),
                plugin_version: "1.0.0".to_owned(),
                protocol,
                device: fixture_manifest().device,
                capabilities: vec![ActionDefinition {
                    name: "tap".to_owned(),
                    description: "Tap".to_owned(),
                    input_schema: json!({ "type": "object" }),
                    protection: ActionProtection::Standard,
                }],
            },
        },
        PluginResponseResult::Ack,
        PluginResponseResult::Frame {
            frame: PluginFrame {
                viewport: Viewport {
                    width: 1,
                    height: 1,
                    scale_factor: 1.0,
                },
                screenshot_base64: None,
                metadata: Default::default(),
            },
        },
        PluginResponseResult::Action {
            output: json!({ "accepted": true }),
        },
    ];
    for result in results {
        let response = PluginResponse::success(Uuid::new_v4(), result);
        assert!(
            validator.is_valid(&serde_json::to_value(response).expect("operation response JSON"))
        );
    }
    let error = PluginResponse::failure(Uuid::new_v4(), "not_connected", true);
    assert!(validator.is_valid(&serde_json::to_value(error).expect("error response JSON")));

    let manifest_fixture: serde_json::Value =
        serde_json::from_str(include_str!("../protocol/fixtures/plugin-manifest.json"))
            .expect("manifest fixture JSON");
    assert!(
        jsonschema::validator_for(&manifest_schema)
            .expect("manifest fixture schema")
            .is_valid(&manifest_fixture)
    );
    let manifest: PluginManifest =
        serde_json::from_value(manifest_fixture).expect("typed manifest fixture");
    assert_eq!(manifest.plugin_id, "fixture-plugin");

    let request_fixture: serde_json::Value =
        serde_json::from_str(include_str!("../protocol/fixtures/health.request.json"))
            .expect("request fixture JSON");
    assert!(validator.is_valid(&request_fixture));
    let request: PluginRequest =
        serde_json::from_value(request_fixture).expect("typed request fixture");
    assert!(matches!(request.operation, PluginOperation::Health));

    let response_fixture: serde_json::Value =
        serde_json::from_str(include_str!("../protocol/fixtures/health.response.json"))
            .expect("response fixture JSON");
    assert!(validator.is_valid(&response_fixture));
    let response: PluginResponse =
        serde_json::from_value(response_fixture).expect("typed response fixture");
    assert!(matches!(response.result, Some(PluginResponseResult::Ack)));
}

#[test]
fn operation_debug_never_contains_protected_arguments() {
    const SECRET: &str = "PLUGIN-DEBUG-SECRET-SENTINEL";
    let operation = PluginOperation::Execute {
        call_id: Uuid::nil(),
        name: "inputSecret".to_owned(),
        arguments: json!({ "text": SECRET }),
    };
    assert!(!format!("{operation:?}").contains(SECRET));
    let response = PluginResponse::success(
        Uuid::nil(),
        PluginResponseResult::Action {
            output: json!({ "echo": SECRET }),
        },
    );
    assert!(!format!("{response:?}").contains(SECRET));
}

#[tokio::test(flavor = "multi_thread")]
async fn protected_output_and_observation_metadata_cannot_reflect_arguments() {
    const SECRET: &str = "PLUGIN-PROTECTED-OUTPUT-SENTINEL";
    let driver = Arc::new(fixture_driver());
    let events = Arc::new(MemoryEventStore::default());
    let runtime =
        DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store());
    runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect fixture");
    let start = StartSession::new(None, Some(driver.id().clone()), now_ms());
    let session_id = start.session_id.clone();
    events
        .start_session(start)
        .await
        .expect("start protected Session");
    let context = OperationContext::new(session_id.clone(), None);
    let result = runtime
        .execute(
            &context,
            ActionCall {
                id: Uuid::new_v4(),
                name: "inputSecret".to_owned(),
                arguments: json!({ "text": SECRET }),
            },
        )
        .await
        .expect("execute protected plugin Action");
    assert_eq!(result.output, json!({ "accepted": true }));
    for observation in [result.before.as_ref(), result.after.as_ref()]
        .into_iter()
        .flatten()
    {
        assert!(observation.screenshot.is_none());
        assert!(observation.metadata.is_empty());
    }
    let export = events
        .export_session(&session_id)
        .await
        .expect("export protected Session");
    assert!(
        !serde_json::to_string(&export)
            .expect("serialize export")
            .contains(SECRET)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn manifest_capabilities_must_match_the_negotiated_plugin() {
    let mut manifest = fixture_manifest();
    manifest.capabilities.pop();
    let (_installation, config) = install_fixture(&manifest);
    let descriptor = discover_plugin_descriptors(&config)
        .expect("manifest remains structurally valid")
        .pop()
        .expect("descriptor");
    let error = PluginDriver::load(descriptor, &ExecutionControl::unbounded())
        .await
        .expect_err("undeclared capability must fail negotiation");
    assert!(matches!(
        error,
        DriverError::Platform { code, retryable: false }
            if code == "plugin_capability_mismatch"
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn response_kind_mismatch_permanently_poisons_the_plugin_process() {
    let mut manifest = fixture_manifest();
    manifest.executable = if cfg!(windows) {
        "wrong-kind-plugin.exe".to_owned()
    } else {
        "wrong-kind-plugin".to_owned()
    };
    let (_installation, config) = install_fixture(&manifest);
    let descriptor = discover_plugin_descriptors(&config)
        .expect("discover wrong-kind fixture")
        .pop()
        .expect("fixture descriptor");
    let driver = PluginDriver::load(descriptor, &ExecutionControl::unbounded())
        .await
        .expect("hello remains valid");
    let first = driver
        .health_check(&ExecutionControl::unbounded())
        .await
        .expect_err("wrong response kind must fail");
    assert!(matches!(
        first,
        DriverError::Platform { code, retryable: false }
            if code == "plugin_response_kind_invalid"
    ));
    let second = driver
        .health_check(&ExecutionControl::unbounded())
        .await
        .expect_err("poisoned process must not restart");
    assert!(matches!(
        second,
        DriverError::Platform { code, retryable: false }
            if code == "plugin_process_unavailable"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_terminates_a_blocked_plugin_process() {
    let driver = Arc::new(fixture_driver());
    let events = Arc::new(MemoryEventStore::default());
    let runtime = Arc::new(DeviceRuntime::with_evidence(
        Arc::clone(&driver),
        Arc::clone(&events),
        evidence_store(),
    ));
    runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect fixture");
    let start = StartSession::new(None, Some(driver.id().clone()), now_ms());
    events
        .start_session(start.clone())
        .await
        .expect("start fixture Session");
    let (controller, control) = ExecutionController::new();
    let context = OperationContext::new(start.session_id, None).with_control(control);
    let started = Instant::now();
    let task = tokio::spawn(async move {
        runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "wait".to_owned(),
                    arguments: json!({ "milliseconds": 10_000 }),
                },
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(controller.cancel(CancellationReason::Requested));
    let error = task
        .await
        .expect("join cancellation task")
        .expect_err("cancelled plugin action must fail");
    assert_eq!(error.to_error_info().code, "request_cancelled");
    assert!(started.elapsed() < Duration::from_secs(3));
    let error = driver
        .health_check(&ExecutionControl::unbounded())
        .await
        .expect_err("an ambiguously cancelled process must stay poisoned");
    assert!(matches!(
        error,
        DriverError::Platform { code, retryable: false }
            if code == "plugin_process_unavailable"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_wait_is_deducted_from_the_caller_deadline_and_then_poisons() {
    let (_installation, driver, gate) = gated_health_driver(Duration::from_secs(10)).await;
    let first_driver = Arc::clone(&driver);
    let first = tokio::spawn(async move {
        first_driver
            .health_check(&ExecutionControl::unbounded())
            .await
    });
    wait_for_health_delivery(&gate.first_started, "first").await;

    let control = ExecutionControl::unbounded().with_timeout(3_000, TimeoutScope::Request);
    let initial_budget = control.remaining().expect("caller deadline");
    assert!(initial_budget > Duration::from_secs(2));
    let second_control = control.clone();
    let second_driver = Arc::clone(&driver);
    let second = tokio::spawn(async move { second_driver.health_check(&second_control).await });
    wait_until_remaining_at_most(&control, Duration::from_secs(1)).await;
    gate.release_first();
    tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .expect("released first health request is bounded")
        .expect("join first gated health request")
        .expect("first gated health request completes after release");
    wait_for_health_delivery(&gate.second_started, "second").await;
    assert!(
        control
            .remaining()
            .is_some_and(|remaining| remaining <= Duration::from_secs(1))
    );

    // The second gate is deliberately never released. The current transport
    // has at most one second left and must finish inside this bound. The old
    // implementation snapshotted `initial_budget` before the lock and started
    // that nearly three-second clock only now, so it cannot satisfy this check.
    let error = tokio::time::timeout(Duration::from_millis(1_500), second)
        .await
        .expect("queued request retains its original absolute deadline")
        .expect("join second gated health request")
        .expect_err("queued request must time out after delivery");
    assert!(matches!(error, DriverError::TimedOut));

    let error = driver
        .health_check(&ExecutionControl::unbounded())
        .await
        .expect_err("a deadline during a delivered exchange must poison the process");
    assert!(matches!(
        error,
        DriverError::Platform { code, retryable: false }
            if code == "plugin_process_unavailable"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caller_deadline_while_only_waiting_for_the_supervisor_does_not_poison() {
    let (_installation, driver, gate) = gated_health_driver(Duration::from_secs(10)).await;
    let first_driver = Arc::clone(&driver);
    let first = tokio::spawn(async move {
        first_driver
            .health_check(&ExecutionControl::unbounded())
            .await
    });
    wait_for_health_delivery(&gate.first_started, "first").await;

    let control = ExecutionControl::unbounded().with_timeout(200, TimeoutScope::Request);
    let error = driver
        .health_check(&control)
        .await
        .expect_err("deadline must stop a request that has not acquired the supervisor");
    assert!(matches!(error, DriverError::TimedOut));
    gate.release_second();
    gate.release_first();
    tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .expect("released first health request is bounded")
        .expect("join first gated health request")
        .expect("first request completes after release");
    tokio::time::timeout(
        Duration::from_secs(2),
        driver.health_check(&ExecutionControl::unbounded()),
    )
    .await
    .expect("released second health request is bounded")
    .expect("a request that timed out before delivery must not poison the process");
    wait_for_health_delivery(&gate.second_started, "second").await;
}

#[tokio::test]
async fn configured_transport_timeout_is_distinct_and_poisons_after_delivery() {
    let (_installation, driver, gate) = gated_health_driver(Duration::from_secs(1)).await;
    let health_driver = Arc::clone(&driver);
    let health = tokio::spawn(async move {
        health_driver
            .health_check(&ExecutionControl::unbounded())
            .await
    });
    wait_for_health_delivery(&gate.first_started, "first").await;
    let error = tokio::time::timeout(Duration::from_secs(2), health)
        .await
        .expect("configured transport timeout remains bounded")
        .expect("join transport-timeout health request")
        .expect_err("configured command timeout must stop the slow exchange");
    assert!(matches!(
        error,
        DriverError::Platform { code, retryable: true } if code == "plugin_timeout"
    ));

    let error = driver
        .health_check(&ExecutionControl::unbounded())
        .await
        .expect_err("configured timeout is ambiguous and must poison the process");
    assert!(matches!(
        error,
        DriverError::Platform { code, retryable: false }
            if code == "plugin_process_unavailable"
    ));
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_executables_and_writable_manifests() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let manifest = fixture_manifest();
    let (installation, config) = install_fixture(&manifest);
    let executable = installation.path().join(&manifest.executable);
    fs::remove_file(&executable).expect("remove copied executable");
    symlink(env!("CARGO_BIN_EXE_devicerail-plugin-fixture"), &executable)
        .expect("symlink executable");
    assert_eq!(
        discover_plugin_descriptors(&config).expect_err("symlink must fail"),
        PluginDiscoveryError::UnsafeExecutable
    );

    let manifest_path = installation.path().join("fixture.devicerail-plugin.json");
    fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o666))
        .expect("make manifest unsafe");
    assert_eq!(
        discover_plugin_descriptors(&config).expect_err("writable manifest must fail"),
        PluginDiscoveryError::InvalidManifest
    );
}

#[cfg(target_os = "macos")]
#[test]
fn discovery_rejects_extended_acl_on_directory_manifest_and_executable() {
    use std::process::Command;

    let add_acl = |path: &std::path::Path| {
        let status = Command::new("chmod")
            .args(["+a", "everyone allow read"])
            .arg(path)
            .status()
            .expect("chmod ACL");
        assert!(status.success());
    };

    let manifest = fixture_manifest();
    let (installation, config) = install_fixture(&manifest);
    add_acl(installation.path());
    assert_eq!(
        discover_plugin_descriptors(&config).expect_err("directory ACL"),
        PluginDiscoveryError::UnsafeDirectory
    );

    let (installation, config) = install_fixture(&manifest);
    add_acl(&installation.path().join("fixture.devicerail-plugin.json"));
    assert_eq!(
        discover_plugin_descriptors(&config).expect_err("manifest ACL"),
        PluginDiscoveryError::InvalidManifest
    );

    let (installation, config) = install_fixture(&manifest);
    add_acl(&installation.path().join(&manifest.executable));
    assert_eq!(
        discover_plugin_descriptors(&config).expect_err("executable ACL"),
        PluginDiscoveryError::UnsafeExecutable
    );
}

#[cfg(unix)]
#[tokio::test]
async fn executable_is_revalidated_immediately_before_spawn() {
    use std::os::unix::fs::PermissionsExt as _;

    let manifest = fixture_manifest();
    let (installation, config) = install_fixture(&manifest);
    let descriptor = discover_plugin_descriptors(&config)
        .expect("discover safe executable")
        .pop()
        .expect("descriptor");
    let executable = installation.path().join(&manifest.executable);
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o777))
        .expect("mutate executable after discovery");
    let error = PluginDriver::load(descriptor, &ExecutionControl::unbounded())
        .await
        .expect_err("changed executable must not spawn");
    assert!(matches!(
        error,
        DriverError::Platform { code, retryable: false }
            if code == "plugin_executable_changed"
    ));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn executable_extended_acl_is_revalidated_immediately_before_spawn() {
    use std::process::Command;

    let manifest = fixture_manifest();
    let (installation, config) = install_fixture(&manifest);
    let descriptor = discover_plugin_descriptors(&config)
        .expect("discover safe executable")
        .pop()
        .expect("descriptor");
    let executable = installation.path().join(&manifest.executable);
    let status = Command::new("chmod")
        .args(["+a", "everyone allow read"])
        .arg(&executable)
        .status()
        .expect("chmod ACL");
    assert!(status.success());
    let error = PluginDriver::load(descriptor, &ExecutionControl::unbounded())
        .await
        .expect_err("changed executable ACL must not spawn");
    assert!(matches!(
        error,
        DriverError::Platform { code, retryable: false }
            if code == "plugin_executable_changed"
    ));
}

#[test]
fn incompatible_abi_fails_before_any_process_is_started() {
    let mut manifest = fixture_manifest();
    manifest.abi_version = PLUGIN_ABI_VERSION + 1;
    let (_installation, config) = install_fixture(&manifest);
    assert_eq!(
        discover_plugin_descriptors(&config).expect_err("ABI mismatch must fail"),
        PluginDiscoveryError::AbiIncompatible
    );
}
