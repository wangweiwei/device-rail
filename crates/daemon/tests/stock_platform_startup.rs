use std::{
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use std::path::PathBuf;

use serde_json::{Value, json};
use tempfile::TempDir;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_TIMEOUT: Duration = Duration::from_secs(10);

struct DaemonProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Receiver<Result<String, String>>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<Vec<u8>>>,
}

struct CompletedDaemon {
    status: ExitStatus,
    stderr: String,
}

impl DaemonProcess {
    fn spawn(evidence_dir: &Path, environment: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_devicerail-daemon"));
        for (name, _) in env::vars_os() {
            if name.to_string_lossy().starts_with("DEVICERAIL_") {
                command.env_remove(name);
            }
        }
        command
            .env("DEVICERAIL_EVIDENCE_DIR", evidence_dir)
            .env("DEVICERAIL_ANDROID", "off")
            .env("DEVICERAIL_HARMONY", "off")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in environment {
            command.env(name, value);
        }

        let mut child = command.spawn().expect("spawn stock DeviceRail daemon");
        let stdin = child.stdin.take().expect("daemon stdin");
        let stdout = child.stdout.take().expect("daemon stdout");
        let stderr = child.stderr.take().expect("daemon stderr");

        let (stdout_sender, stdout_receiver) = mpsc::channel();
        let stdout_thread = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let line = line.map_err(|error| format!("read daemon stdout: {error}"));
                if stdout_sender.send(line).is_err() {
                    return;
                }
            }
        });
        let stderr_thread = thread::spawn(move || {
            let mut stderr = stderr;
            let mut bytes = Vec::new();
            stderr
                .read_to_end(&mut bytes)
                .expect("read daemon stderr to EOF");
            bytes
        });

        Self {
            child,
            stdin: Some(stdin),
            stdout: stdout_receiver,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
        }
    }

    fn send(&mut self, request: Value) {
        let stdin = self.stdin.as_mut().expect("daemon stdin remains open");
        serde_json::to_writer(&mut *stdin, &request).expect("serialize request to daemon");
        stdin.write_all(b"\n").expect("terminate daemon request");
        stdin.flush().expect("flush daemon request");
    }

    fn response(&mut self) -> Value {
        let line = match self.stdout.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => panic!("{error}"),
            Err(RecvTimeoutError::Timeout) => {
                let status = self.child.try_wait().expect("query daemon status");
                panic!("timed out waiting for daemon response; process status: {status:?}");
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = self.child.try_wait().expect("query daemon status");
                panic!("daemon stdout closed before a response; process status: {status:?}");
            }
        };
        serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!("daemon emitted invalid JSON ({error}): {line}");
        })
    }

    fn assert_running(&mut self) {
        let status = self.child.try_wait().expect("query daemon status");
        assert!(status.is_none(), "daemon exited unexpectedly: {status:?}");
    }

    #[cfg(unix)]
    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    fn finish(mut self) -> CompletedDaemon {
        self.stdin.take();
        let status = wait_for_exit(&mut self.child);
        self.stdout_thread
            .take()
            .expect("stdout reader thread")
            .join()
            .expect("join stdout reader thread");
        let stderr = self
            .stderr_thread
            .take()
            .expect("stderr reader thread")
            .join()
            .expect("join stderr reader thread");
        CompletedDaemon {
            status,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }

    #[cfg(target_os = "macos")]
    fn wait_for_early_exit(mut self) -> CompletedDaemon {
        let status = wait_for_exit(&mut self.child);
        self.stdin.take();
        self.stdout_thread
            .take()
            .expect("stdout reader thread")
            .join()
            .expect("join stdout reader thread");
        let stderr = self
            .stderr_thread
            .take()
            .expect("stderr reader thread")
            .join()
            .expect("join stderr reader thread");
        CompletedDaemon {
            status,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.stdin.take();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(thread) = self.stdout_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }
}

fn wait_for_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + PROCESS_EXIT_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("query daemon status") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().expect("reap timed-out daemon");
            panic!("daemon did not exit after stdin EOF; killed with status {status}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn hello(daemon: &mut DaemonProcess) {
    daemon.send(json!({
        "jsonrpc": "2.0",
        "id": "hello",
        "method": "system.hello",
        "params": {
            "client": { "name": "stock-platform-e2e", "version": "0.1.0" },
            "protocol": {
                "ranges": [{ "major": 1, "minMinor": 0, "maxMinor": 4 }]
            },
            "features": {
                "required": ["device.routing.v1"],
                "optional": []
            }
        }
    }));
    let response = daemon.response();
    assert_eq!(response.get("id"), Some(&json!("hello")), "{response}");
    assert!(response.get("result").is_some(), "{response}");
}

fn list_devices(daemon: &mut DaemonProcess) -> Value {
    daemon.send(json!({
        "jsonrpc": "2.0",
        "id": "devices",
        "method": "devices.list",
        "params": {}
    }));
    let response = daemon.response();
    assert_eq!(response.get("id"), Some(&json!("devices")), "{response}");
    response
}

fn select_device(daemon: &mut DaemonProcess, device_id: &str) {
    daemon.send(json!({
        "jsonrpc": "2.0",
        "id": "select",
        "method": "device.select",
        "params": { "deviceId": device_id }
    }));
    let response = daemon.response();
    assert_eq!(
        response.pointer("/result/device/id"),
        Some(&json!(device_id)),
        "{response}"
    );
}

fn connect_device(daemon: &mut DaemonProcess) -> Value {
    daemon.send(json!({
        "jsonrpc": "2.0",
        "id": "connect",
        "method": "device.connect",
        "params": {}
    }));
    daemon.response()
}

fn disconnect_device(daemon: &mut DaemonProcess) -> Value {
    daemon.send(json!({
        "jsonrpc": "2.0",
        "id": "disconnect",
        "method": "device.disconnect",
        "params": {}
    }));
    daemon.response()
}

#[cfg(unix)]
fn start_session(daemon: &mut DaemonProcess) -> Value {
    daemon.send(json!({
        "jsonrpc": "2.0",
        "id": "session-start",
        "method": "session.start"
    }));
    daemon.response()
}

#[cfg(unix)]
fn observe_device(daemon: &mut DaemonProcess) -> Value {
    daemon.send(json!({
        "jsonrpc": "2.0",
        "id": "observe",
        "method": "device.observe"
    }));
    daemon.response()
}

#[cfg(unix)]
fn execute_tap(daemon: &mut DaemonProcess) -> Value {
    daemon.send(json!({
        "jsonrpc": "2.0",
        "id": "execute",
        "method": "device.execute",
        "params": {
            "id": "77777777-7777-4777-8777-777777777777",
            "name": "tap",
            "arguments": { "x": 12.5, "y": 34.5 }
        }
    }));
    daemon.response()
}

fn devices(response: &Value) -> &[Value] {
    response
        .pointer("/result/devices")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("devices.list response has no inventory: {response}"))
}

fn assert_mock_only(response: &Value) {
    let devices = devices(response);
    assert_eq!(devices.len(), 1, "{response}");
    assert_eq!(devices[0].get("id"), Some(&json!("mock-1")), "{response}");
}

#[cfg(target_os = "macos")]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::write(path, contents).expect("write fake host executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("make fake host executable owner-only");
}

#[cfg(target_os = "macos")]
const TEST_IOS_SIMULATOR_UDID: &str = "375E1581-A0A4-471D-96E1-80CE46933667";

#[cfg(target_os = "macos")]
fn install_booted_simulator_discovery(directory: &Path) -> String {
    let xcrun = directory.join("xcrun");
    write_executable(
        &xcrun,
        r#"#!/bin/sh
if [ "$1" = 'simctl' ]; then
  printf '%s' '{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-26-4":[{"udid":"375E1581-A0A4-471D-96E1-80CE46933667","isAvailable":true,"state":"Booted","name":"Simulator iPhone 16"}]}}'
  exit 0
fi
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--json-output' ]; then
    shift
    output="$1"
  fi
  shift
done
test -n "$output" || exit 1
printf '%s' '{"result":{"devices":[]}}' > "$output"
"#,
    );
    format!("{}:/usr/bin:/bin", directory.to_string_lossy())
}

#[cfg(target_os = "macos")]
fn compile_managed_appium_fixture(directory: &Path) -> PathBuf {
    let source = directory.join("managed-appium-fixture.rs");
    let executable = directory.join("managed-appium-fixture");
    fs::write(
        &source,
        r###"
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.as_slice() == ["--version"] {
        println!("3.0.0-stock-test");
        return;
    }
    let value = |flag: &str| {
        let index = args.iter().position(|value| value == flag).unwrap_or_else(|| std::process::exit(20));
        args.get(index + 1).cloned().unwrap_or_else(|| std::process::exit(21))
    };
    let address = value("--address");
    let port = value("--port");
    let base_path = value("--base-path");
    if address != "127.0.0.1" || args.len() != 6 {
        std::process::exit(22);
    }
    let listener = TcpListener::bind(format!("{address}:{port}"))
        .unwrap_or_else(|_| std::process::exit(23));
    listener.set_nonblocking(true).unwrap();
    if let Ok(marker) = std::env::var("DEVICERAIL_TEST_APPIUM_PORT_FILE") {
        fs::write(marker, port.as_bytes()).unwrap_or_else(|_| std::process::exit(24));
    }
    let status_path = if base_path == "/" {
        "/status".to_owned()
    } else {
        format!("{base_path}/status")
    };
    loop {
        if std::env::var("DEVICERAIL_TEST_APPIUM_EXIT_TRIGGER")
            .ok()
            .is_some_and(|path| std::path::Path::new(&path).exists())
        {
            std::process::exit(42);
        }
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(_) => std::process::exit(25),
        };
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).unwrap_or_else(|_| std::process::exit(26));
            if count == 0 || request.len() + count > 16 * 1024 {
                std::process::exit(27);
            }
            request.extend_from_slice(&buffer[..count]);
        }
        let expected = format!("GET {status_path} HTTP/1.1");
        let ready = String::from_utf8_lossy(&request).starts_with(&expected);
        let (status, body) = if ready {
            ("200 OK", r#"{"value":{"ready":true}}"#)
        } else {
            ("404 Not Found", r#"{"value":{"ready":false}}"#)
        };
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ).unwrap_or_else(|_| std::process::exit(28));
    }
}
"###,
    )
    .expect("write managed Appium fixture source");
    let status = Command::new("rustc")
        .arg("--edition=2024")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .status()
        .expect("compile managed Appium fixture");
    assert!(
        status.success(),
        "managed Appium fixture compilation failed"
    );
    executable
}

