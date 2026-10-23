# gpui-global-hotkey

System-wide **global hotkeys** for [GPUI](https://gpui.rs) applications: keyboard
shortcuts that fire even when your app is in the background or has no focused
window. OS key events are bridged back to the GPUI main thread and dispatched
either as a GPUI [`Action`] or through a plain callback.

This crate is an **independent repository** (like `gpui-autostart` /
`gpui-tray`), mounted into [`gpui-kit`](https://github.com/kkkqkx123/gpui-kit)
through a git submodule at `crates/global-hotkey`. It is *not* a member of the
`gpui-kit` workspace.

## How it works

```
  GPUI main thread                         OS callback thread (non-GPUI)
  ─────────────────                         ───────────────────────────
  GlobalHotkey::new(cx)
    • create manager (global-hotkey)
    • flume channel + bridge task            Windows  WM_HOTKEY
    • registry id → Registration            macOS    Carbon event
  register / on_hotkey / unregister         Linux    x11rb XGrabKey
  dispatch via cx.spawn + AsyncApp::update
    → App::dispatch_action / callback
```

The native platform threads only ever `send` events; all GPUI state access
(`dispatch_action`, callbacks) happens on the main thread via a `flume` bridge,
exactly like `gpui-tray`. The OS grabs are performed by the cross-platform
[`global-hotkey`](https://github.com/tauri-apps/global-hotkey) crate, so this
crate ships Windows / macOS / Linux(X11) support out of the box (plus a no-op
fallback: when the platform cannot provide hotkeys, `is_available()` is `false`
and every registration call returns an error instead of hanging, so your app
still starts).

## GPUI flavors

Select exactly one flavor (disable default features to pick another):

| feature     | GPUI source                       |
| ----------- | --------------------------------- |
| `gpui-kit`  | `gpui-pre` 0.3.9 (GPUI Kit) — default |
| `gpui`      | upstream Zed GPUI (git rev)       |
| `gpui-ce`   | GPUI Community Edition 0.3.0      |

The selected flavor must match the GPUI your host app builds against.

## Usage

```rust,ignore
use gpui_global_hotkey::{GlobalHotkey, HotKey};
use gpui_kit::*;

actions!(demo, [ShowWindow, Quit]);

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        let hk = GlobalHotkey::new(cx).expect("global hotkey");

        // Action form: reaches `App::on_action` even when unfocused.
        let _r1 = hk.on_hotkey(HotKey::from_str("ctrl+alt+g").unwrap(), ShowWindow);

        // Callback form, aware of Pressed / Released.
        let _r2 = hk.on_hotkey_with(HotKey::from_str("ctrl+alt+q").unwrap(), |cx, ev| {
            if ev.state == gpui_global_hotkey::HotkeyState::Pressed { cx.quit(); }
        });
    });
}
```

`HotKey` parses accelerators (`"ctrl+shift+space"`, `"cmdorctrl+g"`), converts
from a GPUI `Keystroke`, and renders via `Display`. The `hotkey!` macro builds
one from flags: `hotkey!(control | alt => "g")`.

## Platform notes

- **Linux**: X11 only. A Wayland session without XWayland is detected up front
  via an X11 connectivity probe, so `is_available() == false` and registration
  fails with an error (it never blocks on a dead backend thread). X11 delivers
  **both** `Pressed` and `Released`.
- **macOS**: Carbon `RegisterEventHotKey` — no Accessibility permission needed.
- **Windows**: `RegisterHotKey` on the main thread's message loop.
- Every `GlobalHotkey` handle in a process shares **one** process-wide service
  (manager + registry + bridge task). The service is created on the first `new`
  call and is resident for the process lifetime, because the underlying event
  handler is a process-global `OnceCell` that cannot be reset.

## License

Apache-2.0
