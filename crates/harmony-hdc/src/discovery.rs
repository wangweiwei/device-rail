use std::{collections::BTreeMap, sync::Arc};

use devicerail_core::ExecutionControl;

use crate::{
    DiscoveredHarmonyDevice, HarmonyDiscoveryReport, HarmonyHdcDriver, HarmonyHdcError,
    HarmonyHdcResult, HdcCommand, HdcCommandRunner, HdcOperation, HdcTarget, HdcTargetState,
    SystemHdcCommandRunner, SystemHdcConfig,
};

#[derive(Clone)]
pub struct HarmonyHdc {
    runner: Arc<dyn HdcCommandRunner>,
}

impl HarmonyHdc {
    pub fn system(config: SystemHdcConfig) -> HarmonyHdcResult<Self> {
        Ok(Self {
            runner: Arc::new(SystemHdcCommandRunner::new(config)?),
        })
    }

    pub fn with_runner(runner: Arc<dyn HdcCommandRunner>) -> Self {
        Self { runner }
    }

    pub async fn discover(
        &self,
        control: &ExecutionControl,
    ) -> HarmonyHdcResult<HarmonyDiscoveryReport> {
        let command = HdcCommand::host(HdcOperation::ListTargetsVerbose)?;
        let output = self.runner.run(command, control).await?;
        if !output.stderr_text("list_targets")?.trim().is_empty() {
            return Err(HarmonyHdcError::InvalidOutput {
                operation: "list_targets",
            });
        }
        parse_targets(output.stdout_text("list_targets")?)
    }

    pub fn driver(&self, descriptor: DiscoveredHarmonyDevice) -> HarmonyHdcDriver {
        HarmonyHdcDriver::new(descriptor, Arc::clone(&self.runner))
    }
}

fn parse_targets(stdout: &str) -> HarmonyHdcResult<HarmonyDiscoveryReport> {
    let mut devices = BTreeMap::<HdcTarget, DiscoveredHarmonyDevice>::new();
    let mut ignored_diagnostics = Vec::new();

    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.eq_ignore_ascii_case("[empty]") {
            continue;
        }
        if is_diagnostic(line) || is_header(line) {
            ignored_diagnostics.push(line.to_owned());
            continue;
        }

        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.is_empty() {
            continue;
        }
        let target = HdcTarget::parse(fields[0])?;
        let has_state = fields
            .get(1)
            .is_some_and(|field| !field.contains(':') && !field.contains('='));
        let state = if has_state {
            HdcTargetState::parse(fields[1])
        } else {
            HdcTargetState::Ready
        };
        let mut extensions = BTreeMap::new();
        for field in fields.iter().skip(if has_state { 2 } else { 1 }) {
            let pair = field.split_once('=').or_else(|| field.split_once(':'));
            if let Some((key, value)) = pair {
                if !key.is_empty() && !value.is_empty() {
                    extensions.insert(key.to_owned(), value.to_owned());
                }
            }
        }
        let name = ["devName", "deviceName", "model"]
            .into_iter()
            .find_map(|key| extensions.get(key).cloned());
        let os_version = ["version", "osVersion"]
            .into_iter()
            .find_map(|key| extensions.get(key).cloned());
        let device = DiscoveredHarmonyDevice {
            target: target.clone(),
            state,
            name,
            os_version,
            extensions,
        };
        if devices.insert(target, device).is_some() {
            return Err(HarmonyHdcError::DuplicateTarget);
        }
    }

    Ok(HarmonyDiscoveryReport {
        devices: devices.into_values().collect(),
        ignored_diagnostics,
    })
}

fn is_diagnostic(line: &str) -> bool {
    line.starts_with("[Info]")
        || line.starts_with("[Warn]")
        || line.starts_with("[Debug]")
        || line.starts_with("Waiting for")
}

fn is_header(line: &str) -> bool {
    let normalized = line.to_ascii_lowercase();
    normalized.starts_with("connectkey")
        || normalized.starts_with("target ")
        || normalized == "list of targets attached"
}

#[cfg(test)]
mod tests {
    use super::parse_targets;
    use crate::HdcTargetState;

    #[test]
    fn discovery_parses_states_metadata_and_diagnostics() {
        let report = parse_targets(
            "[Info] hdc server started\nconnectKey status metadata\n\
             192.0.2.10:8710 connected devName:Mate60 version:5.0\n\
             FMR022 offline model=Tablet\n",
        )
        .expect("discovery report");
        assert_eq!(report.devices.len(), 2);
        assert_eq!(report.ignored_diagnostics.len(), 2);
        assert_eq!(report.devices[0].target.as_str(), "192.0.2.10:8710");
        assert_eq!(report.devices[0].name.as_deref(), Some("Mate60"));
        assert_eq!(report.devices[0].os_version.as_deref(), Some("5.0"));
        assert!(matches!(report.devices[1].state, HdcTargetState::Offline));
    }

    #[test]
    fn discovery_rejects_duplicates_and_malformed_targets() {
        assert!(parse_targets("A connected\nA connected\n").is_err());
        assert!(parse_targets("--help connected\n").is_err());
    }

    #[test]
    fn a_plain_connect_key_is_a_ready_target() {
        let report = parse_targets("FMR022\n").expect("plain target");
        assert!(matches!(report.devices[0].state, HdcTargetState::Ready));
    }
}