#[cfg(target_os = "macos")]
fn wait_for_port_marker(path: &Path) -> u16 {
    let deadline = Instant::now() + SERVER_TIMEOUT;
    loop {
        if let Ok(value) = fs::read_to_string(path)
            && let Ok(port) = value.parse::<u16>()
        {
            return port;
        }
        assert!(
            Instant::now() < deadline,
            "managed Appium fixture did not publish its port"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "macos")]
fn assert_loopback_port_closed(port: u16) {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "managed Appium listener remained alive after cleanup"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "macos")]
struct HotplugWdaServer {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
impl HotplugWdaServer {
    fn start(address: String, device_marker: PathBuf, launch_marker: PathBuf) -> Self {
        let (stop_sender, stop_receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut listener = None;
            loop {
                if !matches!(stop_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return;
                }
                let available = fs::read(&device_marker).is_ok_and(|value| value == b"original")
                    && launch_marker.exists();
                if !available {
                    listener = None;
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                if listener.is_none() {
                    match TcpListener::bind(&address) {
                        Ok(bound) => {
                            bound
                                .set_nonblocking(true)
                                .expect("set hot-plug WDA nonblocking");
                            listener = Some(bound);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                            thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        Err(error) => panic!("bind hot-plug WDA fixture: {error}"),
                    }
                }
                match listener.as_ref().expect("hot-plug listener").accept() {
                    Ok((mut stream, _)) => {
                        read_http_head(&mut stream);
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\nConnection: close\r\n\r\n{\"value\":{\"ready\":true}}",
                            )
                            .expect("write hot-plug WDA response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept hot-plug WDA connection: {error}"),
                }
            }
        });
        Self {
            stop: stop_sender,
            thread: Some(thread),
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for HotplugWdaServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
fn write_owner_only_json(path: &Path, value: &Value) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::write(
        path,
        serde_json::to_vec(value).expect("serialize owner-only config"),
    )
    .expect("write owner-only config");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("make config owner-only");
}

#[cfg(unix)]
fn evidence_data_path(root: &Path, digest: &str) -> std::path::PathBuf {
    assert_eq!(digest.len(), 64, "canonical SHA-256 digest");
    root.join("v1")
        .join("objects")
        .join("sha256")
        .join(&digest[..2])
        .join(&digest[2..4])
        .join(digest)
        .join("data")
}

fn reserve_loopback_address() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback peer address");
    let address = listener
        .local_addr()
        .expect("reserved loopback peer address")
        .to_string();
    drop(listener);
    address
}

#[cfg(unix)]
fn reserve_two_loopback_addresses() -> (String, String) {
    let first = TcpListener::bind("127.0.0.1:0").expect("reserve first loopback peer address");
    let second = TcpListener::bind("127.0.0.1:0").expect("reserve second loopback peer address");
    let first_address = first
        .local_addr()
        .expect("first reserved loopback peer address")
        .to_string();
    let second_address = second
        .local_addr()
        .expect("second reserved loopback peer address")
        .to_string();
    drop((first, second));
    (first_address, second_address)
}

struct OneShotWda {
    endpoint: String,
    accepted: Receiver<()>,
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl OneShotWda {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind one-shot WDA fixture");
        listener
            .set_nonblocking(true)
            .expect("set WDA fixture nonblocking");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("WDA fixture address")
        );
        let (accepted_sender, accepted_receiver) = mpsc::channel();
        let (stop_sender, stop_receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let deadline = Instant::now() + SERVER_TIMEOUT;
            let mut stream = loop {
                if !matches!(stop_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "daemon never contacted the one-shot WDA fixture"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept WDA fixture connection: {error}"),
                }
            };
            accepted_sender
                .send(())
                .expect("report WDA fixture connection");
            read_http_head(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .expect("write WDA fixture response");
        });
        Self {
            endpoint,
            accepted: accepted_receiver,
            stop: stop_sender,
            thread: Some(thread),
        }
    }

    fn assert_not_contacted(&self) {
        assert!(
            matches!(self.accepted.try_recv(), Err(TryRecvError::Empty)),
            "the daemon contacted WDA during startup or inventory listing"
        );
    }

    fn assert_contacted(&self) {
        self.accepted
            .recv_timeout(RESPONSE_TIMEOUT)
            .expect("device.connect contacts WDA");
    }
}

impl Drop for OneShotWda {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Debug)]
struct CapturedHttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

struct FakeAppiumServer {
    endpoint: String,
    requests: Receiver<CapturedHttpRequest>,
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl FakeAppiumServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake Appium server");
        listener
            .set_nonblocking(true)
            .expect("set fake Appium server nonblocking");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("fake Appium address")
        );
        let (request_sender, request_receiver) = mpsc::channel();
        let (stop_sender, stop_receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let deadline = Instant::now() + SERVER_TIMEOUT;
            loop {
                if !matches!(stop_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return;
                }
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "fake Appium server exceeded its bounded lifetime"
                        );
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("accept fake Appium connection: {error}"),
                };
                let request = read_http_request(&mut stream);
                let response = match (request.method.as_str(), request.path.as_str()) {
                    ("GET", "/status") => json!({
                        "value": {
                            "ready": true,
                            "message": "ready",
                            "build": { "version": "2.0.0" },
                            "os": { "version": "18.0" }
                        }
                    }),
                    ("POST", "/session") => json!({
                        "value": {
                            "sessionId": "stock-appium-session",
                            "capabilities": {}
                        }
                    }),
                    ("DELETE", "/session/stock-appium-session") => {
                        json!({ "value": null })
                    }
                    _ => json!({
                        "value": {
                            "error": "unknown command",
                            "message": "unexpected fake Appium route"
                        }
                    }),
                };
                let known_route = matches!(
                    (request.method.as_str(), request.path.as_str()),
                    ("GET", "/status")
                        | ("POST", "/session")
                        | ("DELETE", "/session/stock-appium-session")
                );
                request_sender
                    .send(request)
                    .expect("report fake Appium request");
                let body = serde_json::to_vec(&response).expect("serialize fake Appium response");
                let status = if known_route {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .expect("write fake Appium response head");
                stream
                    .write_all(&body)
                    .expect("write fake Appium response body");
            }
        });
        Self {
            endpoint,
            requests: request_receiver,
            stop: stop_sender,
            thread: Some(thread),
        }
    }

    fn assert_not_contacted(&self) {
        assert!(
            matches!(self.requests.try_recv(), Err(TryRecvError::Empty)),
            "the daemon contacted Appium before device.connect"
        );
    }

    fn request(&self) -> CapturedHttpRequest {
        self.requests
            .recv_timeout(RESPONSE_TIMEOUT)
            .expect("receive fake Appium request")
    }
}

#[cfg(target_os = "macos")]
fn next_appium_session_request(appium: &FakeAppiumServer) -> CapturedHttpRequest {
    let mut status_count = 0;
    loop {
        let request = appium.request();
        if (request.method.as_str(), request.path.as_str()) == ("POST", "/session") {
            assert!(status_count >= 1);
            return request;
        }
        assert_eq!(
            (request.method.as_str(), request.path.as_str()),
            ("GET", "/status")
        );
        assert!(request.body.is_empty());
        status_count += 1;
        assert!(status_count <= 4, "Appium status probing must stay bounded");
    }
}

impl Drop for FakeAppiumServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_http_request(stream: &mut TcpStream) -> CapturedHttpRequest {
    const MAX_REQUEST_BYTES: usize = 256 * 1024;

    stream
        .set_read_timeout(Some(SERVER_TIMEOUT))
        .expect("bound fake Appium read");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let (header_end, content_length) = loop {
        let read = stream.read(&mut buffer).expect("read fake Appium request");
        assert!(read > 0, "fake Appium request ended before its headers");
        request.extend_from_slice(&buffer[..read]);
        assert!(
            request.len() <= MAX_REQUEST_BYTES,
            "fake Appium request is bounded"
        );
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let header =
            std::str::from_utf8(&request[..header_end]).expect("fake Appium request head is UTF-8");
        let content_length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().expect("valid content length"))
            })
            .unwrap_or(0);
        assert!(
            header_end + 4 + content_length <= MAX_REQUEST_BYTES,
            "fake Appium request body is bounded"
        );
        break (header_end, content_length);
    };
    let total_length = header_end + 4 + content_length;
    while request.len() < total_length {
        let read = stream
            .read(&mut buffer)
            .expect("read fake Appium request body");
        assert!(read > 0, "fake Appium request body ended early");
        request.extend_from_slice(&buffer[..read]);
        assert!(request.len() <= MAX_REQUEST_BYTES);
    }
    let header =
        std::str::from_utf8(&request[..header_end]).expect("fake Appium request head is UTF-8");
    let mut first_line = header.lines().next().expect("HTTP request line").split(' ');
    let method = first_line.next().expect("HTTP method").to_owned();
    let path = first_line.next().expect("HTTP path").to_owned();
    CapturedHttpRequest {
        method,
        path,
        body: request[header_end + 4..total_length].to_vec(),
    }
}

