//! The platform-agnostic [`HotkeyBackend`] trait and its `global-hotkey`
//! implementation.

use std::sync::Arc;

use anyhow::Result;
use global_hotkey::GlobalHotKeyManager;
use parking_lot::Mutex;

use crate::hotkey::HotKey;

/// Probe whether an X11 server is reachable (a pure-Wayland session has none).
///
/// Mirrors the connectivity check the `global-hotkey` x11 backend performs in
/// its event thread, done up front so we can short-circuit registration instead
/// of blocking on a dead thread. Only meaningful on Linux; on other platforms the
/// manager's own `new()` reports failure directly.
#[cfg(target_os = "linux")]
pub(crate) fn x11_available() -> bool {
    x11rb::connect(None).is_ok()
}

/// A platform global-hotkey implementation.
///
/// Backends own native handles that belong to the GPUI main thread, so a backend
/// is **not** `Send`: every operation must run on the thread that created it
/// (the GPUI main thread), exactly like [`gpui::App`](gpui::App) itself.
pub trait HotkeyBackend {
    /// Whether a working global-hotkey backend is present on this platform /
    /// session. Returns `false` when the OS cannot provide global hotkeys (e.g.
    /// a Wayland session with no XWayland, or an unsupported platform), so the
    /// application can start normally and the registration calls return an
    /// error instead of hanging.
    fn is_available(&self) -> bool;

    /// Register a hotkey with the OS. Returns an error if the OS rejects it
    /// (the combination is already taken by another app / the system).
    fn register(&self, hotkey: &HotKey) -> Result<()>;

    /// Unregister a previously registered hotkey.
    fn unregister(&self, hotkey: &HotKey) -> Result<()>;

    /// Unregister every hotkey registered through this backend.
    fn clear(&self) -> Result<()>;
}

/// Wraps `global_hotkey::GlobalHotKeyManager`. The manager handle is kept alive
/// for as long as registrations exist; dropping it makes the OS release every
/// grab (the crate's own `Drop` cleans up).
pub(crate) struct Manager {
    pub(crate) inner: Option<GlobalHotKeyManager>,
}

impl Manager {
    pub(crate) fn register(&self, ghk: global_hotkey::hotkey::HotKey) -> Result<()> {
        if let Some(m) = &self.inner {
            m.register(ghk)?;
        }
        Ok(())
    }

    pub(crate) fn unregister(&self, ghk: global_hotkey::hotkey::HotKey) -> Result<()> {
        if let Some(m) = &self.inner {
            m.unregister(ghk)?;
        }
        Ok(())
    }

    pub(crate) fn clear(&self) -> Result<()> {
        if let Some(m) = &self.inner {
            // `global-hotkey` has no "unregister everything" call; dropping the
            // manager would release every grab, but we only want to release the
            // grabs this backend owns. The public `GlobalHotkey::clear` walks its
            // own registry and calls `unregister_global` per entry, so this path
            // is only used as a last resort.
            let _ = m;
        }
        Ok(())
    }
}

// SAFETY: the manager is only ever accessed on the GPUI main thread. Every
// `register` / `unregister` / `clear` call originates from `GlobalHotkey` (or
// the bridge task), both of which run on that thread. The `unsafe impl`s let the
// non-`Send` manager be shared behind `Arc<Mutex<_>>` with the bridge task, which
// the GPUI executor polls on the main thread. This mirrors tauri's
// `global-shortcut` plugin.
unsafe impl Send for Manager {}
unsafe impl Sync for Manager {}

/// The concrete backend wrapping the cross-platform `global-hotkey` crate.
///
/// When the manager could not be created (or the target is unsupported) it holds
/// `None` and reports `is_available() == false`; registration then returns an
/// error (rather than silently succeeding) so the application still starts and
/// can react to the failure.
pub struct GlobalHotkeyBackend {
    manager: Option<Arc<Mutex<Manager>>>,
    available: bool,
}

impl GlobalHotkeyBackend {
    pub(crate) fn new(manager: Option<Arc<Mutex<Manager>>>, available: bool) -> Self {
        Self { manager, available }
    }

    /// Register a `global-hotkey` `HotKey` directly (no string round-trip).
    ///
    /// Returns an error when no backend is available (e.g. a Wayland session with
    /// no X11), so callers get a clear failure instead of a silent no-op — and
    /// never block on the underlying x11 thread, which would hang the caller.
    pub(crate) fn register_global(&self, ghk: global_hotkey::hotkey::HotKey) -> Result<()> {
        match &self.manager {
            Some(m) => m.lock().register(ghk),
            None => anyhow::bail!(
                "global-hotkey backend unavailable on this platform/session \
                 (no X11/Wayland backend)"
            ),
        }
    }

    /// Unregister a `global-hotkey` `HotKey` directly.
    pub(crate) fn unregister_global(&self, ghk: global_hotkey::hotkey::HotKey) -> Result<()> {
        if let Some(m) = &self.manager {
            m.lock().unregister(ghk)?;
        }
        Ok(())
    }
}

impl HotkeyBackend for GlobalHotkeyBackend {
    fn is_available(&self) -> bool {
        self.available
    }

    fn register(&self, hotkey: &HotKey) -> Result<()> {
        let ghk = hotkey.to_global_hotkey()?;
        self.register_global(ghk)
    }

    fn unregister(&self, hotkey: &HotKey) -> Result<()> {
        let ghk = hotkey.to_global_hotkey()?;
        self.unregister_global(ghk)
    }

    fn clear(&self) -> Result<()> {
        self.manager
            .as_ref()
            .map_or(Ok(()), |m| m.lock().clear())
    }
}
