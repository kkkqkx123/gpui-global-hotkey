//! # gpui-global-hotkey
//!
//! System-wide **global hotkeys** for [GPUI](https://gpui.rs) applications: shortcuts
//! that fire even when the app is in the background or has no focused window.
//! OS key events are bridged back to the GPUI main thread and dispatched either
//! as a GPUI [`Action`](gpui::Action) or through a plain callback.
//!
//! The architecture follows [`gpui-tray`](https://github.com/kkkqkx123/gpui-tray):
//! a thin wrapper over the cross-platform [`global-hotkey`] crate registers the
//! OS grabs, and a `flume` channel carries events across the thread boundary to a
//! bridge task that runs on the GPUI main thread (`cx.spawn` + `AsyncApp::update`
//! + `App::dispatch_action`). The native platform threads only ever `send` events;
//! all GPUI state access happens on the main thread.
//!
//! ## Process-wide singleton
//!
//! The OS-level event handler in `global-hotkey` (`GlobalHotKeyEvent::
//! set_event_handler`) is a process-global `OnceCell`: it can be installed exactly
//! once per process, and a second installation is silently ignored. To avoid the
//! silent event loss that a naive per-instance design would suffer, this crate
//! backs *every* [`GlobalHotkey`] handle with a single process-wide
//! [`GlobalHotkeyService`] (created on the first `new` call and shared by all
//! handles). The service is accordingly resident for the lifetime of the process.
//!
//! ## GPUI flavors
//!
//! Select exactly one flavor: `gpui-kit` (default, GPUI Kit's `gpui-pre` types),
//! `gpui` (upstream Zed GPUI), or `gpui-ce` (GPUI Community Edition). Disable
//! default features when selecting an alternative. The selected flavor must match
//! the GPUI the host application builds against, because every operation takes
//! that flavor's `gpui::App`.
//!
//! ## Availability & platform notes
//!
//! `is_available()` reports whether a working backend exists on the current
//! platform / session. On Linux it probes X11 connectivity up front, so a pure
//! Wayland session (no XWayland) reports `false` and every registration call
//! fails with an error instead of hanging. When `is_available()` is `false`,
//! [`GlobalHotkey::new`] still succeeds (the app starts normally) but
//! `on_hotkey` / `register` return an error.
//!
//! ## Example
//!
//! ```ignore
//! use gpui_global_hotkey::{GlobalHotkey, HotKey};
//! use gpui_kit::*;
//!
//! actions!(demo, [ShowWindow, Quit]);
//!
//! fn main() {
//!     gpui_kit::application().run(|cx| {
//!         gpui_kit::init(cx);
//!         let hk = GlobalHotkey::new(cx).expect("global hotkey");
//!         println!("available: {}", hk.is_available());
//!
//!         // Dispatch a GPUI Action (reaches `App::on_action` when no window is focused).
//!         let _r1 = hk.on_hotkey(HotKey::from_str("ctrl-alt-g").unwrap(), ShowWindow);
//!
//!         // Callback form, aware of Pressed / Released.
//!         let _r2 = hk.on_hotkey_with(HotKey::from_str("ctrl-alt-m").unwrap(), |cx, ev| {
//!             if ev.state == gpui_global_hotkey::HotkeyState::Pressed {
//!                 cx.quit();
//!             }
//!         });
//!     });
//! }
//! ```

#[cfg(all(feature = "gpui", feature = "gpui-kit"))]
compile_error!(
    "features `gpui` and `gpui-kit` are mutually exclusive; use default-features = false and select exactly one backend"
);
#[cfg(all(feature = "gpui", feature = "gpui-ce"))]
compile_error!(
    "features `gpui` and `gpui-ce` are mutually exclusive; use default-features = false and select exactly one backend"
);
#[cfg(all(feature = "gpui-kit", feature = "gpui-ce"))]
compile_error!(
    "features `gpui-kit` and `gpui-ce` are mutually exclusive; use default-features = false and select exactly one backend"
);
#[cfg(not(any(feature = "gpui", feature = "gpui-kit", feature = "gpui-ce")))]
compile_error!("enable exactly one of the `gpui`, `gpui-kit`, or `gpui-ce` features");

// Every flavor publishes the same `gpui` API under a renamed dependency, so the
// selected one is aliased to `gpui` here.
#[cfg(all(feature = "gpui-ce", not(any(feature = "gpui", feature = "gpui-kit"))))]
extern crate gpui_ce_backend as gpui;
#[cfg(all(feature = "gpui-kit", not(any(feature = "gpui", feature = "gpui-ce"))))]
extern crate gpui_kit_backend as gpui;
// The upstream flavor's dependency *is* named `gpui`, so no alias is needed.
#[cfg(all(feature = "gpui", not(any(feature = "gpui-kit", feature = "gpui-ce"))))]
extern crate gpui;

