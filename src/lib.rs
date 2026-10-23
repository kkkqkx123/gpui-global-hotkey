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
//! ## GPUI flavors
//!
//! Select exactly one flavor: `gpui-kit` (default, GPUI Kit's `gpui-pre` types),
//! `gpui` (upstream Zed GPUI), or `gpui-ce` (GPUI Community Edition). Disable
//! default features when selecting an alternative. The selected flavor must match
//! the GPUI the host application builds against, because every operation takes
//! that flavor's `gpui::App`.
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
use std::sync::Arc;

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
    /// Tear down the bridge; the manager's `Drop` then releases every grab.
    Shutdown,
}

/// Shared state behind an [`GlobalHotkey`] handle.
struct Inner {
    backend: PlatformHotkeyBackend,
    registry: Arc<Mutex<HashMap<u32, Registration>>>,
    ctrl_tx: Sender<BridgeMsg>,
}

/// A handle to the system global-hotkey service.
///
/// Cheaply cloneable (`Arc`), and bound to the GPUI main thread like every other
/// handle into application state: create it inside `application().run(|cx| …)` and
/// call its methods there. Registering a shortcut returns a
/// [`HotkeyRegistration`] whose lifetime is the registration — drop it to
/// unregister.
pub struct GlobalHotkey {
    inner: Arc<Inner>,
}

impl Clone for GlobalHotkey {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl GlobalHotkey {
    /// Create the global-hotkey service on the current platform.
    ///
    /// Never blocks startup: if the OS cannot provide global hotkeys (e.g. a
    /// Wayland session without XWayland, or an unsupported platform), the handle
    /// is still returned with [`GlobalHotkey::is_available`] reporting `false`
    /// and every registration degrading to a silent no-op.
    pub fn new(cx: &mut App) -> Result<Self> {
        let supported = cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
        ));

        let manager: Option<Arc<Mutex<backend::Manager>>> = if supported {
            match GlobalHotKeyManager::new() {
                Ok(m) => Some(Arc::new(Mutex::new(backend::Manager { inner: Some(m) }))),
                Err(e) => {
                    log::warn!("global-hotkey manager unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };
        let available = manager.is_some();

        let (bridge_tx, bridge_rx) = flume::unbounded::<BridgeMsg>();
        let registry = Arc::new(Mutex::new(HashMap::new()));

        // Forward OS events into our bridge channel. The global-hotkey handler is a
        // process-global, `Send + Sync` hook, so exactly one `GlobalHotkey` should
        // exist per process (subsequent instances cannot replace it).
        if available {
            let event_tx = bridge_tx.clone();
            GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                let _ = event_tx.send(BridgeMsg::Event(e));
            }));
        }

        // Bridge task: runs on the GPUI main thread (the executor polls it there).
        // It owns the manager so the OS grabs are released only when the task ends.
        let app = cx.to_async();
        let registry_bridge = registry.clone();
        let manager_bridge = manager.clone();
        let _task = cx.spawn(move |_cx: &mut AsyncApp| {
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
                        BridgeMsg::Shutdown => break,
                    }
                }
            }
        });

        let backend = GlobalHotkeyBackend::new(manager, available);
        Ok(Self {
            inner: Arc::new(Inner {
                backend,
                registry,
                ctrl_tx: bridge_tx,
            }),
        })
    }

    /// Whether a working global-hotkey backend exists on this platform / session.
    pub fn is_available(&self) -> bool {
        self.inner.backend.is_available()
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
        let ghk = public.to_global_hotkey()?;
        self.inner.backend.register_global(ghk)?;
        let id = ghk.id();
        self.inner.registry.lock().insert(
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
            ctrl_tx: self.inner.ctrl_tx.clone(),
        })
    }

    /// Unregister every hotkey registered through this handle.
    pub fn clear(&self) {
        let ghks: Vec<global_hotkey::hotkey::HotKey> = self
            .inner
            .registry
            .lock()
            .values()
            .map(|r| r.ghk)
            .collect();
        for ghk in ghks {
            let _ = self.inner.backend.unregister_global(ghk);
        }
        self.inner.registry.lock().clear();
        let _ = self.inner.backend.clear();
    }
}

impl Drop for GlobalHotkey {
    fn drop(&mut self) {
        // Tell the bridge to stop; when it ends, the manager `Arc` it holds drops
        // and the OS releases every grab.
        let _ = self.inner.ctrl_tx.send(BridgeMsg::Shutdown);
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
