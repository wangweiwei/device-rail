use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::Arc,
};

use devicerail_core::ExecutionControl;

use crate::{
    AdbCommand, AdbCommandRunner, AdbDeviceState, AdbDiscoveryIssue, AdbDiscoveryReport,
    AdbOperation, AdbSerial, AndroidAdbError, AndroidAdbResult, AndroidDevice, AndroidDeviceConfig,
    AndroidDriver, DiscoveredAndroidDevice, SystemAdbCommandRunner, SystemAdbConfig,
};

const DEVICES_HEADER: &str = "List of devices attached";

/// Host-level Android discovery backed by an injectable, shell-free adb runner.
#[derive(Clone)]
pub struct AndroidAdb {
    runner: Arc<dyn AdbCommandRunner>,
}

impl AndroidAdb {
    pub fn system(config: SystemAdbConfig) -> AndroidAdbResult<Self> {
        Ok(Self::with_runner(Arc::new(SystemAdbCommandRunner::new(
            config,
        )?)))
    }

    pub(crate) fn with_runner(runner: Arc<dyn AdbCommandRunner>) -> Self {
        Self { runner }
    }

    pub fn device(
        &self,
        descriptor: DiscoveredAndroidDevice,
        config: AndroidDeviceConfig,
    ) -> AndroidAdbResult<AndroidDevice> {
        AndroidDevice::new(descriptor, Arc::clone(&self.runner), config)
    }

    pub fn driver(
        &self,
        descriptor: DiscoveredAndroidDevice,
        config: AndroidDeviceConfig,
    ) -> AndroidAdbResult<AndroidDriver> {
        self.device(descriptor, config).map(AndroidDriver::new)
    }

    pub async fn discover(
        &self,
        control: &ExecutionControl,
    ) -> AndroidAdbResult<AdbDiscoveryReport> {
        let command = AdbCommand::host(AdbOperation::DevicesLong);
        let output = self.runner.run(command, control).await?;
        parse_devices_output(output.stdout_text()?)
    }
}

fn parse_devices_output(stdout: &str) -> AndroidAdbResult<AdbDiscoveryReport> {
    let mut found_header = false;
    let mut devices = BTreeMap::<AdbSerial, DiscoveredAndroidDevice>::new();
    let mut issues = Vec::new();

    for (index, raw_line) in stdout.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();

        if !found_header {
            if line.is_empty() || is_daemon_chatter(line) {
                continue;
            }
            if line == DEVICES_HEADER {
                found_header = true;
                continue;
            }
            return malformed(line_number, "expected the adb devices header", line);
        }

        if line.is_empty() {
            continue;
        }
        if line == DEVICES_HEADER {
            return malformed(line_number, "duplicate adb devices header", line);
        }

        match parse_device_line(line, line_number)? {
            ParsedDeviceLine::Device(device) => match devices.entry(device.serial.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(device);
                }
                Entry::Occupied(entry) => {
                    return Err(AndroidAdbError::DuplicateSerial(
                        entry.key().as_str().to_owned(),
                    ));
                }
            },
            ParsedDeviceLine::Issue(issue) => issues.push(issue),
        }
    }

    if !found_header {
        return Err(AndroidAdbError::MalformedDevicesOutput(
            "missing `List of devices attached` header".to_owned(),
        ));
    }

    Ok(AdbDiscoveryReport {
        devices: devices.into_values().collect(),
        issues,
    })
}

enum ParsedDeviceLine {
    Device(DiscoveredAndroidDevice),
    Issue(AdbDiscoveryIssue),
}

