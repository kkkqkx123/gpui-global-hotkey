//! Platform backend selection.
//!
//! Route A (recommended) wraps the cross-platform `global-hotkey` crate, which
//! already implements Windows / macOS / Linux(X11) and a no-op fallback. A
//! single [`GlobalHotkeyBackend`] therefore covers every target; the null /
//! "unavailable" behaviour is expressed through [`HotkeyBackend::is_available`]
//! rather than a separate type.

pub use crate::backend::GlobalHotkeyBackend;

/// The backend type in use for the current platform.
pub type PlatformHotkeyBackend = GlobalHotkeyBackend;