mod backend;
mod event;
mod hotkey;
mod platform;

pub use backend::HotkeyBackend;
pub use event::{HotkeyEvent, HotkeyState};
pub use hotkey::HotKey;
pub use platform::{GlobalHotkeyBackend, PlatformHotkeyBackend};

// Re-export the underlying `global-hotkey` types for advanced callers, mirroring
// tauri's `global-shortcut` plugin.
pub use global_hotkey::hotkey::{Code, Modifiers};

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use flume::Sender;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use gpui::{Action, App, AsyncApp};
use parking_lot::Mutex;

/// A boxed GPUI action, dispatchable from the bridge task.
pub type AnyAction = Arc<dyn Action + Send + Sync + 'static>;

/// A per-registration entry kept in the main-thread registry.
struct Registration {
    /// The `global-hotkey` representation, used for id and unregistration.
    ghk: global_hotkey::hotkey::HotKey,
    /// The GPUI-native hotkey, surfaced on the event.
    public: HotKey,
    /// An action dispatched on `Pressed` (reaches `App::on_action` when unfocused).
    action: Option<AnyAction>,
    /// A callback invoked on `Pressed` (and `Released` when present).
    handler: Option<Box<dyn FnMut(&mut App, HotkeyEvent) + Send + 'static>>,
}

/// Messages delivered to the bridge task on the GPUI main thread.
///
/// A single channel carries both OS events and control requests, so the bridge
/// loop needs no `flume::select!` (which would require flume's opt-in `select`
/// feature) — it just `recv()`s until the channel closes.
enum BridgeMsg {
    /// An OS hotkey event to dispatch.
    Event(GlobalHotKeyEvent),
    /// Unregister the hotkey with this id (the OS call runs on the main thread).
    Unregister(u32),
}

/// The process-wide service backing every [`GlobalHotkey`] handle.
///
/// Because `global-hotkey`'s OS event handler is a process-global `OnceCell`
/// (see [`GlobalHotKeyEvent::set_event_handler`]), this service is created at
/// most once per process via [`GlobalHotkeyService::get_or_init`]. All handles
/// share its manager, registry, and bridge task. The service is intentionally
/// resident for the process lifetime: the upstream handler cannot be reset, so a
/// torn-down service cannot be re-created within the same process (tauri's
/// `global-shortcut` plugin is likewise process-resident).
struct GlobalHotkeyService {
    /// The OS backend; `is_available()` reflects whether registration will work.
    backend: GlobalHotkeyBackend,
    /// Shared registry of all currently-registered hotkeys (across *every*
    /// [`GlobalHotkey`] handle in the process).
    registry: Arc<Mutex<HashMap<u32, Registration>>>,
    /// The sender side of the bridge channel; cloned into every registration so
    /// its `Drop` can request unregistration.
    bridge_tx: Sender<BridgeMsg>,
}

impl GlobalHotkeyService {
    /// Return the process-wide service, creating it on first call.
    ///
    /// `cx` is required only for the very first call (to spawn the bridge task
    /// on the GPUI main thread); subsequent calls ignore it and return the
    /// already-created shared service.
    fn get_or_init(cx: &mut App) -> Arc<Self> {
        static SERVICE: OnceLock<Arc<GlobalHotkeyService>> = OnceLock::new();
        SERVICE
            .get_or_init(|| Arc::new(GlobalHotkeyService::new(cx)))
            .clone()
    }