fn parse_device_line(line: &str, line_number: usize) -> AndroidAdbResult<ParsedDeviceLine> {
    let mut fields = line.split_whitespace();
    let serial_text = fields
        .next()
        .ok_or_else(|| malformed_error(line_number, "missing serial", line))?;
    let state_text = fields
        .next()
        .ok_or_else(|| malformed_error(line_number, "missing device state", line))?;
    let remainder = fields.collect::<Vec<_>>();
    let remainder_text = remainder.join(" ");

    // Some adb/udev combinations omit even the usual question-mark serial.
    if serial_text == "no" && state_text == "permissions" {
        return Ok(unstable_no_permissions_issue(line));
    }

    let state = AdbDeviceState::parse(state_text, &remainder_text);
    if state == AdbDeviceState::NoPermissions && !is_stable_serial(serial_text) {
        return Ok(unstable_no_permissions_issue(line));
    }

    let serial = AdbSerial::parse(serial_text)?;
    let mut product = None;
    let mut model = None;
    let mut device = None;
    let mut transport_id = None;
    let mut extensions = BTreeMap::new();

    // `no permissions (...)` is a diagnostic sentence, not long-format
    // metadata. Stable serials still remain discoverable so lifecycle code can
    // return the precise host-permission error for that device.
    if state != AdbDeviceState::NoPermissions {
        let mut seen_keys = BTreeMap::<&str, ()>::new();
        for field in remainder {
            let (key, value) = field.split_once(':').ok_or_else(|| {
                malformed_error(
                    line_number,
                    "metadata must use non-empty `key:value` fields",
                    line,
                )
            })?;
            if key.is_empty() {
                return malformed(
                    line_number,
                    "metadata must use non-empty `key:value` fields",
                    line,
                );
            }
            if seen_keys.insert(key, ()).is_some() {
                return malformed(
                    line_number,
                    &format!("duplicate metadata key `{key}`"),
                    line,
                );
            }

            match key {
                "product" => product = Some(metadata_value(value, line_number, line)?),
                "model" => model = Some(metadata_value(value, line_number, line)?),
                "device" => device = Some(metadata_value(value, line_number, line)?),
                "transport_id" => {
                    if value.is_empty()
                        || !value.bytes().all(|character| character.is_ascii_digit())
                    {
                        return Err(AndroidAdbError::InvalidValue {
                            field: "transport_id",
                            value: value.to_owned(),
                        });
                    }
                    transport_id =
                        Some(
                            value
                                .parse::<u64>()
                                .map_err(|_| AndroidAdbError::InvalidValue {
                                    field: "transport_id",
                                    value: value.to_owned(),
                                })?,
                        );
                }
                _ => {
                    extensions.insert(key.to_owned(), metadata_value(value, line_number, line)?);
                }
            }
        }
    }

    Ok(ParsedDeviceLine::Device(DiscoveredAndroidDevice {
        serial,
        state,
        product,
        model,
        device,
        transport_id,
        extensions,
    }))
}

fn metadata_value(value: &str, line_number: usize, line: &str) -> AndroidAdbResult<String> {
    if value.is_empty() {
        malformed(
            line_number,
            "metadata must use non-empty `key:value` fields",
            line,
        )
    } else {
        Ok(value.to_owned())
    }
}

fn is_daemon_chatter(line: &str) -> bool {
    line.starts_with("* daemon ")
}

fn is_stable_serial(value: &str) -> bool {
    !value.is_empty() && !value.chars().all(|character| character == '?')
}

fn unstable_no_permissions_issue(line: &str) -> ParsedDeviceLine {
    ParsedDeviceLine::Issue(AdbDiscoveryIssue {
        line: line.to_owned(),
        message: "adb reported a no-permissions device without a stable serial".to_owned(),
    })
}

fn malformed<T>(line_number: usize, reason: &str, line: &str) -> AndroidAdbResult<T> {
    Err(malformed_error(line_number, reason, line))
}

