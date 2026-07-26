use alacritty_terminal::vte::ansi::CursorShape;
use gpui::{
    Bounds, ContentMask, Context, Corners, ExternalPaths, Font, FontFallbacks, FontStyle,
    FontWeight, Hitbox, HitboxBehavior, Hsla, MouseButton, Pixels, Render, Window,
    WindowControlArea, canvas, div, fill, font, point, prelude::*, px, rgb, rgba, size,
};

use crate::cli::Theme;
use crate::convenience::cursor_indicator::cursor_color_for_focus;
use crate::grid_cells::{cell_advance_cols, selection_contains_cell, visual_extra_cols_before};
use crate::{AgentTerminal, AgentTerminalInputHandler};

pub(crate) const LINE_HEIGHT_SCALE: f32 = 18.0 / 14.0;
pub(crate) const TEXT_PADDING_X: Pixels = px(12.0);
pub(crate) const TEXT_PADDING_Y: Pixels = px(1.0);
const MIN_TEXT_PADDING_Y: Pixels = px(1.0);
pub(crate) const CUSTOM_TITLE_BAR_HEIGHT: Pixels = px(32.0);
pub(crate) const STATUS_BAR_HEIGHT: Pixels = px(42.0);
const CURSOR_TRAIL_MIN_LEN_CELLS: f32 = 0.34;
const CURSOR_TRAIL_MAX_LEN_CELLS: f32 = 1.35;
const CURSOR_TRAIL_PRIMARY_ALPHA_SCALE: f32 = 0.62;
const CURSOR_TRAIL_SECONDARY_ALPHA_SCALE: f32 = 0.28;
const CURSOR_TRAIL_SECONDARY_LEN_SCALE: f32 = 1.6;