fn read_http_head(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(SERVER_TIMEOUT))
        .expect("bound WDA fixture read");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).expect("read WDA fixture request");
        assert!(read > 0, "WDA request ended before its headers");
        request.extend_from_slice(&buffer[..read]);
        assert!(
            request.len() <= 64 * 1024,
            "WDA request headers are bounded"
        );
    }
}

#[test]
fn ios_environment_registers_a_lazy_stock_route() {
    let evidence = TempDir::new().expect("temporary evidence directory");
    let wda = OneShotWda::start();
    let mut daemon = DaemonProcess::spawn(
        evidence.path(),
        &[
            ("DEVICERAIL_IOS_WDA_ENDPOINT", wda.endpoint.as_str()),
            ("DEVICERAIL_IOS_DEVICE_TOKEN", "stock-ios-e2e"),
            ("DEVICERAIL_IOS_DEVICE_NAME", "Stock iPhone"),
            ("DEVICERAIL_IOS_OS_VERSION", "18.0"),
        ],
    );

    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    let inventory = devices(&listed);
    assert_eq!(inventory.len(), 2, "{listed}");
    assert!(
        inventory
            .iter()
            .any(|device| device.get("id") == Some(&json!("mock-1"))),
        "{listed}"
    );
    assert!(
        inventory.iter().any(|device| {
            device.get("id") == Some(&json!("ios-wda:stock-ios-e2e"))
                && device.pointer("/platform/kind") == Some(&json!("ios"))
                && device.get("connected") == Some(&json!(false))
        }),
        "{listed}"
    );
    wda.assert_not_contacted();

    select_device(&mut daemon, "ios-wda:stock-ios-e2e");
    let connected = connect_device(&mut daemon);
    assert_eq!(
        connected.pointer("/error/data/code"),
        Some(&json!("platform_error")),
        "{connected}"
    );
    assert_eq!(
        connected.pointer("/error/data/details/platformCode"),
        Some(&json!("wda_http_status")),
        "{connected}"
    );
    wda.assert_contacted();
    daemon.assert_running();

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
}

#[test]
fn ios_appium_environment_uses_bundled_wda_and_owns_one_lazy_w3c_session() {
    let evidence = TempDir::new().expect("temporary evidence directory");
    let appium = FakeAppiumServer::start();
    let mut daemon = DaemonProcess::spawn(
        evidence.path(),
        &[
            ("DEVICERAIL_IOS_BACKEND", "appium"),
            ("DEVICERAIL_IOS_APPIUM_ENDPOINT", &appium.endpoint),
            ("DEVICERAIL_IOS_DEVICE_TOKEN", "stock-ios-appium-e2e"),
            ("DEVICERAIL_IOS_DEVICE_NAME", "Stock Appium iPhone"),
            ("DEVICERAIL_IOS_OS_VERSION", "18.0"),
        ],
    );

    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    assert!(devices(&listed).iter().any(|device| {
        device.get("id") == Some(&json!("ios-wda:stock-ios-appium-e2e"))
            && device.pointer("/platform/kind") == Some(&json!("ios"))
            && device.get("connected") == Some(&json!(false))
    }));
    appium.assert_not_contacted();

    select_device(&mut daemon, "ios-wda:stock-ios-appium-e2e");
    let connected = connect_device(&mut daemon);
    assert_eq!(
        connected.pointer("/result/connected"),
        Some(&json!(true)),
        "{connected}"
    );

    let mut status_count = 0;
    let create = loop {
        let request = appium.request();
        if (request.method.as_str(), request.path.as_str()) == ("POST", "/session") {
            break request;
        }
        assert_eq!(
            (request.method.as_str(), request.path.as_str()),
            ("GET", "/status")
        );
        assert!(request.body.is_empty());
        status_count += 1;
        assert!(status_count <= 4, "Appium status probing must stay bounded");
    };
    assert!(status_count >= 1);
    assert_eq!(
        (create.method.as_str(), create.path.as_str()),
        ("POST", "/session")
    );
    let capabilities: Value =
        serde_json::from_slice(&create.body).expect("parse Appium session capabilities");
    let always_match = capabilities
        .pointer("/capabilities/alwaysMatch")
        .expect("closed alwaysMatch capabilities");
    assert_eq!(always_match.get("platformName"), Some(&json!("iOS")));
    assert_eq!(
        always_match.get("appium:automationName"),
        Some(&json!("XCUITest"))
    );
    assert_eq!(
        always_match.get("appium:udid"),
        Some(&json!("stock-ios-appium-e2e"))
    );
    assert_eq!(
        always_match.get("appium:deviceName"),
        Some(&json!("Stock Appium iPhone"))
    );
    assert_eq!(
        always_match.get("appium:platformVersion"),
        Some(&json!("18.0"))
    );
    assert_eq!(
        always_match.get("appium:includeSafariInWebviews"),
        Some(&json!(true))
    );
    assert_eq!(
        always_match.get("appium:newCommandTimeout"),
        Some(&json!(600))
    );
    assert!(
        always_match.get("appium:webDriverAgentUrl").is_none(),
        "without an explicit WDA endpoint, XCUITest Driver must manage its bundled WDA"
    );
    assert_eq!(
        always_match.as_object().map(serde_json::Map::len),
        Some(7),
        "daemon must not pass arbitrary capabilities: {always_match}"
    );

    let disconnected = disconnect_device(&mut daemon);
    assert_eq!(
        disconnected.pointer("/result/disconnected"),
        Some(&json!(true)),
        "{disconnected}"
    );
    let delete = appium.request();
    assert_eq!(
        (delete.method.as_str(), delete.path.as_str()),
        ("DELETE", "/session/stock-appium-session")
    );
    assert!(delete.body.is_empty());

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
}

#[cfg(target_os = "macos")]
#[test]
fn ios_appium_discovery_registers_a_booted_simulator_with_an_explicit_safari_session() {
    let directory = TempDir::new().expect("temporary Simulator discovery directory");
    let path = install_booted_simulator_discovery(directory.path());
    let appium = FakeAppiumServer::start();
    let mut daemon = DaemonProcess::spawn(
        directory.path(),
        &[
            ("PATH", &path),
            ("DEVICERAIL_IOS", "required"),
            ("DEVICERAIL_IOS_BACKEND", "appium"),
            ("DEVICERAIL_IOS_SESSION_TARGET", "safari"),
            ("DEVICERAIL_IOS_APPIUM_ENDPOINT", &appium.endpoint),
            ("DEVICERAIL_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS", "601"),
        ],
    );

    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    let simulator_id = format!("ios-wda:{TEST_IOS_SIMULATOR_UDID}");
    assert!(devices(&listed).iter().any(|device| {
        device.get("id") == Some(&json!(&simulator_id))
            && device.pointer("/platform/kind") == Some(&json!("ios"))
            && device.get("connected") == Some(&json!(false))
    }));
    appium.assert_not_contacted();

    select_device(&mut daemon, &simulator_id);
    let connected = connect_device(&mut daemon);
    assert_eq!(
        connected.pointer("/result/connected"),
        Some(&json!(true)),
        "{connected}"
    );

    let create = next_appium_session_request(&appium);
    let capabilities: Value =
        serde_json::from_slice(&create.body).expect("parse Simulator Appium capabilities");
    let always_match = capabilities
        .pointer("/capabilities/alwaysMatch")
        .expect("closed alwaysMatch capabilities");
    assert_eq!(always_match.get("platformName"), Some(&json!("iOS")));
    assert_eq!(
        always_match.get("appium:automationName"),
        Some(&json!("XCUITest"))
    );
    assert_eq!(
        always_match.get("appium:udid"),
        Some(&json!(TEST_IOS_SIMULATOR_UDID))
    );
    assert_eq!(
        always_match.get("appium:deviceName"),
        Some(&json!("Simulator iPhone 16"))
    );
    assert_eq!(
        always_match.get("appium:platformVersion"),
        Some(&json!("26.4"))
    );
    assert_eq!(always_match.get("browserName"), Some(&json!("Safari")));
    assert_eq!(
        always_match.get("appium:includeSafariInWebviews"),
        Some(&json!(true))
    );
    assert_eq!(
        always_match.get("appium:newCommandTimeout"),
        Some(&json!(601))
    );
    assert!(always_match.get("appium:bundleId").is_none());
    assert!(always_match.get("appium:webDriverAgentUrl").is_none());
    assert_eq!(
        always_match.as_object().map(serde_json::Map::len),
        Some(8),
        "daemon must emit only the typed Simulator Safari capabilities: {always_match}"
    );

    let disconnected = disconnect_device(&mut daemon);
    assert_eq!(
        disconnected.pointer("/result/disconnected"),
        Some(&json!(true)),
        "{disconnected}"
    );
    let delete = appium.request();
    assert_eq!(
        (delete.method.as_str(), delete.path.as_str()),
        ("DELETE", "/session/stock-appium-session")
    );

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
}

