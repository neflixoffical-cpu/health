//! health-theme — the Health design system (bright greens on black).
//!
//! * [`palette`] colors and [`style`] presets
//! * [`fx`] gradients, shimmer, blending, easing, the global animation switch
//! * [`mascot::Mascot`] — the animated green plus with eyes
//! * [`logo`] — the big HEALTH wordmark
//! * [`widgets`] — panels, modals, backdrop, hints, progress, heartbeat, toasts, meters…
//! * [`list::SelectList`], [`field::TextField`], [`widgets::Tabs`] — interactive widgets
//! * [`snapshot`] — render any frame to HTML for visual QA
//!
//! Keyboard handlers ignore `KeyEventKind::Release` (Windows reports press and release).

pub mod palette;
pub mod fx;
pub mod mascot;
pub mod logo;
pub mod widgets;
pub mod list;
pub mod field;
pub mod hit;
pub mod textarea;
pub mod snapshot;

pub use field::{FieldAction, TextField};
pub use textarea::{AreaAction, TextArea};
pub use list::{ListAction, ListItem, SelectList};
pub use mascot::{Mascot, MascotSize, Mood};
pub use palette::*;
pub use widgets::{Tabs, Toasts};

use crossterm::event::{KeyEvent, KeyEventKind};

/// True for key events that should be handled (press / repeat, not release).
pub fn is_press(k: &KeyEvent) -> bool {
    k.kind != KeyEventKind::Release
}