pub(crate) fn measure_cell_width(
    window: &mut Window,
    font_family: &str,
    font_fallbacks: Option<&FontFallbacks>,
    font_size: Pixels,
) -> Pixels {
    let mono = build_terminal_font(font_family, font_fallbacks);
    let font_id = window.text_system().resolve_font(&mono);
    if let Ok(advance) = window.text_system().advance(font_id, font_size, 'M') {
        return advance.width;
    }

    // advance 测量失败（字体名完全无效等）时，退到与渲染同一条 shaping 路径量
    // 一个 'M'（DSP-13）：渲染仍会用系统回退字体的真实 advance 画字，网格/PTY
    // winsize 必须用同一把尺子，否则视觉与网格错位。固定 px(8.0) 只是最后防线。
    let run = gpui::TextRun {
        len: 1,
        font: mono,
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = window
        .text_system()
        .shape_line("M".into(), font_size, &[run], None);
    if shaped.width > px(0.0) {
        shaped.width
    } else {
        px(8.0)
    }
}

pub(crate) fn build_terminal_font(
    font_family: &str,
    font_fallbacks: Option<&FontFallbacks>,
) -> Font {
    let mut mono = font(font_family.to_string());
    mono.fallbacks = font_fallbacks.cloned();
    mono
}

fn link_hover_bounds(
    snapshot: &crate::terminal::ScreenSnapshot,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    font_family: &str,
    font_fallbacks: Option<&FontFallbacks>,
    font_size: Pixels,
    line_height: Pixels,
) -> Option<Bounds<Pixels>> {
    let mouse = window.mouse_position();
    if !bounds.contains(&mouse) {
        return None;
    }

    let cell_width =
        measure_cell_width(window, font_family, font_fallbacks, font_size).max(px(1.0));
    let dynamic_padding_y =
        terminal_content_padding_y(bounds.size.height, line_height, snapshot.cells.len());
    let origin = bounds.origin + point(TEXT_PADDING_X, dynamic_padding_y);
    let raw_row = ((mouse.y - origin.y) / line_height).floor() as isize;
    if raw_row < 0 {
        return None;
    }
    let row_index = raw_row as usize;
    let row = snapshot.cells.get(row_index)?;
    let raw_visual_col = ((mouse.x - origin.x) / cell_width).floor() as f32;
    if raw_visual_col < 0.0 {
        return None;
    }

    let mut covered_until_col = 0usize;
    let mut extra_visual_cols = 0f32;
    for (col_index, cell) in row.iter().enumerate() {
        let is_spacer_col = col_index < covered_until_col;
        let x_cols = col_index as f32 + extra_visual_cols;
        let width_cols = cell.width_cols as f32;
        if !is_spacer_col
            && cell.link.is_some()
            && raw_visual_col >= x_cols
            && raw_visual_col < x_cols + width_cols
        {
            let cell_origin = point(
                origin.x + x_cols * cell_width,
                origin.y + row_index as f32 * line_height,
            );
            return Some(Bounds::new(
                cell_origin,
                size(cell_width.max(px(2.0)) * width_cols, line_height),
            ));
        }

        if !is_spacer_col {
            covered_until_col = col_index.saturating_add(cell_advance_cols(cell));
            if cell.expands_layout && cell.width_cols > 1 {
                extra_visual_cols += f32::from(cell.width_cols - 1);
            }
        }
    }

    None
}

pub(crate) fn line_height_for(font_size: Pixels) -> Pixels {
    (font_size * LINE_HEIGHT_SCALE).max(font_size + px(2.0))
}

pub(crate) fn terminal_content_padding_y(
    surface_height: Pixels,
    line_height: Pixels,
    rows: usize,
) -> Pixels {
    let grid_painted_height = line_height * rows as f32;
    let vertical_slack = (surface_height - grid_painted_height).max(MIN_TEXT_PADDING_Y * 2.0);
    (vertical_slack / 2.0).max(MIN_TEXT_PADDING_Y)
}

#[derive(Clone, Copy)]
pub(crate) struct RenderPalette {
    pub(crate) app_bg: Hsla,
    pub(crate) terminal_bg: Hsla,
    pub(crate) title_bg: Hsla,
    pub(crate) title_fg: Hsla,
    pub(crate) selection_bg: Hsla,
    pub(crate) cursor_bg: Hsla,
    /// 默认前景色，IME 组合文本等"不属于任何 cell"的文字用它（DSP-9）。
    pub(crate) foreground: Hsla,
}

struct TerminalCanvasPrepaint {
    link_hover_hitbox: Option<Hitbox>,
}

pub(crate) fn palette_for(theme: Theme) -> RenderPalette {
    match theme {
        Theme::Default => RenderPalette {
            app_bg: rgb(0x0f1115).into(),
            terminal_bg: rgb(0x000000).into(),
            title_bg: rgb(0x171a21).into(),
            title_fg: rgb(0xa9b1c6).into(),
            selection_bg: rgba(0x4b93ffaa).into(),
            cursor_bg: rgba(0xffea00a6).into(),
            foreground: rgb(0xd7dae0).into(),
        },
        Theme::EyeCare => RenderPalette {
            app_bg: rgb(0x151b17).into(),
            terminal_bg: rgb(0x1b241e).into(),
            title_bg: rgb(0x222d26).into(),
            title_fg: rgb(0xc0cbbd).into(),
            selection_bg: rgba(0x7ca67899).into(),
            cursor_bg: rgba(0xffea00a6).into(),
            foreground: rgb(0xccd6c8).into(),
        },
    }
}

impl Render for AgentTerminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 渲染保持只读：AX 同步与输入法状态刷新在 `sync_ax_and_input_state`
        //（周期任务）里，不要加回到这里。
        window.set_window_title(&self.window_title());
        let snapshot = self.snapshot.clone();
        let images = self.images.clone();
        let ime_marked_text = self.ime_marked_text.clone();
        let cursor_shape = self.cursor_shape;
        let (cursor_visual_row, cursor_visual_col, cursor_sliding) = self.cursor_visual_state();
        let cursor_trail_enabled = self.cursor_trail_enabled;
        let cursor_anim_from_col = self.cursor_anim_from_col;
        if cursor_sliding {
            cx.on_next_frame(window, |_, _, cx| {
                cx.notify();
            });
        }
        let focused = self.focus_handle.is_focused(window);
        let window_active = window.is_window_active();
        let focus_handle = self.focus_handle.clone();
        let entity = cx.entity();
        let status = self.debug.status_summary();
        let shell = self.shell.clone();
        let font_family = self.font_family.clone();
        let font_fallbacks = self.font_fallbacks.clone();
        let font_size = self.font_size;
        let line_height = self.line_height();
        let note = self.debug.note();
        let selection = self.selection_bounds();
        let input_mode = self.convenience_state.input_mode();
        let palette = palette_for(self.theme);
        // 灰化判据是"终端失焦"而不只是"窗口失活"（DSP-15）：同窗口内焦点转移
        //（tab 重命名框等）也应让光标变灰。
        let cursor_bg =
            cursor_color_for_focus(palette.cursor_bg, input_mode, window_active && focused);
        let terminal_title = self
            .terminal_title
            .lock()
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| shell.clone());
        let canvas_font_family = font_family.clone();
        let canvas_font_fallbacks = font_fallbacks.clone();
        let canvas_bounds_shared = self.canvas_bounds.clone();
        let canvas_snapshot = snapshot.clone();
        let canvas_font_family_for_prepaint = font_family.clone();
        let canvas_font_fallbacks_for_prepaint = font_fallbacks.clone();

        let status_line = if let Some(note) = note {
            format!("agent terminal | {} | {} | note: {}", shell, status, note)
        } else {
            format!("agent terminal | {} | {}", shell, status)
        };

        let terminal_surface = div()
            .size_full()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down_left))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down_middle))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down_right))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up_left))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up_middle))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up_right))
            .on_drag_move::<ExternalPaths>(cx.listener(Self::on_external_paths_drag_move))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_drop::<ExternalPaths>(cx.listener(Self::on_external_paths_drop))
            .child(
                canvas(
                    move |bounds, window, _| {
                        let link_hover_hitbox = link_hover_bounds(
                            &canvas_snapshot,
                            bounds,
                            window,
                            &canvas_font_family_for_prepaint,
                            canvas_font_fallbacks_for_prepaint.as_ref(),
                            font_size,
                            line_height,
                        )
                        .map(|bounds| window.insert_hitbox(bounds, HitboxBehavior::Normal));
                        TerminalCanvasPrepaint { link_hover_hitbox }
                    },
                    move |bounds, prepaint, window, cx| {
                        *canvas_bounds_shared.lock() = Some(bounds);
                        window.handle_input(
                            &focus_handle,
                            AgentTerminalInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                        window.paint_quad(fill(bounds, palette.terminal_bg));

                        let mono = build_terminal_font(
                            &canvas_font_family,
                            canvas_font_fallbacks.as_ref(),
                        );
                        let run_template = gpui::TextRun {
                            len: 0,
                            font: mono.clone(),
                            // IME 组合文本走这个默认色，随主题（DSP-9）。
                            color: palette.foreground,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };

                        let font_pixels = font_size;
                        let font_id = window.text_system().resolve_font(&mono);
                        let cell_width = window
                            .text_system()
                            .advance(font_id, font_pixels, 'M')
                            .map(|advance| advance.width)
                            .unwrap_or(px(8.0));
                        let link_color: Hsla = rgb(0x6aa8ff).into();
                        if let Some(hitbox) = prepaint.link_hover_hitbox.as_ref()
                            && hitbox.is_hovered(window)
                        {
                            window.set_cursor_style(gpui::CursorStyle::PointingHand, hitbox);
                        }

                        // Dynamically center terminal content vertically:
                        // distribute the fractional row remainder evenly to top and bottom.
                        let dynamic_padding_y = terminal_content_padding_y(
                            bounds.size.height,
                            line_height,
                            snapshot.cells.len(),
                        );
                        let origin = bounds.origin + point(TEXT_PADDING_X, dynamic_padding_y);
                        for (row_index, row) in snapshot.cells.iter().enumerate() {
                            let y = origin.y + row_index as f32 * line_height;
                            let mut covered_until_col = 0usize;
                            let mut extra_visual_cols = 0f32;

                            for (col_index, cell) in row.iter().enumerate() {
                                let is_spacer_col = col_index < covered_until_col;
                                let x =
                                    origin.x + (col_index as f32 + extra_visual_cols) * cell_width;
                                let cell_origin = point(x, y);
                                let cell_width_px =
                                    cell_width.max(px(2.0)) * cell.width_cols as f32;

                                if !is_spacer_col && let Some(bg) = cell.bg {
                                    window.paint_quad(fill(
                                        Bounds::new(cell_origin, size(cell_width_px, line_height)),
                                        bg,
                                    ));
                                }
                                if !is_spacer_col
                                    && selection.is_some_and(|(start, end)| {
                                        selection_contains_cell(start, end, row_index, col_index)
                                    })
                                {
                                    window.paint_quad(fill(
                                        Bounds::new(cell_origin, size(cell_width_px, line_height)),
                                        palette.selection_bg,
                                    ));
                                }

                                if !is_spacer_col && !cell.is_blank() {
                                    let cell_text = cell.text();
                                    let underline = if cell.link.is_some() || cell.underline {
                                        Some(gpui::UnderlineStyle {
                                            color: Some(if cell.link.is_some() {
                                                link_color
                                            } else {
                                                cell.fg
                                            }),
                                            thickness: px(1.0),
                                            wavy: cell.undercurl,
                                        })
                                    } else {
                                        None
                                    };
                                    let mut font = mono.clone();
                                    if cell.bold {
                                        font.weight = FontWeight::BOLD;
                                    }
                                    if cell.italic {
                                        font.style = FontStyle::Italic;
                                    }
                                    let strikethrough =
                                        cell.strikethrough.then_some(gpui::StrikethroughStyle {
                                            color: Some(cell.fg),
                                            thickness: px(1.0),
                                        });
                                    let run = gpui::TextRun {
                                        len: cell_text.len(),
                                        font,
                                        color: if cell.link.is_some() {
                                            link_color
                                        } else {
                                            cell.fg
                                        },
                                        underline,
                                        strikethrough,
                                        ..run_template.clone()
                                    };
                                    let shaped = window.text_system().shape_line(
                                        cell_text.into(),
                                        font_pixels,
                                        &[run],
                                        Some(cell_width_px),
                                    );
                                    let _ = shaped.paint(
                                        cell_origin,
                                        line_height,
                                        gpui::TextAlign::Left,
                                        None,
                                        window,
                                        cx,
                                    );
                                }

                                if !is_spacer_col {
                                    covered_until_col =
                                        col_index.saturating_add(cell_advance_cols(cell));
                                    if cell.expands_layout && cell.width_cols > 1 {
                                        extra_visual_cols += f32::from(cell.width_cols - 1);
                                    }
                                }
                            }
                        }

                        window.with_content_mask(Some(ContentMask { bounds }), |window| {
                            for image in images.iter() {
                                // DSP-2：图片只画在它所属的屏上——主屏图片不浮在
                                // vim/less 之上，alt screen 的预览退出后不残留。
                                if image.alt_screen != snapshot.alt_screen {
                                    continue;
                                }
                                if image.row >= snapshot.cells.len() as isize
                                    || image.row + image.rows as isize <= 0
                                {
                                    continue;
                                }

                                let image_extra_cols = snapshot
                                    .cells
                                    .get(image.row.max(0) as usize)
                                    .map(|row| visual_extra_cols_before(row, image.col))
                                    .unwrap_or(0.0);
                                let image_origin = point(
                                    origin.x + (image.col as f32 + image_extra_cols) * cell_width,
                                    origin.y + image.row as f32 * line_height,
                                );
                                let image_bounds = Bounds::new(
                                    image_origin,
                                    size(
                                        cell_width * image.cols.max(1) as f32,
                                        line_height * image.rows.max(1) as f32,
                                    ),
                                );
                                let _ = window.paint_image(
                                    image_bounds,
                                    Corners::default(),
                                    image.image.clone(),
                                    0,
                                    false,
                                );
                            }
                        });

                        if focused
                            && let Some(text_to_mark) = ime_marked_text.as_ref()
                            && !text_to_mark.is_empty()
                        {
                            let marked_row = snapshot.cursor_row;
                            let marked_col = snapshot.cursor_col;
                            let marked_extra_cols = snapshot
                                .cells
                                .get(marked_row)
                                .map(|row| visual_extra_cols_before(row, marked_col))
                                .unwrap_or(0.0);
                            let marked_origin = point(
                                origin.x + (marked_col as f32 + marked_extra_cols) * cell_width,
                                origin.y + marked_row as f32 * line_height,
                            );
                            let ime_run = gpui::TextRun {
                                len: text_to_mark.len(),
                                underline: Some(gpui::UnderlineStyle {
                                    color: Some(run_template.color),
                                    thickness: px(1.0),
                                    wavy: false,
                                }),
                                ..run_template.clone()
                            };
                            let shaped = window.text_system().shape_line(
                                text_to_mark.clone().into(),
                                font_pixels,
                                &[ime_run],
                                None,
                            );
                            window.paint_quad(fill(
                                Bounds::new(marked_origin, size(shaped.width, line_height)),
                                palette.terminal_bg,
                            ));
                            let _ = shaped.paint(
                                marked_origin,
                                line_height,
                                gpui::TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }

                        if snapshot.cursor_visible && ime_marked_text.is_none() {
                            let cursor_logical_col_floor =
                                cursor_visual_col.max(0.0).floor() as usize;
                            let cursor_extra_cols = snapshot
                                .cells
                                .get(cursor_visual_row)
                                .map(|row| visual_extra_cols_before(row, cursor_logical_col_floor))
                                .unwrap_or(0.0);
                            let cursor_origin = point(
                                origin.x + (cursor_visual_col + cursor_extra_cols) * cell_width,
                                origin.y + cursor_visual_row as f32 * line_height,
                            );
                            let single_cell_width_px = cell_width.max(px(2.0));
                            // Block/Underline/HollowBlock 覆盖光标所在 cell 的全部
                            // 列宽（DSP-8）：停在 CJK/emoji 上要盖两格，不是左半格。
                            // Beam 是插入点，粗细仍按单格算。
                            let cursor_width_cols = snapshot
                                .cells
                                .get(cursor_visual_row)
                                .and_then(|row| row.get(cursor_logical_col_floor))
                                .map(|cell| f32::from(cell.width_cols.max(1)))
                                .unwrap_or(1.0);
                            let cell_width_px = single_cell_width_px * cursor_width_cols;
                            match cursor_shape {
                                CursorShape::Beam => {
                                    let beam_width = (single_cell_width_px * 0.14).max(px(2.0));
                                    if cursor_trail_enabled && cursor_sliding {
                                        let delta_cols = cursor_visual_col - cursor_anim_from_col;
                                        if delta_cols.abs() > f32::EPSILON {
                                            let trail_cells = (delta_cols.abs() * 0.7).clamp(
                                                CURSOR_TRAIL_MIN_LEN_CELLS,
                                                CURSOR_TRAIL_MAX_LEN_CELLS,
                                            );
                                            let primary_trail_width =
                                                single_cell_width_px * trail_cells;
                                            let primary_trail_origin_x =
                                                if delta_cols.is_sign_positive() {
                                                    cursor_origin.x - primary_trail_width
                                                } else {
                                                    cursor_origin.x + beam_width
                                                };
                                            let mut primary_trail_color = cursor_bg;
                                            primary_trail_color.a = (primary_trail_color.a
                                                * CURSOR_TRAIL_PRIMARY_ALPHA_SCALE)
                                                .clamp(0.0, 1.0);
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    point(primary_trail_origin_x, cursor_origin.y),
                                                    size(primary_trail_width, line_height),
                                                ),
                                                primary_trail_color,
                                            ));

                                            let secondary_trail_width = (primary_trail_width
                                                * CURSOR_TRAIL_SECONDARY_LEN_SCALE)
                                                .min(
                                                    single_cell_width_px
                                                        * (CURSOR_TRAIL_MAX_LEN_CELLS * 1.8),
                                                );
                                            let secondary_trail_origin_x =
                                                if delta_cols.is_sign_positive() {
                                                    cursor_origin.x - secondary_trail_width
                                                } else {
                                                    cursor_origin.x + beam_width
                                                };
                                            let mut secondary_trail_color = cursor_bg;
                                            secondary_trail_color.a = (secondary_trail_color.a
                                                * CURSOR_TRAIL_SECONDARY_ALPHA_SCALE)
                                                .clamp(0.0, 1.0);
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    point(
                                                        secondary_trail_origin_x,
                                                        cursor_origin.y,
                                                    ),
                                                    size(secondary_trail_width, line_height),
                                                ),
                                                secondary_trail_color,
                                            ));
                                        }
                                    }
                                    window.paint_quad(fill(
                                        Bounds::new(cursor_origin, size(beam_width, line_height)),
                                        cursor_bg,
                                    ));
                                }
                                CursorShape::Underline => {
                                    let underline_height = (line_height * 0.12).max(px(2.0));
                                    let underline_origin = point(
                                        cursor_origin.x,
                                        cursor_origin.y + line_height - underline_height,
                                    );
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            underline_origin,
                                            size(cell_width_px, underline_height),
                                        ),
                                        cursor_bg,
                                    ));
                                }
                                CursorShape::HollowBlock => {
                                    let border_x = (cell_width_px * 0.08).max(px(1.0));
                                    let border_y = (line_height * 0.08).max(px(1.0));

                                    window.paint_quad(fill(
                                        Bounds::new(cursor_origin, size(cell_width_px, border_y)),
                                        cursor_bg,
                                    ));
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(
                                                cursor_origin.x,
                                                cursor_origin.y + line_height - border_y,
                                            ),
                                            size(cell_width_px, border_y),
                                        ),
                                        cursor_bg,
                                    ));
                                    window.paint_quad(fill(
                                        Bounds::new(cursor_origin, size(border_x, line_height)),
                                        cursor_bg,
                                    ));
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(
                                                cursor_origin.x + cell_width_px - border_x,
                                                cursor_origin.y,
                                            ),
                                            size(border_x, line_height),
                                        ),
                                        cursor_bg,
                                    ));
                                }
                                CursorShape::Hidden => {}
                                CursorShape::Block => {
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            cursor_origin,
                                            size(cell_width_px, line_height),
                                        ),
                                        cursor_bg,
                                    ));
                                }
                            }
                        }
                    },
                )
                .size_full(),
            );

        let title_bar = div()
            .w_full()
            .h(CUSTOM_TITLE_BAR_HEIGHT)
            .px_3()
            .bg(palette.title_bg)
            .window_control_area(WindowControlArea::Drag)
            .flex()
            .items_center()
            .justify_between()
            .child(div().w(px(52.0)))
            .child(
                div()
                    .flex_1()
                    .text_color(palette.title_fg)
                    .font_family(font_family.clone())
                    .text_center()
                    .child(terminal_title),
            )
            .child(div().w(px(52.0)));

        let root = div()
            .id("agent-terminal")
            .size_full()
            .bg(palette.app_bg)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up));

        let root = if self.show_title_bar {
            root.child(title_bar)
        } else {
            root
        };

        let root = if self.show_status_bar {
            root.child(
                div()
                    .w_full()
                    .h(STATUS_BAR_HEIGHT)
                    .px_3()
                    .flex()
                    .items_center()
                    .bg(palette.title_bg)
                    .text_color(palette.title_fg)
                    .font_family(font_family.clone())
                    .child(status_line),
            )
        } else {
            root
        };

        root.child(terminal_surface)
    }
}