#[cfg(target_os = "macos")]
#[test]
fn ios_appium_simulator_defaults_to_a_native_session() {
    let directory = TempDir::new().expect("temporary Simulator discovery directory");
    let path = install_booted_simulator_discovery(directory.path());
    let appium = FakeAppiumServer::start();
    let mut daemon = DaemonProcess::spawn(
        directory.path(),
        &[
            ("PATH", &path),
            ("DEVICERAIL_IOS", "required"),
            ("DEVICERAIL_IOS_BACKEND", "appium"),
            ("DEVICERAIL_IOS_APPIUM_ENDPOINT", &appium.endpoint),
        ],
    );

    hello(&mut daemon);
    let simulator_id = format!("ios-wda:{TEST_IOS_SIMULATOR_UDID}");
    let listed = list_devices(&mut daemon);
    assert!(
        devices(&listed)
            .iter()
            .any(|device| device.get("id") == Some(&json!(&simulator_id)))
    );
    select_device(&mut daemon, &simulator_id);
    let connected = connect_device(&mut daemon);
    assert_eq!(
        connected.pointer("/result/connected"),
        Some(&json!(true)),
        "{connected}"
    );

    let create = next_appium_session_request(&appium);
    let capabilities: Value =
        serde_json::from_slice(&create.body).expect("parse Simulator Appium capabilities");
    let always_match = capabilities
        .pointer("/capabilities/alwaysMatch")
        .expect("closed alwaysMatch capabilities");
    assert_eq!(
        always_match.get("appium:udid"),
        Some(&json!(TEST_IOS_SIMULATOR_UDID))
    );
    assert!(
        always_match.get("browserName").is_none(),
        "Simulator kind must not implicitly select Safari: {always_match}"
    );
    assert_eq!(
        always_match.get("appium:newCommandTimeout"),
        Some(&json!(600))
    );
    assert_eq!(always_match.as_object().map(serde_json::Map::len), Some(7));

    let disconnected = disconnect_device(&mut daemon);
    assert_eq!(
        disconnected.pointer("/result/disconnected"),
        Some(&json!(true)),
        "{disconnected}"
    );
    let delete = appium.request();
    assert_eq!(
        (delete.method.as_str(), delete.path.as_str()),
        ("DELETE", "/session/stock-appium-session")
    );
    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
}

#[test]
fn ios_appium_environment_rejects_non_loopback_endpoint_without_leaking_it() {
    let evidence = TempDir::new().expect("temporary evidence directory");
    let secret_marker = "appium-endpoint-secret-marker";
    let appium_endpoint = format!("http://192.0.2.1:4723/{secret_marker}");
    let daemon = DaemonProcess::spawn(
        evidence.path(),
        &[
            ("DEVICERAIL_IOS_BACKEND", "appium"),
            ("DEVICERAIL_IOS_APPIUM_ENDPOINT", &appium_endpoint),
            ("DEVICERAIL_IOS_WDA_ENDPOINT", "http://127.0.0.1:8100"),
            ("DEVICERAIL_IOS_DEVICE_TOKEN", "stock-ios-appium-e2e"),
        ],
    );

    let completed = daemon.finish();
    assert!(!completed.status.success(), "daemon unexpectedly succeeded");
    assert!(
        completed.stderr.contains("InvalidIosAppiumConfiguration"),
        "{}",
        completed.stderr
    );
    assert!(!completed.stderr.contains(secret_marker));
}

#[cfg(target_os = "macos")]
#[test]
fn managed_appium_is_critical_and_is_cleaned_on_shutdown_and_startup_failure() {
    let directory = TempDir::new().expect("temporary managed Appium directory");
    let executable = compile_managed_appium_fixture(directory.path());
    let executable = executable.to_string_lossy().into_owned();

    let normal_evidence = directory.path().join("normal-evidence");
    let normal_port_marker = directory.path().join("normal-port");
    let normal_port_marker_text = normal_port_marker.to_string_lossy().into_owned();
    let mut normal = DaemonProcess::spawn(
        &normal_evidence,
        &[
            ("DEVICERAIL_IOS_BACKEND", "appium"),
            ("DEVICERAIL_IOS_APPIUM_PATH", &executable),
            ("DEVICERAIL_IOS_DEVICE_TOKEN", "managed-appium-normal"),
            ("DEVICERAIL_TEST_APPIUM_PORT_FILE", &normal_port_marker_text),
        ],
    );
    hello(&mut normal);
    let listed = list_devices(&mut normal);
    assert!(
        devices(&listed)
            .iter()
            .any(|device| { device.get("id") == Some(&json!("ios-wda:managed-appium-normal")) })
    );
    let normal_port = wait_for_port_marker(&normal_port_marker);
    let completed = normal.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
    assert_loopback_port_closed(normal_port);

    let failed_evidence = directory.path().join("failed-evidence");
    let failed_port_marker = directory.path().join("failed-port");
    let failed_port_marker_text = failed_port_marker.to_string_lossy().into_owned();
    let missing_hdc = directory.path().join("missing-hdc");
    let missing_hdc = missing_hdc.to_string_lossy().into_owned();
    let failed = DaemonProcess::spawn(
        &failed_evidence,
        &[
            ("DEVICERAIL_IOS_BACKEND", "appium"),
            ("DEVICERAIL_IOS_APPIUM_PATH", &executable),
            (
                "DEVICERAIL_IOS_DEVICE_TOKEN",
                "managed-appium-startup-failure",
            ),
            ("DEVICERAIL_HARMONY", "required"),
            ("DEVICERAIL_HDC_PATH", &missing_hdc),
            ("DEVICERAIL_TEST_APPIUM_PORT_FILE", &failed_port_marker_text),
        ],
    );
    let failed_port = wait_for_port_marker(&failed_port_marker);
    let completed = failed.wait_for_early_exit();
    assert!(!completed.status.success(), "daemon unexpectedly succeeded");
    assert!(
        completed.stderr.contains("HarmonyRequired")
            && completed.stderr.contains("hdc_executable_not_found"),
        "{}",
        completed.stderr
    );
    assert_loopback_port_closed(failed_port);

    let critical_evidence = directory.path().join("critical-evidence");
    let critical_port_marker = directory.path().join("critical-port");
    let critical_port_marker_text = critical_port_marker.to_string_lossy().into_owned();
    let exit_trigger = directory.path().join("exit-appium");
    let exit_trigger_text = exit_trigger.to_string_lossy().into_owned();
    let mut critical = DaemonProcess::spawn(
        &critical_evidence,
        &[
            ("DEVICERAIL_IOS_BACKEND", "appium"),
            ("DEVICERAIL_IOS_APPIUM_PATH", &executable),
            ("DEVICERAIL_IOS_DEVICE_TOKEN", "managed-appium-critical"),
            (
                "DEVICERAIL_TEST_APPIUM_PORT_FILE",
                &critical_port_marker_text,
            ),
            ("DEVICERAIL_TEST_APPIUM_EXIT_TRIGGER", &exit_trigger_text),
        ],
    );
    hello(&mut critical);
    let critical_port = wait_for_port_marker(&critical_port_marker);
    fs::write(&exit_trigger, b"exit").expect("trigger managed Appium exit");
    let completed = critical.wait_for_early_exit();
    assert!(
        !completed.status.success(),
        "daemon ignored managed Appium exit"
    );
    assert!(
        completed.stderr.contains("IosManagedAppiumRuntime")
            && completed.stderr.contains("ios_appium_exited"),
        "{}",
        completed.stderr
    );
    assert!(
        !completed
            .stderr
            .contains("managed Appium cleanup failed after server shutdown"),
        "runtime failure must not be delayed or misclassified: {}",
        completed.stderr
    );
    assert_loopback_port_closed(critical_port);
}

#[test]
fn managed_ios_auto_missing_project_preserves_the_stock_daemon() {
    let evidence = TempDir::new().expect("temporary evidence directory");
    let secret_marker = "managed-wda-project-secret-marker";
    let missing = evidence
        .path()
        .join(secret_marker)
        .join("WebDriverAgent.xcodeproj");
    let missing = missing.to_string_lossy().into_owned();
    let mut daemon = DaemonProcess::spawn(
        evidence.path(),
        &[
            ("DEVICERAIL_IOS", "auto"),
            ("DEVICERAIL_IOS_WDA_PROJECT", &missing),
        ],
    );

    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    assert_mock_only(&listed);
    daemon.assert_running();

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
    assert!(completed.stderr.contains("ios_wda_project_missing"));
    assert!(!completed.stderr.contains(secret_marker));
    assert!(!completed.stderr.contains(&missing));
}

#[cfg(target_os = "macos")]
#[test]
fn managed_ios_auto_registers_after_hotplug_and_recovers_the_same_route() {
    let directory = TempDir::new().expect("temporary managed iOS directory");
    let project = directory.path().join("WebDriverAgent.xcodeproj");
    fs::create_dir_all(&project).expect("fake WDA project directory");
    fs::write(
        project.join("project.pbxproj"),
        "// managed hot-plug fixture",
    )
    .expect("fake WDA project");
    let derived_data = directory.path().join("DerivedData");
    let device_marker = directory.path().join("device-connected");
    let launch_marker = directory.path().join("wda-launched");
    let launch_count = directory.path().join("wda-launch-count");
    for path in [&device_marker, &launch_marker, &launch_count] {
        assert!(!path.to_string_lossy().contains('\''));
    }

    let xcrun = directory.path().join("xcrun");
    write_executable(
        &xcrun,
        &format!(
            r#"#!/bin/sh
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--json-output' ]; then
    shift
    output="$1"
  fi
  shift
done
test -n "$output" || exit 1
if [ "$(cat '{}' 2>/dev/null)" = 'original' ]; then
  printf '%s' '{{"result":{{"devices":[{{"connectionProperties":{{"pairingState":"paired","tunnelState":"connected"}},"deviceProperties":{{"bootState":"booted","ddiServicesAvailable":true,"developerModeStatus":"enabled","name":"Hotplug iPhone","osVersionNumber":"18.5"}},"hardwareProperties":{{"platform":"iOS","udid":"hotplug-test-device"}}}}]}}}}' > "$output"
elif [ "$(cat '{}' 2>/dev/null)" = 'replacement' ]; then
  printf '%s' '{{"result":{{"devices":[{{"connectionProperties":{{"pairingState":"paired","tunnelState":"connected"}},"deviceProperties":{{"bootState":"booted","ddiServicesAvailable":true,"developerModeStatus":"enabled","name":"Other iPhone","osVersionNumber":"18.5"}},"hardwareProperties":{{"platform":"iOS","udid":"replacement-test-device"}}}}]}}}}' > "$output"
else
  printf '%s' '{{"result":{{"devices":[]}}}}' > "$output"
fi
"#,
            device_marker.display(),
            device_marker.display()
        ),
    );

    let xcodebuild = directory.path().join("xcodebuild");
    write_executable(
        &xcodebuild,
        &format!(
            r#"#!/bin/sh
if [ "$1" = '-version' ]; then
  printf '%s\n' 'Xcode 16.4' 'Build version 16F6'
  exit 0
fi
action=''
derived=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -derivedDataPath) shift; derived="$1" ;;
    build-for-testing) action='build' ;;
    test-without-building) action='test' ;;
  esac
  shift
