use std::{ffi::OsString, path::PathBuf};

use devicerail_ios_host::{
    DiagnosticStatus, DoctorOptions, IosHostBackend, ManagedIosConfig, SystemIosHost,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let command = arguments.next().unwrap_or_else(|| OsString::from("doctor"));
    let mut json = false;
    let mut device = None;
    let mut project = None;
    let mut derived_data = None;
    let mut iproxy = None;
    let mut local_port = None;
    let mut allow_provisioning_updates = false;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--json") => json = true,
            Some("--allow-provisioning-updates") => allow_provisioning_updates = true,
            Some("--device") => device = Some(required_utf8(&mut arguments, "--device")?),
            Some("--wda-project") => {
                project = Some(PathBuf::from(required(&mut arguments, "--wda-project")?))
            }
            Some("--derived-data") => {
                derived_data = Some(PathBuf::from(required(&mut arguments, "--derived-data")?))
            }
            Some("--iproxy") => iproxy = Some(PathBuf::from(required(&mut arguments, "--iproxy")?)),
            Some("--local-port") => {
                local_port = Some(required_utf8(&mut arguments, "--local-port")?.parse::<u16>()?);
            }
            _ => return Err(usage().into()),
        }
    }

    let host = SystemIosHost::default();
    match command.to_str() {
        Some("doctor") => {
            let report = host
                .doctor(&DoctorOptions {
                    device_udid: device,
                    wda_project: project,
                    iproxy_path: iproxy,
                    wda_endpoint: std::env::var("DEVICERAIL_IOS_WDA_ENDPOINT").ok(),
                    skip_iproxy_check: false,
                    skip_wda_build_checks: false,
                })
                .await;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for check in &report.checks {
                    let label = match check.status {
                        DiagnosticStatus::Pass => "PASS",
                        DiagnosticStatus::Warn => "WARN",
                        DiagnosticStatus::Fail => "FAIL",
                    };
                    println!("{label:4} {:42} {}", check.code, check.summary);
                    if let Some(remediation) = &check.remediation {
                        println!("     -> {remediation}");
                    }
                }
            }
            if report.failed() {
                std::process::exit(2);
            }
        }
        Some("prepare" | "serve") => {
            if let Some(project) = project {
                // Preserve CLI override without mutating process-global environment.
                let mut config = ManagedIosConfig::new(project)?;
                config.device_udid = device;
                if let Some(path) = derived_data {
                    config.derived_data = path;
                }
                if let Some(path) = iproxy {
                    config.iproxy_path = path;
                }
                if let Some(port) = local_port {
                    config.local_port = port;
                }
                config.allow_provisioning_updates = allow_provisioning_updates;
                run_managed(&host, command == "serve", config).await?;
            } else {
                let mut config = ManagedIosConfig::from_environment()?;
                if let Some(device) = device {
                    config.device_udid = Some(device);
                }
                if let Some(path) = derived_data {
                    config.derived_data = path;
                }
                if let Some(path) = iproxy {
                    config.iproxy_path = path;
                }
                if let Some(port) = local_port {
                    config.local_port = port;
                }
                config.allow_provisioning_updates |= allow_provisioning_updates;
                run_managed(&host, command == "serve", config).await?;
            }
        }
        Some("--version") => println!("devicerail-ios {}", env!("CARGO_PKG_VERSION")),
        _ => return Err(usage().into()),
    }
    Ok(())
}

async fn run_managed(
    host: &SystemIosHost,
    serve: bool,
    config: ManagedIosConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    if !serve {
        let prepared = host.prepare(&config).await?;
        println!(
            "prepared {} ({})",
            prepared.device.name,
            if prepared.used_cached_build {
                "cached"
            } else {
                "built"
            }
        );
        return Ok(());
    }
    let runtime = host.start(config).await?;
    println!("{}", runtime.endpoint().wda_url);
    tokio::signal::ctrl_c().await?;
    runtime.shutdown().await?;
    Ok(())
}

fn required(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<OsString, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn required_utf8(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<String, String> {
    required(arguments, option)?
        .into_string()
        .map_err(|_| format!("{option} requires UTF-8"))
}

fn usage() -> String {
    "usage: devicerail-ios doctor|prepare|serve [--json] [--device UDID] [--wda-project PATH] [--derived-data PATH] [--iproxy PATH] [--local-port PORT] [--allow-provisioning-updates]".to_owned()
}