    fn new(cx: &mut App) -> Self {
        let supported = cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
        ));

        // Determine (manager, available). On Linux we probe X11 connectivity
        // *before* constructing the manager so a pure-Wayland session (no
        // XWayland) reports `available == false` instead of blocking later in
        // `register` (the upstream x11 thread would never reply).
        let (manager, available) = if supported {
            create_backend()
        } else {
            (None, false)
        };

        let (bridge_tx, bridge_rx) = flume::unbounded::<BridgeMsg>();
        let registry = Arc::new(Mutex::new(HashMap::new()));

        // Install the OS event handler exactly once. `OnceLock` guarantees this
        // closure is created only on the first `new` call; the upstream
        // `OnceCell` would silently ignore any later attempt anyway.
        if available {
            let event_tx = bridge_tx.clone();
            GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                let _ = event_tx.send(BridgeMsg::Event(e));
            }));
        }

        // Bridge task: runs on the GPUI main thread (the executor polls it
        // there). The returned `Task` handle is intentionally dropped — dropping
        // it does not cancel the spawned future, so the loop runs for the
        // process lifetime. It owns the manager so OS grabs are released only at
        // process exit.
        let app = cx.to_async();
        let registry_bridge = registry.clone();
        let manager_bridge = manager.clone();
        let _bridge_task = cx.spawn(move |_cx: &mut AsyncApp| {
            let app = app.clone();
            async move {
                while let Ok(msg) = bridge_rx.recv() {
                    match msg {
                        BridgeMsg::Event(e) => dispatch_event(&registry_bridge, &app, e),
                        BridgeMsg::Unregister(id) => {
                            if let Some(m) = &manager_bridge {
                                let ghk = registry_bridge.lock().remove(&id).map(|r| r.ghk);
                                if let Some(ghk) = ghk {
                                    let _ = m.lock().unregister(ghk);
                                }
                            } else {
                                registry_bridge.lock().remove(&id);
                            }
                        }
                    }
                }
            }
        });

        let backend = GlobalHotkeyBackend::new(manager, available);
        Self {
            backend,
            registry,
            bridge_tx,
        }
    }
}

/// Build the OS backend (manager + availability flag) for a supported platform.
///
/// On Linux this probes X11 first; on other platforms `GlobalHotKeyManager::new`
/// itself reports failure (so a `None` manager with `available == false` is
/// returned). Unsupported platforms never reach here.
fn create_backend() -> (Option<Arc<Mutex<backend::Manager>>>, bool) {
    #[cfg(target_os = "linux")]
    {
        if crate::backend::x11_available() {
            make_manager()
        } else {
            (None, false)
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        make_manager()
    }
}

/// Try to construct the `global-hotkey` manager; on failure report unavailable.
fn make_manager() -> (Option<Arc<Mutex<backend::Manager>>>, bool) {
    match GlobalHotKeyManager::new() {
        Ok(m) => (
            Some(Arc::new(Mutex::new(backend::Manager { inner: Some(m) }))),
            true,
        ),
        Err(e) => {
            log::warn!("global-hotkey manager unavailable: {e}");
            (None, false)
        }
    }
}

/// A handle to the system global-hotkey service.
///
/// Cheaply cloneable (`Arc`): every handle in the process shares the same
/// underlying [`GlobalHotkeyService`] (one manager, one registry, one bridge
/// task). Create it inside `application().run(|cx| …)` and call its methods
/// there. Registering a shortcut returns a [`HotkeyRegistration`] whose lifetime
/// is the registration — drop it to unregister.
pub struct GlobalHotkey {
    service: Arc<GlobalHotkeyService>,
}

impl Clone for GlobalHotkey {
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
        }
    }
}

impl GlobalHotkey {
    /// Obtain a handle to the process-wide global-hotkey service.
    ///
    /// The first call in the process creates the service (installing the OS event
    /// handler and spawning the bridge task on the GPUI main thread); later calls
    /// return a shared handle. Never blocks startup: if the OS cannot provide
    /// global hotkeys (e.g. a Wayland session without XWayland, or an
    /// unsupported platform), the handle is still returned with
    /// [`GlobalHotkey::is_available`] reporting `false`, and every registration
    /// call returns an error instead of hanging.
    pub fn new(cx: &mut App) -> Result<Self> {
        Ok(Self {
            service: GlobalHotkeyService::get_or_init(cx),
        })
    }

    /// Whether a working global-hotkey backend exists on this platform / session.
    ///
    /// On Linux this reflects X11 connectivity (a pure Wayland session returns
    /// `false`); on Windows / macOS it reflects whether the manager could be
    /// created. When `false`, [`on_hotkey`](Self::on_hotkey) / `register`
    /// return an error.
    pub fn is_available(&self) -> bool {
        self.service.backend.is_available()
    }

    /// Register a global hotkey that dispatches a GPUI [`Action`](gpui::Action).
    ///
    /// When the app is unfocused, the action reaches the application-level
    /// [`App::on_action`](gpui::App::on_action) listener (commonly used to show
    /// or focus the main window). Returns a [`HotkeyRegistration`] whose lifetime
    /// is the registration; drop it to unregister.
    pub fn on_hotkey<A>(&self, hotkey: HotKey, action: A) -> Result<HotkeyRegistration>
    where
        A: Action + Send + Sync + 'static,
    {
        self.register_internal(hotkey, Some(Arc::new(action)), None)
    }