done
if [ "$action" = 'build' ]; then
  mkdir -p "$derived/Build/Products"
  exit 0
fi
if [ "$action" = 'test' ]; then
  count=0
  if [ -f '{}' ]; then count=$(cat '{}'); fi
  count=$((count + 1))
  printf '%s' "$count" > '{}'
  printf '%s' "$USE_PORT" > '{}'
  while [ "$(cat '{}' 2>/dev/null)" = 'original' ]; do sleep 1; done
  exit 0
fi
exit 1
"#,
            launch_count.display(),
            launch_count.display(),
            launch_count.display(),
            launch_marker.display(),
            device_marker.display(),
        ),
    );
    let git = directory.path().join("git");
    write_executable(
        &git,
        r#"#!/bin/sh
case " $* " in
  *' rev-parse '*) printf '%s\n' '0123456789abcdef0123456789abcdef01234567' ;;
  *' diff '*) printf '%s' 'tracked hot-plug fixture' ;;
  *' ls-files '*) : ;;
  *) exit 1 ;;
esac
"#,
    );
    let iproxy = directory.path().join("iproxy");
    write_executable(&iproxy, "#!/bin/sh\nexec sleep 600\n");

    let local_address = reserve_loopback_address();
    let local_port = local_address
        .rsplit_once(':')
        .expect("loopback port")
        .1
        .to_owned();
    let _wda = HotplugWdaServer::start(local_address, device_marker.clone(), launch_marker.clone());
    let path = format!("{}:/usr/bin:/bin", directory.path().to_string_lossy());
    let project_text = project.to_string_lossy().into_owned();
    let derived_text = derived_data.to_string_lossy().into_owned();
    let iproxy_text = iproxy.to_string_lossy().into_owned();
    let mut daemon = DaemonProcess::spawn(
        directory.path(),
        &[
            ("PATH", &path),
            ("DEVICERAIL_IOS", "auto"),
            ("DEVICERAIL_IOS_WDA_PROJECT", &project_text),
            ("DEVICERAIL_IOS_DERIVED_DATA", &derived_text),
            ("DEVICERAIL_IOS_IPROXY_PATH", &iproxy_text),
            ("DEVICERAIL_IOS_WDA_LOCAL_PORT", &local_port),
        ],
    );

    hello(&mut daemon);
    assert_mock_only(&list_devices(&mut daemon));
    fs::write(&device_marker, b"original").expect("hot-plug iPhone");
    let ready_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let listed = list_devices(&mut daemon);
        if devices(&listed)
            .iter()
            .any(|device| device.get("id") == Some(&json!("ios-wda:hotplug-test-device")))
        {
            break;
        }
        assert!(Instant::now() < ready_deadline, "hot-plug route timed out");
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        fs::read_to_string(&launch_count).expect("first WDA launch count"),
        "1"
    );

    fs::remove_file(&device_marker).expect("unplug iPhone");
    thread::sleep(Duration::from_millis(2_500));
    fs::write(&device_marker, b"replacement").expect("connect a different iPhone");
    thread::sleep(Duration::from_secs(5));
    assert_eq!(
        fs::read_to_string(&launch_count).expect("identity-pinned WDA launch count"),
        "1",
        "a published route must not drift to a replacement phone"
    );
    fs::write(&device_marker, b"original").expect("reconnect original iPhone");
    let recovery_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let launches = fs::read_to_string(&launch_count)
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or_default();
        if launches >= 2 {
            break;
        }
        assert!(
            Instant::now() < recovery_deadline,
            "hot-plug recovery timed out"
        );
        thread::sleep(Duration::from_millis(100));
    }
    let listed = list_devices(&mut daemon);
    assert_eq!(
        devices(&listed)
            .iter()
            .filter(|device| device.get("id") == Some(&json!("ios-wda:hotplug-test-device")))
            .count(),
        1,
        "{listed}"
    );

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
    assert!(completed.stderr.contains("ios_hotplug_ready"));
    assert!(completed.stderr.contains("ios_wda_recovered"));
}

#[cfg(target_os = "macos")]
#[test]
fn managed_ios_simulator_auto_registers_without_iproxy_and_recovers_the_same_udid() {
    const ORIGINAL_UDID: &str = "375E1581-A0A4-471D-96E1-80CE46933667";
    const REPLACEMENT_UDID: &str = "A7D8E6C1-2D04-4C7A-A998-3B6FB044067E";

    let directory = TempDir::new().expect("temporary managed Simulator directory");
    let project = directory.path().join("WebDriverAgent.xcodeproj");
    fs::create_dir_all(&project).expect("fake WDA project directory");
    fs::write(
        project.join("project.pbxproj"),
        "// managed Simulator hot-plug fixture",
    )
    .expect("fake WDA project");
    let derived_data = directory.path().join("DerivedData");
    let simulator_state = directory.path().join("simulator-state");
    let launch_marker = directory.path().join("simulator-wda-port");
    let launch_count = directory.path().join("simulator-wda-launch-count");
    let destination_marker = directory.path().join("simulator-wda-destinations");
    let iproxy_marker = directory.path().join("iproxy-invoked");
    for path in [
        &simulator_state,
        &launch_marker,
        &launch_count,
        &destination_marker,
        &iproxy_marker,
    ] {
        assert!(!path.to_string_lossy().contains('\''));
    }

    let simulator_state_text = simulator_state.to_string_lossy().into_owned();
    let xcrun = directory.path().join("xcrun");
    let xcrun_script = r#"#!/bin/sh
if [ "$1" = 'simctl' ]; then
  original_state='Shutdown'
  replacement_state='Shutdown'
  case "$(cat '__SIMULATOR_STATE__' 2>/dev/null)" in
    original) original_state='Booted' ;;
    replacement) replacement_state='Booted' ;;
  esac
  printf '{"runtimes":[{"identifier":"com.apple.CoreSimulator.SimRuntime.iOS-26-4","version":"26.4.1","isAvailable":true}],"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-26-4":[{"state":"%s","isAvailable":true,"name":"Original Simulator","udid":"375E1581-A0A4-471D-96E1-80CE46933667"},{"state":"%s","isAvailable":true,"name":"Replacement Simulator","udid":"A7D8E6C1-2D04-4C7A-A998-3B6FB044067E"}]}}' "$original_state" "$replacement_state"
  exit 0
fi
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--json-output' ]; then
    shift
    output="$1"
  fi
  shift
done
test -n "$output" || exit 1
printf '%s' '{"result":{"devices":[]}}' > "$output"
"#
    .replace("__SIMULATOR_STATE__", &simulator_state_text);
    write_executable(&xcrun, &xcrun_script);

    let launch_marker_text = launch_marker.to_string_lossy().into_owned();
    let launch_count_text = launch_count.to_string_lossy().into_owned();
    let destination_marker_text = destination_marker.to_string_lossy().into_owned();
    let xcodebuild = directory.path().join("xcodebuild");
    let xcodebuild_script = r#"#!/bin/sh
if [ "$1" = '-version' ]; then
  printf '%s\n' 'Xcode 16.4' 'Build version 16F6'
  exit 0
fi
action=''
derived=''
destination=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -derivedDataPath) shift; derived="$1" ;;
    -destination) shift; destination="$1" ;;
    build-for-testing) action='build' ;;
    test-without-building) action='test' ;;
  esac
  shift
done
if [ "$action" = 'build' ]; then
  mkdir -p "$derived/Build/Products"
  exit 0
fi
if [ "$action" = 'test' ]; then
  printf '%s' "$USE_PORT" > '__LAUNCH_MARKER__'
  printf '%s\n' "$destination" >> '__DESTINATION_MARKER__'
  count=0
  if [ -f '__LAUNCH_COUNT__' ]; then count=$(cat '__LAUNCH_COUNT__'); fi
  count=$((count + 1))
  printf '%s' "$count" > '__LAUNCH_COUNT__'
  while [ "$(cat '__SIMULATOR_STATE__' 2>/dev/null)" = 'original' ]; do sleep 1; done
  exit 0
fi
exit 1
"#
    .replace("__LAUNCH_MARKER__", &launch_marker_text)
    .replace("__DESTINATION_MARKER__", &destination_marker_text)
    .replace("__LAUNCH_COUNT__", &launch_count_text)
    .replace("__SIMULATOR_STATE__", &simulator_state_text);
    write_executable(&xcodebuild, &xcodebuild_script);

    let git = directory.path().join("git");
    write_executable(
        &git,
        r#"#!/bin/sh
case " $* " in
  *' rev-parse '*) printf '%s\n' '0123456789abcdef0123456789abcdef01234567' ;;
  *' diff '*) printf '%s' 'tracked Simulator hot-plug fixture' ;;
  *' ls-files '*) : ;;
  *) exit 1 ;;
