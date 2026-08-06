# `devicerail-android-adb`

Bounded Android Debug Bridge support for DeviceRail.

DR-012 uses the host's existing `adb` executable; it never bundles or
downloads Android platform tools. Discovery parses `adb devices -l` into
stable `DeviceInfo` values, and each lifecycle command is routed with an
explicit serial. Unauthorized, offline, missing, permission-denied, malformed
output, process failure, timeout, and cancellation paths return explicit
errors.

```rust,no_run
use devicerail_android_adb::{AndroidAdb, AndroidDeviceConfig, SystemAdbConfig};
use devicerail_core::DeviceDriver;
use devicerail_core::ExecutionControl;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let adb = AndroidAdb::system(SystemAdbConfig::default())?;
let report = adb.discover(&ExecutionControl::unbounded()).await?;
let descriptor = report
    .devices
    .into_iter()
    .next()
    .ok_or_else(|| std::io::Error::other("no Android device"))?;
let driver = adb.driver(descriptor, AndroidDeviceConfig::default())?;
let info = driver.connect(&ExecutionControl::unbounded()).await?;
println!("connected to {}", info.id);
# Ok(())
# }
```

The replaceable command boundary is crate-private so callers cannot bypass
the logical lifecycle with raw ADB operations. Its system implementation uses
argument vectors rather than a host shell, caps stdout and stderr, applies a
finite 65-second default timeout (covering the advertised 60-second maximum
swipe plus process overhead), kills a child when its future is dropped, and
never runs global `kill-server`, `disconnect`, or unscoped device mutation
commands.

`AndroidDriver` implements DeviceRail's complete shared Driver contract. It
advertises exactly eleven closed actions: `tap`, `keyPress`, `swipe`, `scroll`,
`inputText`, `launch`, `terminate`, `back`, `home`, `recentApps`, and the
protected `inputSecret`. Standard Actions validate their schema, capture a
Session-owned before observation, execute one typed serial-scoped ADB
operation, and capture a Session-owned after observation. The result returns
both snapshots and their de-duplicated evidence references. The crate runs
Core's four-input conformance suite with a real filesystem Evidence Store,
including strict operation-receipt reconciliation.

`keyPress` is intentionally limited to editing and directional keys. Back,
home, and recent-apps navigation use independent, auditable actions; power and
volume are not exposed. `inputText` accepts 1–1024 bytes from a small ASCII
allowlist. Caller-provided `%`, Unicode, control bytes, quotes, and remote-shell
metacharacters are rejected; the encoder alone converts an allowed space to
Android's `%s` representation. Text-bearing command values use redacted Debug
output, and public Driver failures expose only stable platform codes.

`inputSecret` accepts 1–1024 printable ASCII bytes and rejects Android input's
reserved `%s` sequence without changing `inputText` semantics. Its argument is
moved into a non-clone, redacted-debug buffer and zeroed when dropped. The only
host argument vector is serial-scoped `adb shell -T` plus the fixed remote
script `IFS= read -r DEVICERAIL_SECRET && input text
"$DEVICERAIL_SECRET"`; the secret and one newline travel only through child
stdin. Protected stdout and stderr are discarded after connectivity
classification and never appear in results, debug output, or public errors.
The result is only `{ "accepted": true }`, with no value or length metadata.

Protected Actions never capture or store screenshots. Their before and after
observations use only `wm size` and `wm density`, set
`screenshotOmission: "protectedAction"`, and return no evidence references.
The runtime-wide omit policy applies the same display-only path to ordinary
observations and Actions with `screenshotOmission: "policy"`; capture mode for
ordinary operations is unchanged.

`launch` and `terminate` accept only 3–255 byte Android application ids with
at least two dot-separated ASCII segments. Each segment begins with a letter
and continues with letters, digits, or underscore, so package input cannot
become a shell fragment. Launch resolves the package's current-user
MAIN/LAUNCHER component on-device with `cmd package resolve-activity` and
starts it explicitly through `am start -W -n`, so launcher activities that
omit `android.intent.category.DEFAULT` still start; the remote line is fixed
apart from the grammar-constrained package id. The wait output accepts only
AOSP's ordered `am start -W` fields through `Complete`, including the legacy
`ThisTime` and current `LaunchState` forms. Only the two
documented already-running/top-task warnings are accepted; unknown non-empty
lines and explicit failure output fail closed even when `Status: ok` is also
present. Every other mutation requires empty stdout and permits only ADB
daemon-start chatter on stderr; command output and package values never enter
Action results or public errors.

Observation accepts at most 32 MiB of encoded PNG, 16,384 pixels per side,
33,554,432 pixels total, and 128 MiB of decoded frame data. The pure-Rust
`png` decoder reads every scanline and the complete trailer with CRC and Adler
verification; text and ICC payloads are skipped, APNG is rejected, and the
viewport always comes from the decoded frame. Android display metadata uses
the same dimension and pixel ceilings and caps density at 10,000 dpi.

Each device also has an internal read/write operation gate. An observation
holds a read lease from the connected-state check through its Session evidence
pin. Connect, health, and disconnect take a control-aware write lease. An
Action holds that exclusive lease across before capture, mutation, and after
capture, so observations and lifecycle transitions cannot cross an Action.
Cancellation and deadlines also interrupt gate waits and in-flight ADB work;
the lifecycle mutex itself is held only for short state reads and updates.
Missing, offline, unauthorized, and host-permission failures invalidate cached
connected state immediately. A later `connect` must probe and recover the
transport; generic process failures, cancellation, and timeout do not
incorrectly mark an otherwise reachable device disconnected.