    /// Register a global hotkey that invokes `f` on the GPUI main thread.
    ///
    /// `f` receives the [`HotkeyEvent`]; use `ev.state` to react to `Pressed`
    /// and/or `Released`. Returns a [`HotkeyRegistration`]; drop it to unregister.
    pub fn on_hotkey_with<F>(&self, hotkey: HotKey, f: F) -> Result<HotkeyRegistration>
    where
        F: FnMut(&mut App, HotkeyEvent) + Send + 'static,
    {
        self.register_internal(hotkey, None, Some(Box::new(f)))
    }

    /// Register a global hotkey with no action or callback (id reserved, e.g. for
    /// later inspection).
    pub fn register(&self, hotkey: HotKey) -> Result<HotkeyRegistration> {
        self.register_internal(hotkey, None, None)
    }

    fn register_internal(
        &self,
        public: HotKey,
        action: Option<AnyAction>,
        handler: Option<Box<dyn FnMut(&mut App, HotkeyEvent) + Send + 'static>>,
    ) -> Result<HotkeyRegistration> {
        // Fast-fail before doing any work: if the backend cannot register (e.g.
        // a Wayland session with no X11), report it as an error rather than
        // letting the underlying `register` block on a dead thread.
        if !self.service.backend.is_available() {
            anyhow::bail!(
                "global-hotkey backend unavailable on this platform/session \
                 (no X11/Wayland backend, or the OS rejected the manager)"
            );
        }

        let ghk = public.to_global_hotkey()?;
        self.service.backend.register_global(ghk)?;
        let id = ghk.id();
        self.service.registry.lock().insert(
            id,
            Registration {
                ghk,
                public,
                action,
                handler,
            },
        );
        Ok(HotkeyRegistration {
            id,
            ctrl_tx: self.service.bridge_tx.clone(),
        })
    }

    /// Unregister every hotkey registered through this handle.
    pub fn clear(&self) {
        let ghks: Vec<global_hotkey::hotkey::HotKey> = self
            .service
            .registry
            .lock()
            .values()
            .map(|r| r.ghk)
            .collect();
        for ghk in ghks {
            let _ = self.service.backend.unregister_global(ghk);
        }
        self.service.registry.lock().clear();
        let _ = self.service.backend.clear();
    }
}

impl Drop for GlobalHotkey {
    fn drop(&mut self) {
        // The service is process-resident: dropping a handle does not tear it
        // down (other handles may still depend on it, and the upstream event
        // handler cannot be reset). Registrations are owned by
        // [`HotkeyRegistration`], whose `Drop` still unregisters them
        // individually.
    }
}

/// A handle to one registered hotkey. Drop it to unregister; the request is
/// forwarded to the bridge task (which runs on the main thread) so the OS call
/// happens on the correct thread.
pub struct HotkeyRegistration {
    id: u32,
    ctrl_tx: Sender<BridgeMsg>,
}

impl HotkeyRegistration {
    /// Explicitly unregister this hotkey. Equivalent to dropping the handle.
    pub fn unregister(self) {
        // `self` is moved in and dropped here, so `Drop` sends `Unregister`.
    }
}

impl Drop for HotkeyRegistration {
    fn drop(&mut self) {
        let _ = self.ctrl_tx.send(BridgeMsg::Unregister(self.id));
    }
}

/// Deliver an OS hotkey event on the GPUI main thread.
///
/// The registry lock is held only to copy the entry's action / handler out; user
/// code (action dispatch and callbacks) runs *outside* the lock, so a callback
/// that re-enters the registry cannot deadlock (the lesson tauri's plugin bakes
/// in). Callbacks are swapped back into the registry afterwards so they persist
/// across multiple presses.
fn dispatch_event(
    registry: &Arc<Mutex<HashMap<u32, Registration>>>,
    app: &AsyncApp,
    e: GlobalHotKeyEvent,
) {
    let (action, public, handler) = {
        let mut g = registry.lock();
        match g.get_mut(&e.id) {
            Some(r) => (r.action.clone(), r.public.clone(), r.handler.take()),
            None => return,
        }
    };

    let ev = HotkeyEvent {
        hotkey: public,
        state: e.state,
    };

    match e.state {
        HotkeyState::Pressed => {
            if let Some(action) = action {
                let _ = app.update(|cx| cx.dispatch_action(&*action));
            }
            if let Some(mut h) = handler {
                let _ = app.update(|cx| h(cx, ev));
                registry.lock().get_mut(&e.id).map(|r| r.handler = Some(h));
            }
        }
        HotkeyState::Released => {
            if let Some(mut h) = handler {
                let _ = app.update(|cx| h(cx, ev));
                registry.lock().get_mut(&e.id).map(|r| r.handler = Some(h));
            }
        }
    }
}
