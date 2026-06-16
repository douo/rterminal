//! The decoded SIXEL image layer.
//!
//! Images live alongside — not inside — the terminal grid: the grid only
//! reserves blank layout space for them, while the pixels are kept here as
//! [`RenderImage`]s and painted by the renderer. [`TerminalImages`] owns that
//! collection together with the bookkeeping that keeps image rows aligned with
//! the text that scrolls and erases around them.

use std::sync::Arc;

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::sixel::SixelImage;
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use gpui::{Pixels, RenderImage, px};

#[derive(Clone)]
pub(crate) struct TerminalImage {
    pub(crate) row: isize,
    pub(crate) col: usize,
    pub(crate) cols: usize,
    pub(crate) rows: usize,
    pub(crate) image: Arc<RenderImage>,
}

/// Owns the live set of placed images and the invariants that keep them in sync
/// with the terminal grid.
#[derive(Clone, Default)]
pub(crate) struct TerminalImages {
    images: Vec<TerminalImage>,
}

impl TerminalImages {
    /// Cap on retained images, bounding memory growth. There is no full graphics
    /// storage model yet; the oldest images are dropped past this limit.
    const MAX_IMAGES: usize = 128;

    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn iter(&self) -> std::slice::Iter<'_, TerminalImage> {
        self.images.iter()
    }

    /// Decode and place a SIXEL image at its anchor, reserving grid layout below it.
    ///
    /// Returns `false` (storing nothing) if the payload does not form a valid image.
    ///
    /// [`reserve_sixel_layout`] reserves vertical space with real line feeds; when
    /// the anchor sits near the bottom margin those scroll the grid and emit
    /// `Scroll` events. The terminal drains those events into [`Self::scroll_region`],
    /// which moves this freshly-stored image up in lockstep so it lands inside the
    /// reserved region rather than below the viewport.
    pub(crate) fn store<T: EventListener>(
        &mut self,
        image: &SixelImage,
        cell_width: Pixels,
        line_height: Pixels,
        processor: &mut Processor<StdSyncHandler>,
        term: &mut Term<T>,
    ) -> bool {
        let occupied_cols = sixel_occupied_cols(image.width, cell_width);
        let occupied_rows = sixel_occupied_rows(image.height, line_height);
        let Some(render_image) = render_image_from_sixel(image) else {
            return false;
        };

        self.images.push(TerminalImage {
            row: image.row as isize,
            col: image.col,
            cols: occupied_cols,
            rows: occupied_rows,
            image: render_image,
        });
        if self.images.len() > Self::MAX_IMAGES {
            let remove_count = self.images.len() - Self::MAX_IMAGES;
            self.images.drain(0..remove_count);
        }

        reserve_sixel_layout(processor, term, occupied_cols, occupied_rows, image.row);
        true
    }

    /// Move images intersecting `[region_top, region_bottom)` by `delta` rows,
    /// dropping any pushed fully out of the region.
    pub(crate) fn scroll_region(&mut self, region_top: usize, region_bottom: usize, delta: i32) {
        scroll_images_in_region(&mut self.images, region_top, region_bottom, delta);
    }

    /// Drop images intersecting `[region_top, region_bottom)`.
    pub(crate) fn erase_region(&mut self, region_top: usize, region_bottom: usize) {
        erase_images_in_region(&mut self.images, region_top, region_bottom);
    }
}

fn render_image_from_sixel(sixel: &SixelImage) -> Option<Arc<RenderImage>> {
    if sixel.width == 0 || sixel.height == 0 || sixel.rgba.len() != sixel.width * sixel.height * 4 {
        return None;
    }

    let mut bgra = sixel.rgba.clone();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }

    let pixels = image::RgbaImage::from_raw(sixel.width as u32, sixel.height as u32, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
}

