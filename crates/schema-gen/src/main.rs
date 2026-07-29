use std::{
    collections::BTreeMap,
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

use devicerail_protocol::{
    PROTOCOL_VERSION,
    schema::{JSON_SCHEMA_DRAFT, SCHEMA_DIRECTORY, SCHEMA_SET_VERSION, SchemaDocument, documents},
};
use serde_json::json;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Write,
    Check,
}

fn main() -> ExitCode {
    match run(env::args().skip(1)) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("schema generation failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl IntoIterator<Item = String>) -> Result<String> {
    let mode = parse_mode(args)?;
    let output = default_output_directory();
    let expected = expected_files()?;

    match mode {
        Mode::Write => {
            prepare_default_output_directory(&output)?;
            write_schema_set(&output, &expected)?;
            Ok(format!(
                "wrote {} protocol schemas to {}",
                expected.len() - 1,
                output.display()
            ))
        }
        Mode::Check => {
            if fs::symlink_metadata(&output).is_ok() {
                ensure_default_output_is_inside_repository(&output)?;
            }
            check_schema_set(&output, &expected)?;
            Ok(format!(
                "protocol schemas are current in {}",
                output.display()
            ))
        }
    }
}

fn parse_mode(args: impl IntoIterator<Item = String>) -> Result<Mode> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Ok(Mode::Write),
        [command] if command == "write" => Ok(Mode::Write),
        [flag] if flag == "--check" => Ok(Mode::Check),
        _ => Err(format!(
            "usage: devicerail-schema-gen [write|--check], received: {}",
            args.join(" ")
        )
        .into()),
    }
}

fn default_output_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("schema-gen crate is below the repository root")
        .join("protocol/schema")
        .join(SCHEMA_DIRECTORY)
}

fn expected_files() -> Result<BTreeMap<String, Vec<u8>>> {
    let documents = documents();
    let mut files = BTreeMap::new();
    for document in &documents {
        files.insert(
            document.file_name.to_owned(),
            pretty_json_bytes(&document.schema)?,
        );
    }
    files.insert(
        "manifest.json".to_owned(),
        pretty_json_bytes(&manifest(&documents))?,
    );
    Ok(files)
}

fn manifest(documents: &[SchemaDocument]) -> serde_json::Value {
    json!({
        "schemaSetVersion": SCHEMA_SET_VERSION,
        "protocolVersion": {
            "major": PROTOCOL_VERSION.major,
            "minor": PROTOCOL_VERSION.minor,
        },
        "draft": JSON_SCHEMA_DRAFT,
        "documents": documents
            .iter()
            .map(|document| json!({
                "name": document.name,
                "file": document.file_name,
                "id": document.schema_id,
            }))
            .collect::<Vec<_>>(),
    })
}