#[cfg(test)]
mod tests {
    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        haystack.match_indices(needle).count()
    }

    #[test]
    fn render_keeps_keydown_binding_on_root() {
        let source = include_str!("render.rs");
        let keydown_binding = [".on_key_down(", "cx.listener(Self::on_key_down)", ")"].concat();
        assert!(
            source.contains(&keydown_binding),
            "render root must bind keydown so Backspace/Ctrl combos reach terminal"
        );
    }

    #[test]
    fn render_binds_keydown_exactly_once() {
        let source = include_str!("render.rs");
        let keydown_binding = [".on_key_down(", "cx.listener(Self::on_key_down)", ")"].concat();
        assert_eq!(
            count_occurrences(source, &keydown_binding),
            1,
            "keydown listener should be bound once to avoid duplicate key handling"
        );
    }

    #[test]
    fn render_keeps_keyup_binding_on_root() {
        let source = include_str!("render.rs");
        let keyup_binding = [".on_key_up(", "cx.listener(Self::on_key_up)", ")"].concat();
        assert!(
            source.contains(&keyup_binding),
            "render root must bind keyup so kitty REPORT_EVENT_TYPES can emit release events"
        );
    }

    #[test]
    fn render_binds_keyup_exactly_once() {
        let source = include_str!("render.rs");
        let keyup_binding = [".on_key_up(", "cx.listener(Self::on_key_up)", ")"].concat();
        assert_eq!(
            count_occurrences(source, &keyup_binding),
            1,
            "keyup listener should be bound once to avoid duplicate release events"
        );
    }
}