/// Reserve `occupied_rows` of vertical layout for an image anchored at `anchor_row`,
/// returning how many rows the grid actually scrolled to make room.
///
/// Space is reserved with real line feeds rather than `CUD` (`ESC[nB`): cursor-down
/// clamps at the bottom margin and never scrolls, so an image anchored near the
/// bottom would not push content up to make room and would be painted (mostly)
/// below the viewport. A line feed scrolls at the bottom margin and emits a `Scroll`
/// event, which moves the stored image up in lockstep so it lands inside the
/// reserved region.
fn reserve_sixel_layout<T: EventListener>(
    processor: &mut Processor<StdSyncHandler>,
    term: &mut Term<T>,
    occupied_cols: usize,
    occupied_rows: usize,
    anchor_row: usize,
) -> usize {
    let mut reservation = Vec::with_capacity(32);
    if occupied_cols > 0 {
        reservation.extend_from_slice(format!("\x1b[{}C", occupied_cols).as_bytes());
    }
    for _ in 0..occupied_rows.max(1) {
        reservation.push(b'\r');
        reservation.push(b'\n');
    }
    processor.advance(term, &reservation);

    let content = term.renderable_content();
    let cursor_row = (content.cursor.point.line.0 + content.display_offset as i32).max(0) as usize;
    anchor_row
        .saturating_add(occupied_rows)
        .saturating_sub(cursor_row)
}

fn sixel_occupied_cols(image_width: usize, cell_width: Pixels) -> usize {
    let cell_width = f32::from(cell_width.max(px(1.0)));
    ((image_width as f32) / cell_width).ceil().max(1.0) as usize
}

fn sixel_occupied_rows(image_height: usize, line_height: Pixels) -> usize {
    let line_height = f32::from(line_height.max(px(1.0)));
    ((image_height as f32) / line_height).ceil().max(1.0) as usize
}

fn scroll_images_in_region(
    images: &mut Vec<TerminalImage>,
    region_top: usize,
    region_bottom: usize,
    delta: i32,
) {
    if delta == 0 || region_top >= region_bottom {
        return;
    }

    let region_top = region_top as isize;
    let region_bottom = region_bottom as isize;
    let delta = delta as isize;
    *images = images
        .drain(..)
        .filter_map(|mut image| {
            let image_bottom = image.row + image.rows as isize;
            let intersects_region = image.row < region_bottom && image_bottom > region_top;
            if !intersects_region {
                return Some(image);
            }

            image.row += delta;
            let image_bottom = image.row + image.rows as isize;
            if image.row >= region_bottom || image_bottom <= region_top {
                return None;
            }

            Some(image)
        })
        .collect();
}