fn malformed_error(line_number: usize, reason: &str, line: &str) -> AndroidAdbError {
    AndroidAdbError::MalformedDevicesOutput(format!("line {line_number}: {reason}: {line:?}"))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use devicerail_core::ExecutionControl;

    use super::{AndroidAdb, parse_devices_output};
    use crate::{
        AdbCommand, AdbCommandOutput, AdbCommandRunner, AdbDeviceState, AdbOperation,
        AndroidAdbError, AndroidAdbResult,
    };

    struct RecordingErrorRunner {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AdbCommandRunner for RecordingErrorRunner {
        async fn run(
            &self,
            command: AdbCommand,
            _control: &ExecutionControl,
        ) -> AndroidAdbResult<AdbCommandOutput> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert!(command.serial().is_none());
            assert_eq!(command.operation(), &AdbOperation::DevicesLong);
            Err(AndroidAdbError::ProcessFailed {
                operation: "devices_long",
                status: Some(23),
                stderr_tail: "fixture failure".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn discovery_uses_host_devices_long_and_propagates_runner_errors() {
        let runner = Arc::new(RecordingErrorRunner {
            calls: AtomicUsize::new(0),
        });
        let adb = AndroidAdb::with_runner(runner.clone());

        assert!(matches!(
            adb.discover(&ExecutionControl::unbounded()).await,
            Err(AndroidAdbError::ProcessFailed {
                operation: "devices_long",
                status: Some(23),
                stderr_tail,
            }) if stderr_tail == "fixture failure"
        ));
        assert_eq!(runner.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn parses_crlf_daemon_chatter_states_metadata_extensions_and_sorted_serials() {
        let output = concat!(
            "* daemon not running; starting now at tcp:5037\r\n",
            "* daemon started successfully\r\n",
            "List of devices attached\r\n",
            "z-last\toffline transport_id:7 usb:1-2\r\n",
            "a-first\tdevice product:bluejay model:Pixel_6a device:bluejay transport_id:2 features:cmd\r\n",
            "b-middle\tunauthorized usb:3-4 transport_id:3\r\n",
            "c-authorizing\tauthorizing\r\n",
            "d-recovery\trecovery\r\n",
            "e-sideload\tsideload\r\n",
            "f-bootloader\tbootloader\r\n",
            "g-future\tfuture_state custom:value\r\n",
        );

        let report = parse_devices_output(output).expect("parse discovery output");
        let serials = report
            .devices
            .iter()
            .map(|device| device.serial.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            serials,
            vec![
                "a-first",
                "b-middle",
                "c-authorizing",
                "d-recovery",
                "e-sideload",
                "f-bootloader",
                "g-future",
                "z-last",
            ]
        );
        assert!(report.issues.is_empty());

        let ready = &report.devices[0];
        assert_eq!(ready.state, AdbDeviceState::Ready);
        assert_eq!(ready.product.as_deref(), Some("bluejay"));
        assert_eq!(ready.model.as_deref(), Some("Pixel_6a"));
        assert_eq!(ready.device.as_deref(), Some("bluejay"));
        assert_eq!(ready.transport_id, Some(2));
        assert!(!ready.device_info().connected);
        assert_eq!(
            ready.extensions,
            BTreeMap::from([("features".to_owned(), "cmd".to_owned())])
        );
        assert_eq!(report.devices[1].state, AdbDeviceState::Unauthorized);
        assert_eq!(report.devices[2].state, AdbDeviceState::Authorizing);
        assert_eq!(report.devices[3].state, AdbDeviceState::Recovery);
        assert_eq!(report.devices[4].state, AdbDeviceState::Sideload);
        assert_eq!(report.devices[5].state, AdbDeviceState::Bootloader);
        assert_eq!(
            report.devices[6].state,
            AdbDeviceState::Unknown("future_state".to_owned())
        );
        assert_eq!(report.devices[7].state, AdbDeviceState::Offline);
        assert_eq!(
            report.devices[7].extensions,
            BTreeMap::from([("usb".to_owned(), "1-2".to_owned())])
        );
    }

    #[test]
    fn accepts_empty_device_list() {
        let report = parse_devices_output("List of devices attached\n\n")
            .expect("parse empty discovery output");
        assert!(report.devices.is_empty());
        assert!(report.issues.is_empty());
    }

    #[test]
    fn reports_unstable_no_permissions_entries_without_fabricating_devices() {
        let output = concat!(
            "List of devices attached\n",
            "????????????\tno permissions (user in plugdev group); see [https://developer.android.com/tools/device.html]\n",
            "no permissions (serial unavailable)\n",
            "stable-serial\tno permissions (access denied)\n",
        );
        let report = parse_devices_output(output).expect("parse no-permissions entries");

        assert_eq!(report.devices.len(), 1);
        assert_eq!(report.devices[0].serial.as_str(), "stable-serial");
        assert_eq!(report.devices[0].state, AdbDeviceState::NoPermissions);
        assert_eq!(report.issues.len(), 2);
        assert!(report.issues[0].line.starts_with("????????????"));
        assert_eq!(report.issues[1].line, "no permissions (serial unavailable)");
    }

    #[test]
    fn requires_header_and_rejects_unexpected_preamble() {
        for output in [
            "",
            "serial\tdevice\n",
            "adb server version mismatch\nList of devices attached\n",
        ] {
            assert!(matches!(
                parse_devices_output(output),
                Err(AndroidAdbError::MalformedDevicesOutput(_))
            ));
        }
    }

    #[test]
    fn rejects_malformed_lines_and_duplicate_metadata() {
        for line in [
            "serial-only",
            "serial device not-metadata",
            "serial device model:",
            "serial device model:first model:second",
            "List of devices attached",
        ] {
            let output = format!("List of devices attached\n{line}\n");
            assert!(matches!(
                parse_devices_output(&output),
                Err(AndroidAdbError::MalformedDevicesOutput(_))
            ));
        }
    }

    #[test]
    fn rejects_duplicate_serials_exactly_but_keeps_case_distinct_serials() {
        let duplicates = concat!(
            "List of devices attached\n",
            "serial device\n",
            "serial offline\n",
        );
        assert!(matches!(
            parse_devices_output(duplicates),
            Err(AndroidAdbError::DuplicateSerial(serial)) if serial == "serial"
        ));

        let distinct = concat!(
            "List of devices attached\n",
            "Serial device\n",
            "serial device\n",
        );
        let report = parse_devices_output(distinct).expect("case-distinct serials");
        assert_eq!(report.devices.len(), 2);
        assert_eq!(report.devices[0].serial.as_str(), "Serial");
        assert_eq!(report.devices[1].serial.as_str(), "serial");
    }

    #[test]
    fn rejects_invalid_transport_id_explicitly() {
        for value in ["", "not-a-number", "-1", "18446744073709551616"] {
            let output = format!("List of devices attached\nserial device transport_id:{value}\n");
            assert!(matches!(
                parse_devices_output(&output),
                Err(AndroidAdbError::InvalidValue {
                    field: "transport_id",
                    value: rejected,
                }) if rejected == value
            ));
        }
    }
}
