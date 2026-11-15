//! Native Ghostty GPU terminal adapter, derived from gpui-libghostty 0.2.5.
//! Ghostty's full Metal/OpenGL backends render terminal frames; GPUI composes
//! cached completed-frame readbacks because its public API cannot import BGRA.
//! Native child views never cover GPUI overlays. No libghostty-vt renderer is used.

mod clipboard;
mod frame;
pub mod input_surface;
mod native;
mod terminal;

pub use clipboard::{ClipboardApproval, ClipboardApprovalCallback, ClipboardOperation};
pub use terminal::{Terminal, TerminalColor, TerminalOptions, TerminalStatus, TerminalTheme};