fn erase_images_in_region(images: &mut Vec<TerminalImage>, region_top: usize, region_bottom: usize) {
    if region_top >= region_bottom {
        return;
    }

    let region_top = region_top as isize;
    let region_bottom = region_bottom as isize;
    images.retain(|image| {
        let image_bottom = image.row + image.rows as isize;
        !(image.row < region_bottom && image_bottom > region_top)
    });
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::Term;
    use alacritty_terminal::event::EventListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::sixel::decode_sixel_payload;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use gpui::px;

    use super::{
        TerminalImage, erase_images_in_region, reserve_sixel_layout, scroll_images_in_region,
        sixel_occupied_cols, sixel_occupied_rows,
    };
    use crate::sixel::parser::{SixelStreamAction, SixelStreamParser};
    use crate::terminal::GridSize;

    /// Test event sink: the layout tests only inspect the resulting grid, not the
    /// emitted events, so the default no-op `send_event` is enough.
    struct NoopListener;
    impl EventListener for NoopListener {}

    fn dummy_render_image() -> std::sync::Arc<gpui::RenderImage> {
        let pixels = image::RgbaImage::from_raw(1, 1, vec![0, 0, 0, 255]).unwrap();
        std::sync::Arc::new(gpui::RenderImage::new(vec![image::Frame::new(pixels)]))
    }

    #[test]
    fn sixel_layout_reservation_places_following_text_below_image() {
        let mut term = Term::new(Config::default(), &GridSize { cols: 80, rows: 6 }, NoopListener);
        let mut processor = Processor::<StdSyncHandler>::new();
        let mut parser = SixelStreamParser::default();
        let actions = parser.advance(b"before\r\n\x1bPq\"1;1;1;36#1~\x1b\\after");

        for action in actions {
            match action {
                SixelStreamAction::Bytes(bytes) => processor.advance(&mut term, &bytes),
                SixelStreamAction::Sixel(payload) => {
                    let content = term.renderable_content();
                    let row = (content.cursor.point.line.0 + content.display_offset as i32).max(0)
                        as usize;
                    let col = content.cursor.point.column.0;
                    let image = decode_sixel_payload(row, col, &payload).unwrap();
                    reserve_sixel_layout(
                        &mut processor,
                        &mut term,
                        sixel_occupied_cols(image.width, px(8.0)),
                        sixel_occupied_rows(image.height, px(18.0)),
                        row,
                    );
                }
                _ => panic!("unexpected stream action"),
            }
        }

        let rendered: String = (0..5)
            .map(|col| term.grid()[Line(3)][Column(col)].c)
            .collect();
        assert_eq!(rendered, "after");
    }

    #[test]
    fn sixel_layout_reservation_reports_scroll_when_image_starts_at_bottom() {
        let mut term = Term::new(Config::default(), &GridSize { cols: 80, rows: 4 }, NoopListener);
        let mut processor = Processor::<StdSyncHandler>::new();

        processor.advance(&mut term, b"\x1b[4;1H");
        let scrolled_rows = reserve_sixel_layout(&mut processor, &mut term, 1, 2, 3);
        processor.advance(&mut term, b"after");

        let rendered: String = (0..5)
            .map(|col| term.grid()[Line(3)][Column(col)].c)
            .collect();
        assert_eq!(scrolled_rows, 2);
        assert_eq!(rendered, "after");
    }

    #[test]
    fn sixel_images_follow_normal_text_scrollback() {
        let mut term = Term::new(Config::default(), &GridSize { cols: 80, rows: 4 }, NoopListener);
        let mut processor = Processor::<StdSyncHandler>::new();
        let mut images = vec![TerminalImage {
            row: 2,
            col: 0,
            cols: 1,
            rows: 3,
            image: dummy_render_image(),
        }];

        processor.advance(&mut term, b"one\r\ntwo\r\nthree\r\nfour");
        let old_history_size = term.grid().history_size();
        processor.advance(&mut term, b"\r\nfive");
        let new_history_size = term.grid().history_size();
        if new_history_size > old_history_size {
            scroll_images_in_region(
                &mut images,
                0,
                usize::MAX / 2,
                -((new_history_size - old_history_size) as i32),
            );
        }

        assert_eq!(images[0].row, 1);

        processor.advance(&mut term, b"\r\nsix\r\nseven\r\neight");
        let next_history_size = term.grid().history_size();
        if next_history_size > new_history_size {
            scroll_images_in_region(
                &mut images,
                0,
                usize::MAX / 2,
                -((next_history_size - new_history_size) as i32),
            );
        }

        assert_eq!(images[0].row, -2);

        processor.advance(&mut term, b"\r\nnine");
        let final_history_size = term.grid().history_size();
        if final_history_size > next_history_size {
            scroll_images_in_region(
                &mut images,
                0,
                usize::MAX / 2,
                -((final_history_size - next_history_size) as i32),
            );
        }

        assert!(images.is_empty());
    }

    #[test]
    fn terminal_region_scroll_moves_intersecting_sixel_images() {
        let mut images = vec![
            TerminalImage {
                row: 2,
                col: 0,
                cols: 1,
                rows: 2,
                image: dummy_render_image(),
            },
            TerminalImage {
                row: 5,
                col: 0,
                cols: 1,
                rows: 1,
                image: dummy_render_image(),
            },
        ];

        scroll_images_in_region(&mut images, 1, 4, -1);

        assert_eq!(images.len(), 2);
        assert_eq!(images[0].row, 1);
        assert_eq!(images[1].row, 5);

        scroll_images_in_region(&mut images, 1, 4, -3);

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].row, 5);
    }

    #[test]
    fn terminal_erase_removes_intersecting_sixel_images() {
        let mut images = vec![
            TerminalImage {
                row: 2,
                col: 0,
                cols: 1,
                rows: 2,
                image: dummy_render_image(),
            },
            TerminalImage {
                row: 5,
                col: 0,
                cols: 1,
                rows: 1,
                image: dummy_render_image(),
            },
        ];

        erase_images_in_region(&mut images, 0, 4);

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].row, 5);
    }
}