esac
"#,
    );
    let iproxy = directory.path().join("iproxy");
    let iproxy_marker_text = iproxy_marker.to_string_lossy().into_owned();
    let iproxy_script = r#"#!/bin/sh
printf '%s' 'invoked' > '__IPROXY_MARKER__'
exec sleep 600
"#
    .replace("__IPROXY_MARKER__", &iproxy_marker_text);
    write_executable(&iproxy, &iproxy_script);

    let local_address = reserve_loopback_address();
    let local_port = local_address
        .rsplit_once(':')
        .expect("loopback port")
        .1
        .to_owned();
    assert_ne!(
        local_port, "8100",
        "fixture must distinguish local/remote ports"
    );
    let _wda = HotplugWdaServer::start(
        local_address,
        simulator_state.clone(),
        launch_marker.clone(),
    );
    let path = format!("{}:/usr/bin:/bin", directory.path().to_string_lossy());
    let project_text = project.to_string_lossy().into_owned();
    let derived_text = derived_data.to_string_lossy().into_owned();
    let iproxy_text = iproxy.to_string_lossy().into_owned();
    let mut daemon = DaemonProcess::spawn(
        directory.path(),
        &[
            ("PATH", &path),
            ("DEVICERAIL_IOS", "auto"),
            ("DEVICERAIL_IOS_BACKEND", "direct-wda"),
            ("DEVICERAIL_IOS_WDA_PROJECT", &project_text),
            ("DEVICERAIL_IOS_DERIVED_DATA", &derived_text),
            ("DEVICERAIL_IOS_IPROXY_PATH", &iproxy_text),
            ("DEVICERAIL_IOS_WDA_LOCAL_PORT", &local_port),
        ],
    );

    hello(&mut daemon);
    assert_mock_only(&list_devices(&mut daemon));
    fs::write(&simulator_state, b"original").expect("boot original Simulator");
    let ready_deadline = Instant::now() + Duration::from_secs(20);
    let listed = loop {
        let listed = list_devices(&mut daemon);
        if devices(&listed)
            .iter()
            .any(|device| device.get("id") == Some(&json!(format!("ios-wda:{ORIGINAL_UDID}"))))
        {
            break listed;
        }
        assert!(
            Instant::now() < ready_deadline,
            "Simulator hot-plug route timed out"
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        fs::read_to_string(&launch_count).expect("first Simulator WDA launch count"),
        "1"
    );
    assert_eq!(
        fs::read_to_string(&launch_marker).expect("Simulator WDA local port"),
        local_port
    );
    assert!(
        !iproxy_marker.exists(),
        "Direct WDA must not launch iproxy for a Simulator"
    );
    assert!(devices(&listed).iter().any(|device| {
        device.get("id") == Some(&json!(format!("ios-wda:{ORIGINAL_UDID}")))
            && device.get("osVersion") == Some(&json!("26.4.1"))
    }));
    assert!(
        !devices(&listed)
            .iter()
            .any(|device| device.get("id") == Some(&json!(format!("ios-wda:{REPLACEMENT_UDID}"))))
    );
    assert_eq!(
        fs::read_to_string(&destination_marker)
            .expect("initial Simulator destination")
            .lines()
            .collect::<Vec<_>>(),
        vec![format!("id={ORIGINAL_UDID}")]
    );

    fs::remove_file(&launch_marker).expect("remove stale Simulator WDA marker");
    fs::write(&simulator_state, b"shutdown").expect("shutdown original Simulator");
    thread::sleep(Duration::from_millis(2_500));
    fs::write(&simulator_state, b"replacement").expect("boot replacement Simulator");
    thread::sleep(Duration::from_secs(5));
    assert_eq!(
        fs::read_to_string(&launch_count).expect("identity-pinned Simulator launch count"),
        "1",
        "the managed route must not drift to another booted Simulator"
    );
    assert!(
        !iproxy_marker.exists(),
        "Simulator recovery must not launch iproxy"
    );
    let listed = list_devices(&mut daemon);
    assert_eq!(
        devices(&listed)
            .iter()
            .filter(|device| {
                device.get("id") == Some(&json!(format!("ios-wda:{ORIGINAL_UDID}")))
            })
            .count(),
        1,
        "{listed}"
    );
    assert!(
        !devices(&listed)
            .iter()
            .any(|device| device.get("id") == Some(&json!(format!("ios-wda:{REPLACEMENT_UDID}"))))
    );

    fs::write(&simulator_state, b"original").expect("reboot original Simulator");
    let recovery_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let launches = fs::read_to_string(&launch_count)
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or_default();
        if launches >= 2 {
            break;
        }
        assert!(
            Instant::now() < recovery_deadline,
            "Simulator hot-plug recovery timed out"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        fs::read_to_string(&launch_marker).expect("recovered Simulator WDA local port"),
        local_port
    );
    assert_eq!(
        fs::read_to_string(&destination_marker)
            .expect("recovered Simulator destinations")
            .lines()
            .collect::<Vec<_>>(),
        vec![format!("id={ORIGINAL_UDID}"), format!("id={ORIGINAL_UDID}")]
    );
    assert!(
        !iproxy_marker.exists(),
        "recovered Simulator WDA must remain tunnel-free"
    );

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
    assert!(completed.stderr.contains("ios_hotplug_ready"));
    assert!(completed.stderr.contains("ios_wda_recovered"));
}

#[test]
fn managed_ios_required_missing_project_fails_with_a_redacted_stable_code() {
    let evidence = TempDir::new().expect("temporary evidence directory");
    let secret_marker = "required-managed-wda-project-secret-marker";
    let missing = evidence
        .path()
        .join(secret_marker)
        .join("WebDriverAgent.xcodeproj");
    let missing = missing.to_string_lossy().into_owned();
    let daemon = DaemonProcess::spawn(
        evidence.path(),
        &[
            ("DEVICERAIL_IOS", "required"),
            ("DEVICERAIL_IOS_WDA_PROJECT", &missing),
        ],
    );

    let completed = daemon.finish();
    assert!(!completed.status.success(), "daemon unexpectedly succeeded");
    assert!(completed.stderr.contains("ios_wda_project_missing"));
    assert!(!completed.stderr.contains(secret_marker));
    assert!(!completed.stderr.contains(&missing));
}

#[test]
fn harmony_auto_missing_hdc_preserves_the_stock_daemon() {
    let evidence = TempDir::new().expect("temporary evidence directory");
    let secret_marker = "sensitive-hdc-path-must-not-leak";
    let missing_hdc = evidence.path().join(secret_marker);
    let missing_hdc_text = missing_hdc.to_string_lossy().into_owned();
    let mut daemon = DaemonProcess::spawn(
        evidence.path(),
        &[
            ("DEVICERAIL_HARMONY", "auto"),
            ("DEVICERAIL_HDC_PATH", &missing_hdc_text),
        ],
    );

    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    assert_mock_only(&listed);
    daemon.assert_running();

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
    assert!(completed.stderr.contains("hdc_executable_not_found"));
    assert!(
        !completed.stderr.contains(secret_marker),
        "{}",
        completed.stderr
    );
    assert!(
        !completed.stderr.contains(&missing_hdc_text),
        "{}",
        completed.stderr
    );
}

#[test]
fn harmony_required_missing_hdc_fails_with_a_redacted_stable_code() {
    let evidence = TempDir::new().expect("temporary evidence directory");
    let secret_marker = "required-sensitive-hdc-path-must-not-leak";
    let missing_hdc = evidence.path().join(secret_marker);
    let missing_hdc_text = missing_hdc.to_string_lossy().into_owned();
    let daemon = DaemonProcess::spawn(
        evidence.path(),
        &[
            ("DEVICERAIL_HARMONY", "required"),
            ("DEVICERAIL_HDC_PATH", &missing_hdc_text),
        ],
    );

    let completed = daemon.finish();
    assert!(!completed.status.success(), "daemon unexpectedly succeeded");
    assert!(completed.stderr.contains("hdc_executable_not_found"));
    assert!(
        !completed.stderr.contains(secret_marker),
        "{}",
        completed.stderr
    );
    assert!(
        !completed.stderr.contains(&missing_hdc_text),
        "{}",
        completed.stderr
    );
}

