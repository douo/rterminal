//! SIXEL graphics support, split from `terminal.rs`:
//!
//! - [`parser`] — a transparent DCS stream splitter that pulls SIXEL / tmux
//!   passthrough payloads out of the PTY byte stream and passes everything else
//!   through untouched.
//! - [`images`] — the decoded image layer ([`TerminalImages`]) that owns image
//!   placement, grid layout reservation, and scroll/erase bookkeeping.
//! - [`pixel`] — PTY pixel-size reporting derived from the rendered grid.

pub(crate) mod images;
pub(crate) mod parser;
pub(crate) mod pixel;

pub(crate) use images::TerminalImages;
pub(crate) use parser::{SixelStreamAction, SixelStreamParser};
pub(crate) use pixel::{PtyPixelSize, pty_pixel_size_for_grid};
