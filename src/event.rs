//! The [`HotkeyEvent`] delivered on the GPUI main thread.

/// The pressed / released state of a [`HotkeyEvent`](crate::HotkeyEvent).
///
/// Re-exported from the `global-hotkey` crate; the X11 backend only ever emits
/// [`HotkeyState::Pressed`].
pub use global_hotkey::HotKeyState as HotkeyState;

/// A global hotkey event, delivered on the GPUI main thread after the OS reports
/// the key combination.
///
/// `hotkey` is the [`HotKey`](crate::HotKey) that triggered (carried so callers
/// need not look it up), and `state` records whether it was pressed or released.
#[derive(Debug, Clone)]
pub struct HotkeyEvent {
    /// The hotkey that triggered this event.
    pub hotkey: crate::HotKey,
    /// Whether the hotkey was pressed or released.
    pub state: HotkeyState,
}
