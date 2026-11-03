//! Windowless verification probe for `gpui-global-hotkey`.
//!
//! No window is opened; every mode exits with a status code so scripted checks
//! (xvfb + xdotool on Linux, manual runs elsewhere) can assert on it:
//!
//! ```sh
//! cargo run -p hotkey_probe -- --probe
//! cargo run -p hotkey_probe -- --register [--hotkey ctrl+alt+g] [--timeout 10]
//! cargo run -p hotkey_probe -- --unregister [--hotkey ctrl+alt+g]
//! cargo run -p hotkey_probe -- --conflict [--hotkey ctrl+alt+g]
//! ```
//!
//! Exit codes: 0 = the asserted behavior held, 1 = it did not (timeout, a
//! duplicate registration unexpectedly succeeded, …), 2 = bad usage.

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gpui_global_hotkey::{GlobalHotkey, HotKey};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mode = match args.next().as_deref() {
        Some(m) if m.starts_with("--") && m != "--help" && m != "-h" => {
            m.trim_start_matches("--").to_string()
        }
        _ => {
            eprintln!(
                "usage: hotkey_probe --probe | --register | --unregister | --conflict \
                 [--hotkey KEY] [--timeout SECS]"
            );
            return ExitCode::from(2);
        }
    };
    if mode == "help" {
        eprintln!(
            "usage: hotkey_probe --probe | --register | --unregister | --conflict \
             [--hotkey KEY] [--timeout SECS]"
        );
        return ExitCode::from(2);
    }

    let mut hotkey_spec: Option<String> = None;
    let mut timeout = 10u64;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--hotkey" => match args.next() {
                Some(v) => hotkey_spec = Some(v),
                None => {
                    eprintln!("--hotkey requires a value");
                    return ExitCode::from(2);
                }
            },
            "--timeout" => match args.next().and_then(|v| v.parse().ok()) {
                Some(v) => timeout = v,
                None => {
                    eprintln!("--timeout requires a number");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("unknown argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }

    let hotkey_spec = hotkey_spec.unwrap_or_else(|| "ctrl+alt+g".into());

    // Observed outcome, shared with the code running inside the GPUI event
    // loop: `application().run` does not propagate the closure's return value,
    // so the closure stores its exit code here.
    let outcome = Arc::new(AtomicI32::new(0));
    let outcome_in_loop = outcome.clone();
    // Whether the hotkey event arrived (`--register` mode).
    let fired = Arc::new(AtomicBool::new(false));

    let fired_in_loop = fired.clone();
    let spec_in_loop = hotkey_spec.clone();
    let mode_in_loop = mode.clone();
    gpui_kit::application().run(move |cx| {
        let code = run_mode(&mode_in_loop, &spec_in_loop, timeout, &fired_in_loop, cx);
        outcome_in_loop.store(code, Ordering::SeqCst);
        if mode_in_loop != "register" || code != 0 {
            // In register mode a zero code means "registered, waiting for the
            // event"; the loop keeps running until the event or the timeout.
            cx.quit();
        }
    });

    let code = outcome.load(Ordering::SeqCst);
    if mode == "register" && code == 0 && !fired.load(Ordering::SeqCst) {
        // The loop ended without an event and without the timeout thread
        // failing first (e.g. something else quit the app): treat as failure.
        eprintln!("event loop ended before any hotkey event arrived");
        return ExitCode::FAILURE;
    }
    ExitCode::from(code.clamp(0, 255) as u8)
}

/// Run one probe mode inside the GPUI event loop; returns the exit code.
fn run_mode(
    mode: &str,
    hotkey_spec: &str,
    timeout: u64,
    fired: &Arc<AtomicBool>,
    cx: &mut gpui_kit::App,
) -> i32 {
    let hk = match GlobalHotkey::new(cx) {
        Ok(hk) => hk,
        Err(error) => {
            eprintln!("failed to create global hotkey service: {error}");
            return 1;
        }
    };

    let hotkey = match HotKey::from_str(hotkey_spec) {
        Ok(hk) => hk,
        Err(error) => {
            eprintln!("invalid hotkey {hotkey_spec:?}: {error}");
            return 2;
        }
    };

    match mode {
        "probe" => {
            let available = hk.is_available();
            println!("global hotkey available: {available} (platform probe mode)");
            if available {
                0
            } else {
                1
            }
        }
        "register" => {
            if !hk.is_available() {
                eprintln!("backend unavailable; nothing to register against");
                return 1;
            }
            let fired_in_callback = fired.clone();
            let registration = hk.on_hotkey_with(hotkey, move |cx, ev| {
                println!("hotkey event received: state={ev:?}");
                fired_in_callback.store(true, Ordering::SeqCst);
                cx.quit();
            });
            match registration {
                Ok(_reg) => {
                    // Keep the registration alive for the whole run: the
                    // handle unregisters on drop, so forget it instead of
                    // letting it die at the end of this closure.
                    std::mem::forget(_reg);
                    let fired_for_thread = fired.clone();
                    let deadline = Duration::from_secs(timeout);
                    std::thread::spawn(move || {
                        std::thread::sleep(deadline);
                        // Only fail if the loop is still waiting for the
                        // event; if it already fired, the app is exiting.
                        if !fired_for_thread.load(Ordering::SeqCst) {
                            eprintln!("timeout: no hotkey event within {deadline:?}");
                            std::process::exit(1);
                        }
                    });
                    println!("registered {hotkey_spec}; press it within {timeout}s (app is windowless)");
                    0
                }
                Err(error) => {
                    eprintln!("registration failed: {error}");
                    1
                }
            }
        }
        "unregister" => {
            if !hk.is_available() {
                eprintln!("backend unavailable; nothing to register against");
                return 1;
            }
            // register -> unregister -> re-register: the second registration
            // only succeeds if the first grab was actually released.
            let first = hk.on_hotkey_with(hotkey.clone(), |_, _| {});
            match first {
                Ok(reg) => reg.unregister(),
                Err(error) => {
                    eprintln!("initial registration failed: {error}");
                    return 1;
                }
            }
            match hk.register(hotkey) {
                Ok(_reg) => {
                    std::mem::forget(_reg);
                    println!("unregister + re-register of {hotkey_spec} succeeded");
                    0
                }
                Err(error) => {
                    eprintln!("re-register after unregister failed: {error}");
                    1
                }
            }
        }
        "conflict" => {
            if !hk.is_available() {
                eprintln!("backend unavailable; nothing to register against");
                return 1;
            }
            let first = hk.on_hotkey_with(hotkey.clone(), |_, _| {});
            if let Err(error) = first {
                eprintln!("first registration failed: {error}");
                return 1;
            }
            match hk.register(hotkey) {
                Ok(_reg) => {
                    std::mem::forget(_reg);
                    eprintln!("duplicate registration unexpectedly succeeded for {hotkey_spec}");
                    1
                }
                Err(error) => {
                    println!("duplicate registration rejected as expected: {error}");
                    0
                }
            }
        }
        other => {
            eprintln!("unknown mode {other:?}");
            2
        }
    }
}
