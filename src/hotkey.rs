//! The public [`HotKey`] type: a GPUI-native description of a global shortcut.
//!
//! [`HotKey`] is independent of GPUI's in-window [`Keystroke`](gpui::Keystroke):
//! it describes a *system* shortcut that is grabbed by the OS before the window
//! ever sees it. It converts to the `global-hotkey` crate's `HotKey` on
//! registration (see [`HotKey::to_global_hotkey`]).

use std::str::FromStr;

use gpui::{Keystroke, Modifiers, SharedString};

/// A global keyboard shortcut, independent of GPUI's in-window keystrokes.
///
/// `modifiers` uses GPUI's [`Modifiers`] (where `platform` is ⌘ on macOS and the
/// Super/Win key on Windows/Linux). `key` is the same key name GPUI uses for a
/// [`Keystroke`](gpui::Keystroke) — `"a"`, `"f1"`, `"space"`, `"left"`, ….
///
/// Build one with [`HotKey::from_str`] (e.g. `"ctrl+shift+space"`),
/// [`HotKey::from`] a [`Keystroke`](gpui::Keystroke), or the [`hotkey!`] macro.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HotKey {
    /// The modifier keys. `platform` is ⌘ on macOS, Super/Win elsewhere.
    pub modifiers: Modifiers,
    /// The (non-modifier) key, in GPUI keystroke naming.
    pub key: SharedString,
}

impl HotKey {
    /// An empty modifier set, for use by the [`hotkey!`] macro.
    #[doc(hidden)]
    pub fn empty_modifiers() -> Modifiers {
        Modifiers::default()
    }

    /// Parse a shortcut accelerator such as `"ctrl+shift+space"` or
    /// `"cmdorctrl+g"`. Modifiers must precede the single main key.
    ///
    /// Supported modifier tokens: `control`/`ctrl`, `alt`/`option`,
    /// `shift`, and `platform`/`meta`/`cmd`/`command`/`super`/`win`/
    /// `cmdorctrl`/`commandorcontrol`.
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        let mut modifiers = Modifiers::default();
        let mut key: Option<String> = None;

        for raw in s.split('+') {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }
            if key.is_some() {
                anyhow::bail!(
                    "invalid hotkey {s:?}: modifiers must come before the single main key"
                );
            }
            match token.to_ascii_lowercase().as_str() {
                "control" | "ctrl" => modifiers.control = true,
                "alt" | "option" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                "platform" | "meta" | "cmd" | "command" | "super" | "win" | "windows"
                | "cmdorctrl" | "commandorcontrol" => modifiers.platform = true,
                other => key = Some(other.to_string()),
            }
        }

        let key = key.ok_or_else(|| anyhow::anyhow!("invalid hotkey {s:?}: missing main key"))?;
        Ok(HotKey {
            modifiers,
            key: SharedString::from(key),
        })
    }

    /// Convert this GPUI hotkey into the `global-hotkey` crate's `HotKey`.
    ///
    /// We reuse `global-hotkey`'s own string parser (which understands the full
    /// key table) by rebuilding an accelerator string and parsing it: the
    /// modifier order is fixed and the key is upper-cased to match the parser's
    /// case-insensitive `KeyA` / `ArrowLeft` / `Space` / `F1` aliases.
    pub fn to_global_hotkey(&self) -> anyhow::Result<global_hotkey::hotkey::HotKey> {
        let mut s = String::new();
        if self.modifiers.shift {
            s.push_str("shift+");
        }
        if self.modifiers.control {
            s.push_str("control+");
        }
        if self.modifiers.alt {
            s.push_str("alt+");
        }
        if self.modifiers.platform {
            s.push_str("meta+");
        }
        s.push_str(&self.key.to_ascii_uppercase());

        global_hotkey::hotkey::HotKey::from_str(&s)
            .map_err(|e| anyhow::anyhow!("invalid hotkey {s:?}: {e}"))
    }
}

impl FromStr for HotKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        HotKey::from_str(s)
    }
}

impl From<Keystroke> for HotKey {
    /// Build a [`HotKey`] from a GPUI [`Keystroke`](gpui::Keystroke), ignoring
    /// its `key_char` (only the `key` name and `modifiers` matter for a system
    /// grab).
    fn from(ks: Keystroke) -> Self {
        HotKey {
            modifiers: ks.modifiers,
            key: ks.key.into(),
        }
    }
}

impl std::fmt::Display for HotKey {
    /// Render an accelerator string suitable for a menu, e.g. `"Ctrl+Shift+Space"`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut wrote = false;
        if self.modifiers.control {
            write!(f, "Ctrl")?;
            wrote = true;
        }
        if self.modifiers.alt {
            if wrote {
                write!(f, "+")?;
            }
            write!(f, "Alt")?;
            wrote = true;
        }
        if self.modifiers.shift {
            if wrote {
                write!(f, "+")?;
            }
            write!(f, "Shift")?;
            wrote = true;
        }
        if self.modifiers.platform {
            if wrote {
                write!(f, "+")?;
            }
            write!(f, "Super")?;
            wrote = true;
        }
        if wrote {
            write!(f, "+")?;
        }
        write!(f, "{}", self.key)
    }
}

/// Construct a [`HotKey`] from modifier flags and a key literal.
///
/// ```ignore
/// use gpui_global_hotkey::hotkey;
/// let h = hotkey!(control | alt => "g");
/// ```
///
/// Modifier identifiers are the lower-case [`Modifiers`](gpui::Modifiers) fields
/// (`control`, `alt`, `shift`, `platform`); the key is a string literal.
#[macro_export]
macro_rules! hotkey {
    ($($modifier:ident)|+ => $key:literal) => {{
        let mut __modifiers = $crate::hotkey::HotKey::empty_modifiers();
        $(__modifiers.$modifier = true;)+
        $crate::hotkey::HotKey {
            modifiers: __modifiers,
            key: $key.into(),
        }
    }};
}
