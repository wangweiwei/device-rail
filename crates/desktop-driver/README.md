# DeviceRail Desktop Driver

`devicerail-desktop-driver` implements the DeviceRail `DeviceDriver` contract for:

- macOS screen capture and native Quartz input;
- Windows virtual-desktop capture and Win32 input APIs (`SendInput` for Unicode text plus cursor, keyboard, and mouse event APIs for keys, pointers, and wheels);
- Linux X11 capture/input through ImageMagick `import` and `xdotool`;
- Linux Wayland capture through `grim`, with either full `ydotool` input or the deliberately smaller `wtype` keyboard/text capability set.

The crate does not install tools, open permission prompts, or silently switch between display protocols. Discovery returns an explicit error when a configured executable is missing, Linux session detection is ambiguous, or the Wayland viewport is absent.

## macOS permissions

macOS probes the current process with the non-prompting CoreGraphics Screen Recording and Accessibility preflight APIs. `connect` fails with one of these stable platform codes until both permissions are granted:

- `desktop_macos_screen_recording_required`
- `desktop_macos_accessibility_required`

The daemon process itself must be present in both macOS Privacy & Security lists. Granting permission only to an interactive terminal does not grant it to a separately launched service.

## Linux capability truthfulness

X11 and Wayland are separate profiles:

| Session | Capture | Input backend | Advertised actions |
| --- | --- | --- | --- |
| X11 | `import` | `xdotool` | `tap`, `inputText`, `keyPress`, `scroll` |
| Wayland | `grim` | `ydotool` | `tap`, `inputText`, `keyPress`, `scroll` |
| Wayland | `grim` | `wtype` | `inputText`, `keyPress` |

`wtype` never advertises pointer actions. DeviceRail also requires an explicit Wayland `Viewport`: inferring it by taking an unrequested screenshot would violate screenshot-omission policy. The configured dimensions must match the PNG produced by `grim`, otherwise the observation fails explicitly.

Full Wayland input through `ydotool` also requires a running `ydotoold` with access to `/dev/uinput` and a socket visible to the daemon process. Executable discovery does not claim that daemon/socket health is permanent: a missing or inaccessible runtime backend is returned as an explicit input command failure rather than being hidden behind the smaller `wtype` profile.

## Construction

```rust,no_run
use devicerail_core::ExecutionControl;
use devicerail_desktop_driver::{
    DesktopIdentity, NativeDesktopDriver, SystemDesktopConfig, discover_native_driver,
};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let identity = DesktopIdentity::new(
    "desktop-local",
    "Local desktop",
    None,
);
let mut config = SystemDesktopConfig::default();

// Required for Wayland. Use the same physical-pixel coordinate space as grim.
// config.wayland_viewport = Some(devicerail_protocol::Viewport {
//     width: 1920,
//     height: 1080,
//     scale_factor: 1.0,
// });

let native = discover_native_driver(
    identity,
    config,
    &ExecutionControl::unbounded(),
).await?;
let driver = native.into_driver();
# let _ = driver;
# Ok(())
# }
```

## Stock daemon wiring

The stock daemon uses `DEVICERAIL_DESKTOP=auto|off|required`, defaulting to
`off`. Only `auto` and `required` attempt native discovery, and one process can
register only the route for its compile-time host. `auto` logs a stable local
diagnostic and preserves other routes after discovery or registration failure;
`required` fails startup. Setting Desktop auxiliary variables while the mode is
disabled is rejected instead of being silently ignored.

Common optional settings are:

- `DEVICERAIL_DESKTOP_ID`, `DEVICERAIL_DESKTOP_NAME`, and
  `DEVICERAIL_DESKTOP_OS_VERSION` for bounded route metadata; ID and name
  default to `desktop-local` and `Local desktop`, respectively;
- `DEVICERAIL_DESKTOP_COMMAND_TIMEOUT_MS` for a 1–300000 ms command ceiling,
  defaulting to 30000 ms;
- `DEVICERAIL_DESKTOP_MACOS_SCREENCAPTURE`, defaulting to
  `/usr/sbin/screencapture` on macOS;
- `DEVICERAIL_DESKTOP_WINDOWS_POWERSHELL`, defaulting to `powershell.exe` on
  Windows;
- `DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER=x11|wayland`, X11 tool settings
  `DEVICERAIL_DESKTOP_X11_IMPORT` and `DEVICERAIL_DESKTOP_X11_XDOTOOL`, and
  Wayland tool settings `DEVICERAIL_DESKTOP_WAYLAND_GRIM`,
  `DEVICERAIL_DESKTOP_WAYLAND_YDOTOOL`, and
  `DEVICERAIL_DESKTOP_WAYLAND_WTYPE`;
- `DEVICERAIL_DESKTOP_WAYLAND_INPUT=auto|ydotool|wtype`, plus the all-or-none
  physical-pixel fields `DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_WIDTH`,
  `DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_HEIGHT`, and
  `DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_SCALE_FACTOR`. A Wayland route requires
  both an explicit `DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER=wayland` and this
  complete viewport; leaving the display-server setting unset cannot bypass
  the requirement.

Registration resolves the configured profile and tools but performs no capture
or input. The first `device.connect` and later health checks perform the
host-specific profile, permission, and viewport probes; observation and Action
execution exercise capture and input tools. On macOS, grant
Screen Recording and Accessibility to the daemon executable itself. On Windows,
run the daemon in the intended interactive user session; a Session 0 service
cannot control that user's desktop. X11 services need the matching `DISPLAY`
and `XAUTHORITY`; Wayland services need `WAYLAND_DISPLAY` and
`XDG_RUNTIME_DIR`. Full Wayland input also requires a separately running
`ydotoold` with `/dev/uinput` access. DeviceRail installs or starts none of
these external components.

Observations persist PNG bytes through the operation-scoped Evidence Store, and
the stock daemon injects its one process-wide Store into the registered native
route. The daemon configuration example is a reference only; it is never loaded
automatically.

For tests and alternative native integrations, construct `MacOsDriver`, `WindowsDriver`, or `LinuxDriver` with an injected `Arc<dyn DesktopBackend>`. The backend boundary independently covers permission probing, viewport probing, screenshot capture, and input execution.

## Verification

```text
cargo test -p devicerail-desktop-driver
cargo clippy -p devicerail-desktop-driver --all-targets -- -D warnings
```

Each public platform Driver runs the shared `driver_conformance_test!` suite with a fake backend and a real Evidence Store. Linux runs it separately for X11, Wayland/`ydotool`, and Wayland/`wtype`, so the reduced capability profile is exercised rather than inferred. Additional tests cover macOS permission failures, viewport changes between operations, screenshot omission, and typed platform identity.

Daemon configuration tests cover the closed startup contract and auto/required
failure policy. Stock binary tests cover environment configuration through
`system.hello` and `devices.list`; host-tool fixtures do not claim real TCC,
interactive Windows, X server, Wayland compositor, `ydotoold`, or `/dev/uinput`
laboratory E2E.
