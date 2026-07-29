# `devicerail-harmony-hdc`

Typed HarmonyOS HDC discovery and Driver support for DeviceRail (DR-031).

The crate uses an explicitly configured, already-installed `hdc` executable;
it does not download DevEco Studio, an SDK, or device-side packages. Discovery
uses `hdc list targets -v`, retains the HDC connect key as the stable routing
identity, and reports offline, unauthorized, duplicate, and malformed target
records as explicit states or errors.

`HarmonyHdcDriver` implements the shared DeviceRail Driver contract. A
successful observation captures a bounded PNG with `uitest screenCap`, parses
a bounded `uitest dumpLayout` hierarchy, stores the PNG through the
Session-owned Evidence Store, and publishes the hierarchy in observation
metadata. Screenshot-omission policy still probes and parses the hierarchy and
sets the typed omission reason.

The advertised action set is deliberately closed: `tap`, `swipe`,
`inputText`, `keyPress`, and `launch`. Every action validates its JSON arguments
before a typed `HdcOperation` is dispatched. Target ids, text, bundle names,
ability names, and keys are bounded and cannot become arbitrary remote shell
fragments. The production runner invokes HDC directly with argument vectors;
there is no host shell or generic shell operation in its public boundary.
Tap and swipe coordinates are also checked against the viewport from the real
before-action observation immediately before HDC dispatch.

```rust,no_run
use devicerail_core::ExecutionControl;
use devicerail_harmony_hdc::{HarmonyHdc, SystemHdcConfig};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let hdc = HarmonyHdc::system(SystemHdcConfig::default())?;
let report = hdc.discover(&ExecutionControl::unbounded()).await?;
let descriptor = report.devices.into_iter().next()
    .ok_or_else(|| std::io::Error::other("no HarmonyOS target"))?;
let driver = hdc.driver(descriptor);
println!("discovered {}", driver.id());
# Ok(())
# }
```

## Stock daemon wiring

The stock daemon keeps HarmonyOS disabled by default. Set
`DEVICERAIL_HARMONY=auto|required` to authorize one startup discovery;
`DEVICERAIL_HDC_PATH` optionally selects the already-installed executable and
defaults to `hdc`. Supplying a path while the adapter is disabled is a startup
error. `auto` retains other routes after discovery failure or an empty target
set; `required` makes initialization, discovery, empty inventory, or route
registration failure fatal.

```sh
DEVICERAIL_ANDROID=off \
DEVICERAIL_HARMONY=required \
DEVICERAIL_HDC_PATH=/absolute/path/to/hdc \
cargo run -p devicerail-daemon
```

The daemon constructs separate five-second discovery and 65-second runtime
runners, sorts connect-key descriptors, and registers offline or unauthorized
targets as disconnected routes so `device.connect` can return their exact
state. Discovery happens only during process startup, never during
`system.hello` or `devices.list`. No protocol DTO changes are required: each
route reports `Platform::HarmonyOs`.