fn pretty_json_bytes(value: &serde_json::Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_schema_set(output: &Path, expected: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    fs::create_dir_all(output)?;
    require_directory(output)?;

    for entry in fs::read_dir(output)? {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if !expected.contains_key(&file_name) {
            return Err(format!(
                "refusing to write schemas while an unknown entry exists: {}",
                entry.path().display()
            )
            .into());
        }
        require_regular_file(&entry.path())?;
    }

    for (file_name, contents) in expected {
        let path = output.join(file_name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                require_regular_file(&path)?;
                if fs::read(&path)? != *contents {
                    atomic_write(&path, contents)?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                atomic_write(&path, contents)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn check_schema_set(output: &Path, expected: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    match fs::symlink_metadata(output) {
        Ok(_) => require_directory(output)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut problems = Vec::new();
    for (file_name, contents) in expected {
        let path = output.join(file_name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                require_regular_file(&path)?;
                if fs::read(&path)? != *contents {
                    problems.push(format!("changed: {}", path.display()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                problems.push(format!("missing: {}", path.display()));
            }
            Err(error) => return Err(error.into()),
        }
    }

    match fs::read_dir(output) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let file_name = entry.file_name().to_string_lossy().into_owned();
                if !expected.contains_key(&file_name) {
                    problems.push(format!("stale: {}", entry.path().display()));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    if problems.is_empty() {
        Ok(())
    } else {
        problems.sort();
        Err(format!(
            "checked-in protocol schemas are out of date:\n{}\nrun `cargo run -p devicerail-schema-gen -- write`",
            problems.join("\n")
        )
        .into())
    }
}

fn ensure_default_output_is_inside_repository(output: &Path) -> Result<()> {
    require_directory(output)?;
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let canonical_output = output.canonicalize()?;
    if !canonical_output.starts_with(&repository_root) {
        return Err(format!(
            "schema output resolves outside the repository: {}",
            canonical_output.display()
        )
        .into());
    }
    Ok(())
}

fn prepare_default_output_directory(output: &Path) -> Result<()> {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("schema-gen crate is below the repository root");
    let expected = repository_root
        .join("protocol/schema")
        .join(SCHEMA_DIRECTORY);
    if output != expected {
        return Err(format!("unexpected schema output path: {}", output.display()).into());
    }

    let prepared =
        prepare_directory_chain(repository_root, &["protocol", "schema", SCHEMA_DIRECTORY])?;
    debug_assert_eq!(prepared, expected);
    ensure_default_output_is_inside_repository(output)
}

fn prepare_directory_chain(root: &Path, components: &[&str]) -> Result<PathBuf> {
    require_directory(root)?;
    let mut current = root.to_path_buf();
    for component in components {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "schema output ancestors must be real directories: {}",
                        current.display()
                    )
                    .into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current)
}

fn require_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("schema output must be a real directory: {}", path.display()).into());
    }
    Ok(())
}

fn require_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "schema output entries must be regular files, not links or directories: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("schema path has no UTF-8 file name: {}", path.display()))?;
    let temporary = path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    if let Err(error) = file.write_all(contents).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    drop(file);

    #[cfg(windows)]
    if fs::symlink_metadata(path).is_ok() {
        fs::remove_file(path)?;
    }

    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use devicerail_protocol::schema::documents;
    use serde_json::{Value, json};

    #[cfg(unix)]
    use super::prepare_directory_chain;
    use super::{check_schema_set, default_output_directory, expected_files, write_schema_set};

    #[test]
    fn every_document_is_a_valid_draft_2020_12_schema() {
        for document in documents() {
            jsonschema::draft202012::meta::validate(&document.schema).unwrap_or_else(|error| {
                panic!("{} is not a valid schema: {error}", document.file_name)
            });
        }
    }

    #[test]
    fn integer_formats_have_explicit_cross_language_bounds() {
        for document in documents() {
            assert_integer_bounds(&document.schema, "$", document.file_name);
        }
    }

    #[test]
    fn method_golden_examples_validate_against_typed_schemas() {
        let schemas = documents()
            .into_iter()
            .map(|document| (document.file_name, document.schema))
            .collect::<std::collections::BTreeMap<_, _>>();
        validate_fixture(
            &schemas["system-hello-request.schema.json"],
            include_str!("../../protocol/fixtures/system-hello-v1.request.json"),
        );
        validate_fixture(
            &schemas["system-hello-response.schema.json"],
            include_str!("../../protocol/fixtures/system-hello-v1.response.json"),
        );
        validate_fixture(
            &schemas["device-select-request.schema.json"],
            include_str!("../../protocol/fixtures/rpc/device-select-v1.request.json"),
        );
        validate_fixture(
            &schemas["device-select-response.schema.json"],
            include_str!("../../protocol/fixtures/rpc/device-select-v1.response.json"),
        );
        validate_fixture(
            &schemas["devices-list-request.schema.json"],
            include_str!("../../protocol/fixtures/rpc/devices-list-v1.request.json"),
        );
        validate_fixture(
            &schemas["devices-list-response.schema.json"],
            include_str!("../../protocol/fixtures/rpc/devices-list-v1.response.json"),
        );

        let wrong_method = json!({
            "jsonrpc": "2.0",
            "id": "hello-1",
            "method": "system.version",
            "params": {}
        });
        let validator = jsonschema::draft202012::new(&schemas["system-hello-request.schema.json"])
            .expect("compile request schema");
        assert!(!validator.is_valid(&wrong_method));
    }

    #[test]
    fn rpc_envelope_schemas_preserve_runtime_strictness() {
        let schemas = documents()
            .into_iter()
            .map(|document| (document.file_name, document.schema))
            .collect::<std::collections::BTreeMap<_, _>>();
        let request = jsonschema::draft202012::new(&schemas["rpc-request.schema.json"])
            .expect("compile request schema");
        assert!(request.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "device.connect",
            "timeoutMs": 9_007_199_254_740_991_u64
        })));
        assert!(!request.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "device.connect",
            "timeoutMs": 0
        })));
        assert!(!request.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "device.connect",
            "timeoutMs": null
        })));
        assert!(!request.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "device.connect",
            "timeoutMs": 9_007_199_254_740_992_u64
        })));
        assert!(!request.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "device.connect",
            "params": null
        })));
        assert!(!request.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "device.connect",
            "deadlineMs": 100
        })));

        let execute = jsonschema::draft202012::new(&schemas["device-execute-request.schema.json"])
            .expect("compile execute schema");
        assert!(!execute.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "execute-1",
            "method": "device.execute",
            "params": {
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "tap",
                "unknown": true
            }
        })));

        let hello = jsonschema::draft202012::new(&schemas["system-hello-request.schema.json"])
            .expect("compile hello schema");
        assert!(!hello.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "hello-timeout",
            "method": "system.hello",
            "timeoutMs": 100,
            "params": {
                "client": { "name": "client", "version": "0.1.0" },
                "protocol": {
                    "ranges": [{ "major": 1, "minMinor": 0, "maxMinor": 1 }]
                },
                "features": { "required": [], "optional": ["request.control.v1"] }
            }
        })));

        let cancel = jsonschema::draft202012::new(&schemas["request-cancel-request.schema.json"])
            .expect("compile cancel schema");
        assert!(!cancel.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "cancel-timeout",
            "method": "request.cancel",
            "timeoutMs": 100,
            "params": { "requestId": "target" }
        })));

        let list = jsonschema::draft202012::new(&schemas["devices-list-request.schema.json"])
            .expect("compile devices.list schema");
        assert!(list.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "list-1",
            "method": "devices.list"
        })));
        assert!(list.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "list-empty-object",
            "method": "devices.list",
            "params": {}
        })));
        assert!(list.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "list-empty-array",
            "method": "devices.list",
            "params": []
        })));
        assert!(!list.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "list-null",
            "method": "devices.list",
            "params": null
        })));
        assert!(!list.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "list-timeout",
            "method": "devices.list",
            "timeoutMs": 100
        })));
        assert!(!list.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "list-params",
            "method": "devices.list",
            "params": { "unknown": true }
        })));

        let select = jsonschema::draft202012::new(&schemas["device-select-request.schema.json"])
            .expect("compile device.select schema");
        assert!(select.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "select-1",
            "method": "device.select",
            "params": { "deviceId": "mock-1" }
        })));
        assert!(!select.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "select-missing",
            "method": "device.select"
        })));
        assert!(!select.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "select-unknown",
            "method": "device.select",
            "params": { "deviceId": "mock-1", "device": "mock-2" }
        })));
        assert!(!select.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": "select-timeout",
            "method": "device.select",
            "timeoutMs": 100,
            "params": { "deviceId": "mock-1" }
        })));

        let response = jsonschema::draft202012::new(&schemas["rpc-response.schema.json"])
            .expect("compile response schema");
        assert!(!response.is_valid(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {},
            "error": {
                "code": -32603,
                "message": "internal",
                "data": {
                    "code": "internal_error",
                    "message": "internal",
                    "retryable": false,
                    "details": null
                }
            }
        })));
    }

    #[test]
    fn every_manifest_fixture_validates_against_its_generated_schema() {
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("resolve repository root");
        let fixtures_root = repository_root.join("crates/protocol/fixtures");
        let manifest: Value = serde_json::from_slice(
            &fs::read(fixtures_root.join("manifest.json")).expect("read fixture manifest"),
        )
        .expect("parse fixture manifest");

        for entry in manifest["fixtures"].as_array().expect("fixture entries") {
            let id = entry["id"].as_str().expect("fixture id");
            let fixture: Value = serde_json::from_slice(
                &fs::read(fixtures_root.join(entry["path"].as_str().expect("fixture path")))
                    .unwrap_or_else(|error| panic!("read fixture {id}: {error}")),
            )
            .unwrap_or_else(|error| panic!("parse fixture {id}: {error}"));
            let schema: Value = serde_json::from_slice(
                &fs::read(repository_root.join(entry["schema"].as_str().expect("schema path")))
                    .unwrap_or_else(|error| panic!("read schema for {id}: {error}")),
            )
            .unwrap_or_else(|error| panic!("parse schema for {id}: {error}"));

            let validator = jsonschema::draft202012::new(&schema)
                .unwrap_or_else(|error| panic!("compile schema for {id}: {error}"));
            if !validator.is_valid(&fixture) {
                let errors = validator
                    .iter_errors(&fixture)
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>();
                panic!("fixture {id} did not match its schema: {errors:#?}");
            }
        }
    }

    #[test]
    fn checked_in_schema_set_is_current() {
        check_schema_set(
            &default_output_directory(),
            &expected_files().expect("generate schemas"),
        )
        .expect("run `cargo run -p devicerail-schema-gen -- write`");
    }

    #[test]
    fn check_reports_changed_and_stale_files() {
        let directory = test_directory();
        let expected = expected_files().expect("generate schemas");
        write_schema_set(&directory, &expected).expect("write test schemas");
        fs::write(directory.join("rpc-id.schema.json"), b"{}\n").expect("corrupt schema");
        fs::write(directory.join("junk.json"), b"{}\n").expect("add stale file");
        fs::create_dir(directory.join("stale-directory")).expect("add stale directory");

        let error = check_schema_set(&directory, &expected).expect_err("check must fail");
        let message = error.to_string();
        assert!(message.contains("changed:"));
        assert!(message.contains("stale:"));

        fs::remove_dir_all(directory).expect("remove test schemas");
    }

    #[test]
    fn write_refuses_unknown_entries() {
        let directory = test_directory();
        let expected = expected_files().expect("generate schemas");
        write_schema_set(&directory, &expected).expect("write test schemas");
        fs::write(directory.join("notes.txt"), b"not generated\n").expect("add unknown file");

        let error =
            write_schema_set(&directory, &expected).expect_err("write must be conservative");
        assert!(error.to_string().contains("unknown entry"));
        fs::remove_dir_all(directory).expect("remove test schemas");
    }

    #[cfg(unix)]
    #[test]
    fn write_rejects_expected_file_symlinks_without_touching_the_target() {
        use std::os::unix::fs::symlink;

        let directory = test_directory();
        let expected = expected_files().expect("generate schemas");
        write_schema_set(&directory, &expected).expect("write test schemas");
        let schema_path = directory.join("rpc-id.schema.json");
        fs::remove_file(&schema_path).expect("replace generated schema");
        let external = directory.with_extension("external-target");
        fs::write(&external, b"must remain unchanged\n").expect("write external target");
        symlink(&external, &schema_path).expect("create schema symlink");

        let error = write_schema_set(&directory, &expected).expect_err("symlink must be rejected");
        assert!(error.to_string().contains("not links"));
        assert_eq!(
            fs::read(&external).expect("read external target"),
            b"must remain unchanged\n"
        );

        fs::remove_dir_all(directory).expect("remove test schemas");
        fs::remove_file(external).expect("remove external target");
    }

    #[cfg(unix)]
    #[test]
    fn directory_preparation_rejects_symlinked_ancestors() {
        use std::os::unix::fs::symlink;

        let repository = test_directory();
        let external = repository.with_extension("external-directory");
        fs::create_dir_all(&repository).expect("create fake repository");
        fs::create_dir_all(&external).expect("create external directory");
        symlink(&external, repository.join("protocol")).expect("create ancestor symlink");

        let error = prepare_directory_chain(&repository, &["protocol", "schema", "v1"])
            .expect_err("ancestor symlinks must be rejected");
        assert!(error.to_string().contains("real directories"));
        assert!(!external.join("schema").exists());

        fs::remove_file(repository.join("protocol")).expect("remove ancestor symlink");
        fs::remove_dir(repository).expect("remove fake repository");
        fs::remove_dir(external).expect("remove external directory");
    }

    fn validate_fixture(schema: &Value, fixture: &str) {
        let fixture: Value = serde_json::from_str(fixture).expect("parse fixture");
        let validator = jsonschema::draft202012::new(schema).expect("compile schema");
        if !validator.is_valid(&fixture) {
            let errors = validator
                .iter_errors(&fixture)
                .map(|error| error.to_string())
                .collect::<Vec<_>>();
            panic!("fixture did not match schema: {errors:#?}");
        }
    }

    fn assert_integer_bounds(value: &Value, path: &str, document: &str) {
        match value {
            Value::Object(object) => {
                if let Some(format) = object.get("format").and_then(Value::as_str) {
                    let expected = match format {
                        "uint16" => Some((0_i64, 65_535_i64)),
                        "uint32" => Some((0_i64, 4_294_967_295_i64)),
                        "uint64" => Some((0_i64, 9_007_199_254_740_991_i64)),
                        "int32" => Some((-2_147_483_648_i64, 2_147_483_647_i64)),
                        _ => None,
                    };
                    if let Some((minimum, maximum)) = expected {
                        let has_integer_type = match object.get("type") {
                            Some(Value::String(value)) => value == "integer",
                            Some(Value::Array(values)) => {
                                values.iter().any(|value| value == "integer")
                                    && values
                                        .iter()
                                        .all(|value| value == "integer" || value == "null")
                            }
                            _ => false,
                        };
                        assert!(
                            has_integer_type,
                            "{document} {path} has {format} without integer type"
                        );
                        let actual_minimum = object.get("minimum").and_then(Value::as_i64);
                        assert!(
                            actual_minimum.is_some_and(|value| value >= minimum),
                            "{document} {path} has an unsafe {format} minimum"
                        );
                        assert!(
                            object
                                .get("maximum")
                                .and_then(Value::as_i64)
                                .is_some_and(|value| value <= maximum),
                            "{document} {path} has an unsafe {format} maximum"
                        );
                    }
                }

                for (key, child) in object {
                    assert_integer_bounds(child, &format!("{path}/{key}"), document);
                }
            }
            Value::Array(values) => {
                for (index, child) in values.iter().enumerate() {
                    assert_integer_bounds(child, &format!("{path}/{index}"), document);
                }
            }
            _ => {}
        }
    }

    fn test_directory() -> std::path::PathBuf {
        static NEXT_DIRECTORY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "devicerail-schema-gen-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        if Path::new(&path).exists() {
            fs::remove_dir_all(&path).expect("remove old test directory");
        }
        path
    }
}
