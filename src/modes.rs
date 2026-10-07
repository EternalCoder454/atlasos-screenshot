//! What to capture (`Kind`) and what to make of it (`Mode`).

use serde::Deserialize;

/// What part of the desktop to capture. The names are the command-line
/// flags and the values of `capture.default_mode` in the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Drag a rectangle on the frozen desktop (the snip).
    Region,
    /// Every screen, as one image.
    Full,
    /// The screen the pointer is on (the active screen).
    Screen,
    /// The window that has the focus.
    ActiveWindow,
    /// Click a window, with KWin's own picker.
    Window,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Region => "region",
            Kind::Full => "full",
            Kind::Screen => "screen",
            Kind::ActiveWindow => "active-window",
            Kind::Window => "window",
        }
    }

    /// Whether the user picks something on screen (so the watchdog that
    /// bounds the compositor's answer must not run, and a long wait is fine).
    pub fn interactive(self) -> bool {
        matches!(self, Kind::Region | Kind::Window)
    }
}

/// What the capture becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Image,
    Text,
    Redact,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "image" => Some(Mode::Image),
            "text" => Some(Mode::Text),
            "redact" => Some(Mode::Redact),
            _ => None,
        }
    }
}
