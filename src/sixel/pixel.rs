//! PTY pixel-size reporting.
//!
//! Terminals advertise their cell pixel dimensions to the child process (via
//! `TIOCSWINSZ` / `ws_xpixel`,`ws_ypixel`) so that pixel-aware protocols such as
//! SIXEL can size their output. This derives that pixel area from the rendered
//! grid geometry.

use gpui::Pixels;

use crate::terminal::GridSize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PtyPixelSize {
    pub(crate) width: u16,
    pub(crate) height: u16,
}

pub(crate) fn pty_pixel_size_for_grid(
    grid_size: GridSize,
    cell_width: Pixels,
    line_height: Pixels,
) -> PtyPixelSize {
    PtyPixelSize {
        width: pixels_to_pty_size(cell_width * grid_size.cols as f32),
        height: pixels_to_pty_size(line_height * grid_size.rows as f32),
    }
}

fn pixels_to_pty_size(value: Pixels) -> u16 {
    f32::from(value).round().clamp(1.0, u16::MAX as f32) as u16
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::pty_pixel_size_for_grid;
    use crate::terminal::GridSize;

    #[test]
    fn pty_pixel_size_matches_rendered_grid_cell_area() {
        let pixel_size =
            pty_pixel_size_for_grid(GridSize { cols: 80, rows: 24 }, px(7.5), px(18.0));

        assert_eq!(pixel_size.width, 600);
        assert_eq!(pixel_size.height, 432);
    }
}
