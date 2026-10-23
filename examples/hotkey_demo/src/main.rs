//! Global-hotkey demo: register a system-wide shortcut (`Ctrl+Alt+G`) that shows
//! and focuses the main window even when the app is in the background. Run with
//! `cargo run --example hotkey_demo` on a desktop session (X11 on Linux, the
//! menu bar on macOS, the taskbar on Windows).
//!
//! Background test: launch the demo, click away to another app, then press the
//! shortcut — the window should re-appear/activate.

use gpui_global_hotkey::{GlobalHotkey, HotKey, HotkeyState};
use gpui_kit::*;

actions!(hotkey_demo, [ShowWindow, Quit]);

struct DemoApp {
    visible: bool,
}

impl Render for DemoApp {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child("GPUI Global Hotkey Demo")
            .child("Press Ctrl+Alt+G from any app to show this window.")
            .child(format!("Visible: {}", self.visible))
    }
}

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);

        let hk = GlobalHotkey::new(cx).expect("failed to create global hotkey");
        println!("global hotkey available: {}", hk.is_available());

        // Registered once; reaching `App::on_action` even when no window is focused.
        let _reg = hk
            .on_hotkey(HotKey::from_str("ctrl+alt+g").unwrap(), ShowWindow)
            .expect("failed to register hotkey");

        // Callback form, aware of Pressed / Released.
        let _reg2 = hk
            .on_hotkey_with(HotKey::from_str("ctrl+alt+q").unwrap(), |cx, ev| {
                if ev.state == HotkeyState::Pressed {
                    cx.quit();
                }
            });

        // App-level listener: the window-less fallback for background triggers.
        // `App::activate` brings the application (and its windows) to the
        // foreground even when the app is unfocused; open the window only if no
        // window exists yet.
        cx.on_action(|_: &ShowWindow, cx: &mut App| {
            cx.activate(true);
            if cx.active_window().is_none() {
                let _ = gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                    cx.new(|_| DemoApp { visible: true })
                });
            }
        });

        // Open the initial window.
        let _window = gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
            cx.new(|_| DemoApp { visible: true })
        })
        .expect("failed to open window");

        println!("Registered Ctrl+Alt+G (show) and Ctrl+Alt+Q (quit).");
    });
}