#[cfg(unix)]
#[test]
fn harmony_system_hdc_registers_an_offline_route_with_closed_errors() {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    let evidence = TempDir::new().expect("temporary evidence directory");
    let fixture = TempDir::new().expect("temporary HDC fixture directory");
    let hdc = fixture.path().join("fake-hdc");
    fs::write(
        &hdc,
        "#!/bin/sh\n\
         if [ \"$1\" = \"list\" ] && [ \"$2\" = \"targets\" ] && [ \"$3\" = \"-v\" ]; then\n\
           printf '%s\\n' 'stock-harmony offline devName=StockPhone version=5.0'\n\
           exit 0\n\
         fi\n\
         printf '%s\\n' '[Fail] unexpected fake HDC command' >&2\n\
         exit 2\n",
    )
    .expect("write fake HDC executable");
    let mut permissions = fs::metadata(&hdc).expect("fake HDC metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&hdc, permissions).expect("make fake HDC executable");
    let hdc_text = hdc.to_string_lossy().into_owned();

    let mut daemon = DaemonProcess::spawn(
        evidence.path(),
        &[
            ("DEVICERAIL_HARMONY", "required"),
            ("DEVICERAIL_HDC_PATH", &hdc_text),
        ],
    );
    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    let inventory = devices(&listed);
    assert_eq!(inventory.len(), 2, "{listed}");
    assert!(
        inventory.iter().any(|device| {
            device.get("id") == Some(&json!("harmony-hdc:stock-harmony"))
                && device.pointer("/platform/kind") == Some(&json!("harmonyOs"))
                && device.get("name") == Some(&json!("StockPhone"))
                && device.get("osVersion") == Some(&json!("5.0"))
                && device.get("connected") == Some(&json!(false))
        }),
        "{listed}"
    );

    select_device(&mut daemon, "harmony-hdc:stock-harmony");
    let connected = connect_device(&mut daemon);
    assert_eq!(
        connected.pointer("/error/data/code"),
        Some(&json!("platform_error")),
        "{connected}"
    );
    assert_eq!(
        connected.pointer("/error/data/details/platformCode"),
        Some(&json!("hdc_target_offline")),
        "{connected}"
    );
    daemon.assert_running();

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
    assert!(
        !completed.stderr.contains(&hdc_text),
        "{}",
        completed.stderr
    );
}

#[test]
fn desktop_is_off_by_default_and_keeps_the_stock_inventory_unchanged() {
    let evidence = TempDir::new().expect("temporary evidence directory");
    let mut daemon = DaemonProcess::spawn(evidence.path(), &[]);

    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    assert_mock_only(&listed);
    daemon.assert_running();

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn desktop_environment_registers_one_lazy_host_native_route() {
    let evidence = TempDir::new().expect("temporary evidence directory");
    let executable = env::current_exe()
        .expect("current integration-test executable")
        .to_string_lossy()
        .into_owned();
    let environment = desktop_environment("required", &executable);
    let environment = environment_refs(&environment);
    let mut daemon = DaemonProcess::spawn(evidence.path(), &environment);

    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    let inventory = devices(&listed);
    assert_eq!(inventory.len(), 2, "{listed}");
    assert!(
        inventory.iter().any(|device| {
            device.get("id") == Some(&json!("desktop-stock-e2e"))
                && device.get("name") == Some(&json!("Stock native desktop"))
                && device.pointer("/platform/kind") == Some(&json!(desktop_platform_kind()))
                && device.get("osVersion") == Some(&json!("stock-test"))
                && device.get("connected") == Some(&json!(false))
        }),
        "{listed}"
    );
    daemon.assert_running();

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
    assert!(
        !completed.stderr.contains(&executable),
        "{}",
        completed.stderr
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn desktop_auto_and_required_missing_tool_policies_are_redacted() {
    let secret_marker = "desktop-tool-path-must-not-leak";

    let auto_evidence = TempDir::new().expect("temporary auto evidence directory");
    let missing = auto_evidence.path().join(secret_marker);
    let missing = missing.to_string_lossy().into_owned();
    let environment = desktop_environment("auto", &missing);
    let environment = environment_refs(&environment);
    let mut daemon = DaemonProcess::spawn(auto_evidence.path(), &environment);
    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    assert_mock_only(&listed);
    daemon.assert_running();
    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
    assert!(completed.stderr.contains("desktop_tool_not_found"));
    assert!(
        !completed.stderr.contains(secret_marker),
        "{}",
        completed.stderr
    );
    assert!(!completed.stderr.contains(&missing), "{}", completed.stderr);

    let required_evidence = TempDir::new().expect("temporary required evidence directory");
    let missing = required_evidence.path().join(secret_marker);
    let missing = missing.to_string_lossy().into_owned();
    let environment = desktop_environment("required", &missing);
    let environment = environment_refs(&environment);
    let daemon = DaemonProcess::spawn(required_evidence.path(), &environment);
    let completed = daemon.finish();
    assert!(!completed.status.success(), "daemon unexpectedly succeeded");
    assert!(completed.stderr.contains("desktop_tool_not_found"));
    assert!(
        !completed.stderr.contains(secret_marker),
        "{}",
        completed.stderr
    );
    assert!(!completed.stderr.contains(&missing), "{}", completed.stderr);
}

#[cfg(target_os = "linux")]
#[test]
fn desktop_linux_system_backend_connects_through_the_stock_daemon() {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    let evidence = TempDir::new().expect("temporary evidence directory");
    let fixture = TempDir::new().expect("temporary desktop fixture directory");
    let tool = fixture.path().join("fake-desktop-tool");
    fs::write(
        &tool,
        "#!/bin/sh\n\
         if [ \"$1\" = \"getdisplaygeometry\" ] && [ \"$2\" = \"--shell\" ]; then\n\
           printf '%s\\n' 'WIDTH=1280' 'HEIGHT=720'\n\
           exit 0\n\
         fi\n\
         printf '%s\\n' 'unexpected fake desktop command' >&2\n\
         exit 2\n",
    )
    .expect("write fake desktop executable");
    let mut permissions = fs::metadata(&tool)
        .expect("fake desktop metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&tool, permissions).expect("make fake desktop executable");
    let tool = tool.to_string_lossy().into_owned();
    let environment = desktop_environment("required", &tool);
    let environment = environment_refs(&environment);
    let mut daemon = DaemonProcess::spawn(evidence.path(), &environment);

    hello(&mut daemon);
    let listed = list_devices(&mut daemon);
    assert!(devices(&listed).iter().any(|device| {
        device.get("id") == Some(&json!("desktop-stock-e2e"))
            && device.pointer("/platform/kind") == Some(&json!("linux"))
            && device.get("connected") == Some(&json!(false))
    }));
    select_device(&mut daemon, "desktop-stock-e2e");
    let connected = connect_device(&mut daemon);
    assert_eq!(
        connected.pointer("/result/connected"),
        Some(&json!(true)),
        "{connected}"
    );
    daemon.assert_running();

    let completed = daemon.finish();
    assert!(completed.status.success(), "{}", completed.stderr);
    assert!(!completed.stderr.contains(&tool), "{}", completed.stderr);
}

#[cfg(unix)]
#[test]
fn distributed_stock_peer_server_routes_evidence_between_real_daemons() {
    let server_evidence = TempDir::new().expect("temporary server Evidence Store");
    let client_evidence = TempDir::new().expect("temporary client Evidence Store");
    let config_root = TempDir::new().expect("temporary distributed config root");
    let address = reserve_loopback_address();
    let node_id = "stock-peer-node";
    let secret_marker = "distributed-tunnel-secret-marker";

    let server_config = config_root.path().join("server-config-secret-marker.json");
    write_owner_only_json(
        &server_config,
        &json!({
            "schemaVersion": 1,
            "nodeId": node_id,
            "listen": address.clone(),
            "securityMode": "externalSshOrMtls",
            "tunnelId": secret_marker,
            "nodeEpoch": 17,
            "inventoryRevision": 1
        }),
    );
    let server_config_text = server_config.to_string_lossy().into_owned();
    let mut server = DaemonProcess::spawn(
        server_evidence.path(),
        &[("DEVICERAIL_DISTRIBUTED_SERVER", &server_config_text)],
    );

    hello(&mut server);
    let server_inventory = list_devices(&mut server);
    assert_mock_only(&server_inventory);
    server.assert_running();

    let peers_config = config_root.path().join("peers-config-secret-marker.json");
    write_owner_only_json(
        &peers_config,
        &json!({
            "schemaVersion": 1,
            "peers": [{
                "nodeId": node_id,
                "endpoint": address,
                "securityMode": "externalSshOrMtls",
                "tunnelId": secret_marker,
                "ownerId": secret_marker,
                "leaseTtlMs": 30000,
                "renewBeforeMs": 5000
            }]
        }),
    );
    let peers_config_text = peers_config.to_string_lossy().into_owned();
    let mut client = DaemonProcess::spawn(
        client_evidence.path(),
        &[("DEVICERAIL_DISTRIBUTED_PEERS", &peers_config_text)],
    );

    hello(&mut client);
    let listed = list_devices(&mut client);
    let inventory = devices(&listed);
    assert_eq!(inventory.len(), 2, "{listed}");
    let remote_id = inventory
        .iter()
        .find_map(|device| {
            let id = device.get("id")?.as_str()?;
            id.starts_with("remote:stock-peer-node:")
                .then(|| id.to_owned())
        })
        .unwrap_or_else(|| panic!("remote stock route was not registered: {listed}"));
    let remote = inventory
        .iter()
        .find(|device| device.get("id") == Some(&json!(&remote_id)))
        .expect("remote stock route");
    assert_eq!(remote.pointer("/platform/kind"), Some(&json!("mock")));
    assert_eq!(remote.get("connected"), Some(&json!(false)));

    select_device(&mut client, &remote_id);
    let connected = connect_device(&mut client);
    assert_eq!(
        connected.pointer("/result/id"),
        Some(&json!(&remote_id)),
        "{connected}"
    );
    assert_eq!(
        connected.pointer("/result/connected"),
        Some(&json!(true)),
        "{connected}"
    );

    let session = start_session(&mut client);
    assert_eq!(
        session.pointer("/result/state"),
        Some(&json!("active")),
        "{session}"
    );
    let observed = observe_device(&mut client);
    assert_eq!(
        observed.pointer("/result/deviceId"),
        Some(&json!(&remote_id)),
        "{observed}"
    );
    assert_eq!(
        observed.pointer("/result/screenshot/mediaType"),
        Some(&json!("image/png")),
        "{observed}"
    );
    let observation_digest = observed
        .pointer("/result/screenshot/sha256")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("remote Evidence was not imported: {observed}"));
    assert_eq!(observation_digest.len(), 64, "{observed}");

    let executed = execute_tap(&mut client);
    assert_eq!(
        executed.pointer("/result/callId"),
        Some(&json!("77777777-7777-4777-8777-777777777777")),
        "{executed}"
    );
    assert_eq!(
        executed.pointer("/result/output/accepted"),
        Some(&json!(true)),
        "{executed}"
    );
    assert_eq!(
        executed.pointer("/result/after/deviceId"),
        Some(&json!(&remote_id)),
        "{executed}"
    );
    let action_digest = executed
        .pointer("/result/evidence/0/sha256")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("remote Action Evidence was not imported: {executed}"));
    assert_eq!(action_digest.len(), 64, "{executed}");
    let client_observation = evidence_data_path(client_evidence.path(), observation_digest);
    let client_action = evidence_data_path(client_evidence.path(), action_digest);
    let server_observation = evidence_data_path(server_evidence.path(), observation_digest);
    let server_action = evidence_data_path(server_evidence.path(), action_digest);
    for path in [
        &client_observation,
        &client_action,
        &server_observation,
        &server_action,
    ] {
        assert!(
            path.is_file(),
            "Evidence object was not persisted: {path:?}"
        );
    }
    assert_eq!(
        fs::read(&client_observation).expect("read imported client Evidence"),
        fs::read(&server_observation).expect("read source server Evidence"),
        "imported observation bytes must equal the independently stored source",
    );
    assert_eq!(
        fs::read(&client_action).expect("read imported client Action Evidence"),
        fs::read(&server_action).expect("read source server Action Evidence"),
        "imported Action bytes must equal the independently stored source",
    );

    let client_completed = client.finish();
    assert!(
        client_completed.status.success(),
        "{}",
        client_completed.stderr
    );
    assert!(
        !client_completed.stderr.contains(secret_marker),
        "{}",
        client_completed.stderr
    );
    assert!(
        !client_completed.stderr.contains(&peers_config_text),
        "{}",
        client_completed.stderr
    );
    server.assert_running();

    let server_completed = server.finish();
    assert!(
        server_completed.status.success(),
        "{}",
        server_completed.stderr
    );
    assert!(
        !server_completed.stderr.contains(secret_marker),
        "{}",
        server_completed.stderr
    );
    assert!(
        !server_completed.stderr.contains(&server_config_text),
        "{}",
        server_completed.stderr
    );
}

#[cfg(unix)]
#[test]
fn distributed_stock_peers_converge_from_bidirectional_mandatory_cold_start() {
    let evidence_a = TempDir::new().expect("temporary node A Evidence Store");
    let evidence_b = TempDir::new().expect("temporary node B Evidence Store");
    let config_root = TempDir::new().expect("temporary distributed config root");
    let (address_a, address_b) = reserve_two_loopback_addresses();
    let node_a = "cold-node-a";
    let node_b = "cold-node-b";
    let secret_a = "cold-tunnel-a-secret-marker";
    let secret_b = "cold-tunnel-b-secret-marker";

    let server_config_a = config_root.path().join("server-a-secret-marker.json");
    let peers_config_a = config_root.path().join("peers-a-secret-marker.json");
    let server_config_b = config_root.path().join("server-b-secret-marker.json");
    let peers_config_b = config_root.path().join("peers-b-secret-marker.json");
    write_owner_only_json(
        &server_config_a,
        &json!({
            "schemaVersion": 1,
            "nodeId": node_a,
            "listen": address_a,
            "securityMode": "externalSshOrMtls",
            "tunnelId": secret_a,
            "nodeEpoch": 31,
            "inventoryRevision": 1
        }),
    );
    write_owner_only_json(
        &peers_config_a,
        &json!({
            "schemaVersion": 1,
            "peers": [{
                "nodeId": node_b,
                "endpoint": address_b,
                "securityMode": "externalSshOrMtls",
                "tunnelId": secret_b,
                "ownerId": secret_b,
                "leaseTtlMs": 30000,
                "renewBeforeMs": 5000
            }]
        }),
    );
    write_owner_only_json(
        &server_config_b,
        &json!({
            "schemaVersion": 1,
            "nodeId": node_b,
            "listen": address_b,
            "securityMode": "externalSshOrMtls",
            "tunnelId": secret_b,
            "nodeEpoch": 47,
            "inventoryRevision": 1
        }),
    );
    write_owner_only_json(
        &peers_config_b,
        &json!({
            "schemaVersion": 1,
            "peers": [{
                "nodeId": node_a,
                "endpoint": address_a,
                "securityMode": "externalSshOrMtls",
                "tunnelId": secret_a,
                "ownerId": secret_a,
                "leaseTtlMs": 30000,
                "renewBeforeMs": 5000
            }]
        }),
    );

    let server_config_a = server_config_a.to_string_lossy().into_owned();
    let peers_config_a = peers_config_a.to_string_lossy().into_owned();
    let server_config_b = server_config_b.to_string_lossy().into_owned();
    let peers_config_b = peers_config_b.to_string_lossy().into_owned();

    let mut daemon_a = DaemonProcess::spawn(
        evidence_a.path(),
        &[
            ("DEVICERAIL_DISTRIBUTED_SERVER", &server_config_a),
            ("DEVICERAIL_DISTRIBUTED_PEERS", &peers_config_a),
        ],
    );
    let mut daemon_b = DaemonProcess::spawn(
        evidence_b.path(),
        &[
            ("DEVICERAIL_DISTRIBUTED_SERVER", &server_config_b),
            ("DEVICERAIL_DISTRIBUTED_PEERS", &peers_config_b),
        ],
    );

    hello(&mut daemon_a);
    hello(&mut daemon_b);

    let listed_a = list_devices(&mut daemon_a);
    let inventory_a = devices(&listed_a);
    assert_eq!(inventory_a.len(), 2, "{listed_a}");
    assert!(
        inventory_a
            .iter()
            .any(|device| device.get("id") == Some(&json!("mock-1"))),
        "{listed_a}"
    );
    assert!(
        inventory_a.iter().any(|device| {
            device
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("remote:cold-node-b:"))
                && device.get("connected") == Some(&json!(false))
        }),
        "{listed_a}"
    );

    let listed_b = list_devices(&mut daemon_b);
    let inventory_b = devices(&listed_b);
    assert_eq!(inventory_b.len(), 2, "{listed_b}");
    assert!(
        inventory_b
            .iter()
            .any(|device| device.get("id") == Some(&json!("mock-1"))),
        "{listed_b}"
    );
    assert!(
        inventory_b.iter().any(|device| {
            device
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("remote:cold-node-a:"))
                && device.get("connected") == Some(&json!(false))
        }),
        "{listed_b}"
    );
    daemon_a.assert_running();
    daemon_b.assert_running();

    daemon_a.close_stdin();
    daemon_b.close_stdin();
    let completed_a = daemon_a.finish();
    let completed_b = daemon_b.finish();
    for (completed, label) in [(completed_a, "node A"), (completed_b, "node B")] {
        assert!(
            completed.status.success(),
            "{label} failed: {}",
            completed.stderr
        );
        for secret in [
            secret_a,
            secret_b,
            &server_config_a,
            &peers_config_a,
            &server_config_b,
            &peers_config_b,
        ] {
            assert!(
                !completed.stderr.contains(secret),
                "{label} leaked distributed configuration: {}",
                completed.stderr
            );
        }
    }
}

#[cfg(not(unix))]
#[test]
fn distributed_stock_peer_server_config_fails_closed_without_owner_only_acl_proof() {
    let evidence = TempDir::new().expect("temporary Evidence Store");
    let config_root = TempDir::new().expect("temporary distributed config root");
    let address = reserve_loopback_address();
    let secret_marker = "distributed-non-unix-secret-marker";
    let config = config_root.path().join("server-config-secret-marker.json");
    fs::write(
        &config,
        serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "nodeId": "stock-peer-node",
            "listen": address,
            "securityMode": "externalSshOrMtls",
            "tunnelId": secret_marker,
            "nodeEpoch": 17,
            "inventoryRevision": 1
        }))
        .expect("serialize distributed server config"),
    )
    .expect("write distributed server config");
    let config_text = config.to_string_lossy().into_owned();
    let daemon = DaemonProcess::spawn(
        evidence.path(),
        &[("DEVICERAIL_DISTRIBUTED_SERVER", &config_text)],
    );
    let completed = daemon.finish();
    assert!(!completed.status.success(), "daemon unexpectedly succeeded");
    let diagnostic = completed.stderr.to_ascii_lowercase();
    assert!(
        diagnostic.contains("distributed") && diagnostic.contains("server"),
        "{}",
        completed.stderr
    );
    assert!(
        !completed.stderr.contains(secret_marker),
        "{}",
        completed.stderr
    );
    assert!(
        !completed.stderr.contains(&config_text),
        "{}",
        completed.stderr
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn desktop_environment(mode: &'static str, tool: &str) -> Vec<(&'static str, String)> {
    let mut environment = vec![
        ("DEVICERAIL_DESKTOP", mode.to_owned()),
        ("DEVICERAIL_DESKTOP_ID", "desktop-stock-e2e".to_owned()),
        ("DEVICERAIL_DESKTOP_NAME", "Stock native desktop".to_owned()),
        ("DEVICERAIL_DESKTOP_OS_VERSION", "stock-test".to_owned()),
    ];
    #[cfg(target_os = "macos")]
    environment.push(("DEVICERAIL_DESKTOP_MACOS_SCREENCAPTURE", tool.to_owned()));
    #[cfg(target_os = "windows")]
    environment.push(("DEVICERAIL_DESKTOP_WINDOWS_POWERSHELL", tool.to_owned()));
    #[cfg(target_os = "linux")]
    environment.extend([
        ("DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER", "x11".to_owned()),
        ("DEVICERAIL_DESKTOP_X11_IMPORT", tool.to_owned()),
        ("DEVICERAIL_DESKTOP_X11_XDOTOOL", tool.to_owned()),
    ]);
    environment
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn environment_refs<'a>(environment: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    environment
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

#[cfg(target_os = "macos")]
const fn desktop_platform_kind() -> &'static str {
    "macOs"
}

#[cfg(target_os = "windows")]
const fn desktop_platform_kind() -> &'static str {
    "windows"
}

#[cfg(target_os = "linux")]
const fn desktop_platform_kind() -> &'static str {
    "linux"
}
