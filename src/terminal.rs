use std::collections::HashSet;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as AlacTermEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sixel::{SixelImage, decode_sixel_payload, decode_tmux_passthrough_sixel};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{ClipboardType, Config, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape, NamedColor, Processor, StdSyncHandler,
};
use anyhow::{Context as _, Result, ensure};
use gpui::{
    Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, FontFallbacks, Pixels, RenderImage,
    Subscription, Task, Window, px,
};
use parking_lot::Mutex;
use portable_pty::{Child, MasterPty, PtySize};
use serde::Serialize;
use serde_json::json;

use crate::cli::{AmbiguousWidth, CliOptions, Theme};
use crate::color::indexed_to_rgb;
use crate::color::{ansi_bg_to_hsla, ansi_to_hsla};
use crate::debug_server::{SharedDebugState, start_debug_http_server};
use crate::font_fallback::font_fallback_families;
use crate::input_log::InputLogger;
use crate::keyboard::encode_keystroke;
use crate::pty::{PtySession, SharedPtyWriter, write_to_pty};
use crate::render::{
    CUSTOM_TITLE_BAR_HEIGHT, STATUS_BAR_HEIGHT, TEXT_PADDING_X, TEXT_PADDING_Y, line_height_for,
    measure_cell_width,
};
use crate::snapshot_tab::SnapshotTabData;
use crate::text_utils::summarize_text_for_trace;

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 1;
pub(crate) const DEFAULT_FONT_SIZE: Pixels = px(14.0);
pub(crate) const MIN_FONT_SIZE: Pixels = px(8.0);
pub(crate) const MAX_FONT_SIZE: Pixels = px(48.0);
const INPUT_TRACE_ENV: &str = "AGENT_TUI_INPUT_TRACE";
const MAX_PTY_BATCH_CHUNKS: usize = 256;
const MAX_PTY_BATCH_BYTES: usize = 256 * 1024;
const CURSOR_SLIDE_DURATION: Duration = Duration::from_millis(80);
const CURSOR_SLIDE_MAX_COL_DELTA: f32 = 8.0;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AgentTerminalOptions {
    pub(crate) show_title_bar: bool,
}

impl Default for AgentTerminalOptions {
    fn default() -> Self {
        Self {
            show_title_bar: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TerminalExitedEvent;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct GridSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl Default for GridSize {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

#[derive(Clone)]
enum PendingTerminalEvent {
    ClipboardStore(ClipboardType, String),
    ClipboardLoad(
        ClipboardType,
        Arc<dyn Fn(&str) -> String + Sync + Send + 'static>,
    ),
    Scroll {
        region_top: usize,
        region_bottom: usize,
        delta: i32,
    },
    Erase {
        region_top: usize,
        region_bottom: usize,
    },
}

#[derive(Clone)]
pub(crate) struct TitleTrackingListener {
    pub(crate) title: Arc<Mutex<Option<String>>>,
    pub(crate) writer: Option<SharedPtyWriter>,
    pending_events: Arc<Mutex<Vec<PendingTerminalEvent>>>,
}

impl EventListener for TitleTrackingListener {
    fn send_event(&self, event: AlacTermEvent) {
        match event {
            AlacTermEvent::Title(title) => {
                *self.title.lock() = Some(title);
            }
            AlacTermEvent::ResetTitle => {
                *self.title.lock() = None;
            }
            AlacTermEvent::PtyWrite(text) => {
                if let Some(writer) = &self.writer {
                    let _ = write_to_pty(writer, text.as_bytes());
                }
            }
            AlacTermEvent::ClipboardStore(clipboard, text) => {
                self.pending_events
                    .lock()
                    .push(PendingTerminalEvent::ClipboardStore(clipboard, text));
            }
            AlacTermEvent::ClipboardLoad(clipboard, format) => {
                self.pending_events
                    .lock()
                    .push(PendingTerminalEvent::ClipboardLoad(clipboard, format));
            }
            AlacTermEvent::Scroll {
                region_top,
                region_bottom,
                delta,
            } => {
                self.pending_events
                    .lock()
                    .push(PendingTerminalEvent::Scroll {
                        region_top,
                        region_bottom,
                        delta,
                    });
            }
            AlacTermEvent::Erase {
                region_top,
                region_bottom,
            } => {
                self.pending_events.lock().push(PendingTerminalEvent::Erase {
                    region_top,
                    region_bottom,
                });
            }
            _ => {}
        }
    }
}

#[derive(Clone)]
pub(crate) struct TerminalImage {
    pub(crate) row: isize,
    pub(crate) col: usize,
    pub(crate) cols: usize,
    pub(crate) rows: usize,
    pub(crate) image: Arc<RenderImage>,
}

#[derive(Clone)]
pub(crate) struct CellSnapshot {
    pub(crate) ch: char,
    pub(crate) zerowidth: Vec<char>,
    pub(crate) fg: gpui::Hsla,
    pub(crate) bg: Option<gpui::Hsla>,
    pub(crate) link: Option<String>,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) undercurl: bool,
    pub(crate) strikethrough: bool,
    pub(crate) width_cols: u8,
    pub(crate) spans_next_col: bool,
    pub(crate) expands_layout: bool,
}

impl Default for CellSnapshot {
    fn default() -> Self {
        Self {
            ch: ' ',
            zerowidth: Vec::new(),
            fg: gpui::Hsla::default(),
            bg: None,
            link: None,
            bold: false,
            italic: false,
            underline: false,
            undercurl: false,
            strikethrough: false,
            width_cols: 1,
            spans_next_col: false,
            expands_layout: false,
        }
    }
}

fn cell_style_flags(flags: Flags) -> (bool, bool, bool, bool, bool) {
    (
        flags.contains(Flags::BOLD),
        flags.contains(Flags::ITALIC),
        flags.intersects(Flags::ALL_UNDERLINES),
        flags.contains(Flags::UNDERCURL),
        flags.contains(Flags::STRIKEOUT),
    )
}

impl CellSnapshot {
    pub(crate) fn push_text_to(&self, text: &mut String) {
        text.push(self.ch);
        text.extend(self.zerowidth.iter().copied());
    }

    pub(crate) fn text(&self) -> String {
        let mut text = String::with_capacity(
            self.ch.len_utf8() + self.zerowidth.iter().map(|ch| ch.len_utf8()).sum::<usize>(),
        );
        self.push_text_to(&mut text);
        text
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.ch == ' ' && self.zerowidth.is_empty()
    }
}

#[derive(Clone, Default)]
pub(crate) struct ScreenSnapshot {
    pub(crate) cells: Vec<Vec<CellSnapshot>>,
    pub(crate) cursor_row: usize,
    pub(crate) cursor_col: usize,
    pub(crate) cursor_visible: bool,
    pub(crate) alt_screen: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SelectionPoint {
    pub(crate) row: usize,
    pub(crate) col: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct EnterLatencyProbe {
    pub(crate) id: u64,
    pub(crate) keydown_at: Instant,
    pub(crate) write_done_at: Option<Instant>,
    pub(crate) first_pty_at: Option<Instant>,
}

pub(crate) struct AgentTerminal {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) term: Term<TitleTrackingListener>,
    pub(crate) processor: Processor<StdSyncHandler>,
    sixel_parser: SixelStreamParser,
    pub(crate) grid_size: GridSize,
    pub(crate) snapshot: ScreenSnapshot,
    pub(crate) images: Vec<TerminalImage>,
    pub(crate) cursor_shape: CursorShape,
    pub(crate) force_vertical_cursor: bool,
    pub(crate) cursor_slide_enabled: bool,
    pub(crate) cursor_trail_enabled: bool,
    pub(crate) option_as_meta: bool,
    pub(crate) cursor_visual_initialized: bool,
    pub(crate) cursor_visual_row: usize,
    pub(crate) cursor_anim_from_col: f32,
    pub(crate) cursor_anim_to_col: f32,
    pub(crate) cursor_anim_started_at: Option<Instant>,
    pub(crate) shell: String,
    pub(crate) terminal_title: Arc<Mutex<Option<String>>>,
    pub(crate) show_title_bar: bool,
    pub(crate) show_status_bar: bool,
    pub(crate) theme: Theme,
    pub(crate) font_family: String,
    pub(crate) font_fallbacks: Option<FontFallbacks>,
    pub(crate) forced_double_width_chars: HashSet<char>,
    pub(crate) font_size: Pixels,
    pub(crate) cell_width: Pixels,
    pty_pixel_size: PtyPixelSize,
    pub(crate) master: Option<Arc<Mutex<Box<dyn MasterPty + Send>>>>,
    pub(crate) writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    pub(crate) child: Option<Arc<Mutex<Box<dyn Child + Send>>>>,
    pub(crate) input_line: String,
    pub(crate) input_cursor_utf16: usize,
    pub(crate) ime_marked_text: Option<String>,
    pub(crate) last_ax_published_line: String,
    pub(crate) last_ax_published_cursor_utf16: usize,
    pub(crate) input_trace: bool,
    pub(crate) input_logger: Option<InputLogger>,
    pub(crate) last_local_key_event_at: Option<Instant>,
    pub(crate) last_focus_in_at: Option<Instant>,
    pub(crate) pty_sample_started_at: Instant,
    pub(crate) pty_sample_bytes: usize,
    pub(crate) pty_sample_chunks: usize,
    pub(crate) last_pty_chunk_at: Option<Instant>,
    pub(crate) enter_latency_seq: u64,
    pub(crate) enter_latency_probe: Option<EnterLatencyProbe>,
    pub(crate) mouse_scroll_accum_x: f32,
    pub(crate) mouse_scroll_accum_y: f32,
    pub(crate) last_mouse_report: Option<(usize, usize, u8)>,
    pub(crate) selection_mode_active: bool,
    pub(crate) selection_button: Option<gpui::MouseButton>,
    pub(crate) selection_anchor: Option<SelectionPoint>,
    pub(crate) selection_focus: Option<SelectionPoint>,
    pub(crate) canvas_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    pub(crate) paste_guard_prompt_open: bool,
    pub(crate) shell_exited: bool,
    pub(crate) debug: SharedDebugState,
    pending_term_events: Arc<Mutex<Vec<PendingTerminalEvent>>>,
    pub(crate) _window_bounds_sub: Option<Subscription>,
    pub(crate) _focus_in_sub: Option<Subscription>,
    pub(crate) _focus_out_sub: Option<Subscription>,
    pub(crate) _pump_task: Task<Result<()>>,
}

impl AgentTerminal {
    #[allow(dead_code)]
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>, cli: CliOptions) -> Self {
        Self::new_with_options(window, cx, cli, AgentTerminalOptions::default())
    }

    pub(crate) fn new_embedded(
        window: &mut Window,
        cx: &mut Context<Self>,
        cli: CliOptions,
    ) -> Self {
        Self::new_with_options(
            window,
            cx,
            cli,
            AgentTerminalOptions {
                show_title_bar: false,
            },
        )
    }

    pub(crate) fn new_with_options(
        window: &mut Window,
        cx: &mut Context<Self>,
        cli: CliOptions,
        options: AgentTerminalOptions,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let font_size = DEFAULT_FONT_SIZE;
        let font_fallbacks = parse_font_fallbacks(&cli.font_fallbacks);
        let forced_double_width_chars = parse_double_width_chars(&cli.double_width_chars);
        let cell_width =
            measure_cell_width(window, &cli.font_family, font_fallbacks.as_ref(), font_size);
        let viewport = window.viewport_size();
        let grid_size = compute_grid_size(
            viewport,
            cell_width,
            line_height_for(font_size),
            cli.show_status_bar,
        );
        let pty_pixel_size =
            pty_pixel_size_for_grid(grid_size, cell_width, line_height_for(font_size));

        let terminal_title = Arc::new(Mutex::new(None));
        let pending_term_events = Arc::new(Mutex::new(Vec::new()));
        let (shell, master, writer, child, output_rx, debug) = match PtySession::spawn(
            grid_size.rows,
            grid_size.cols,
            pty_pixel_size.width,
            pty_pixel_size.height,
        ) {
            Ok(session) => {
                let shell = session.shell.clone();
                let debug =
                    SharedDebugState::new(shell.clone(), "connected".to_string(), grid_size);
                (
                    shell,
                    Some(session.master),
                    Some(session.writer),
                    Some(session.child),
                    Some(session.output_rx),
                    debug,
                )
            }
            Err(err) => {
                let message = format!("failed to start shell: {err:#}");
                let debug = SharedDebugState::new("<none>".to_string(), message.clone(), grid_size);
                debug.set_error(message);
                (String::from("<none>"), None, None, None, None, debug)
            }
        };
        let term_config = Config {
            ambiguous_wide: matches!(cli.ambiguous_width, AmbiguousWidth::Double),
            kitty_keyboard: true,
            ..Config::default()
        };
        let term = Term::new(
            term_config,
            &grid_size,
            TitleTrackingListener {
                title: terminal_title.clone(),
                writer: writer.clone(),
                pending_events: pending_term_events.clone(),
            },
        );
        let processor = Processor::<StdSyncHandler>::new();

        start_debug_http_server(debug.clone(), writer.clone());
        let input_logger = match cli.input_log_file.as_ref() {
            Some(path) => match InputLogger::new(path, cli.input_log_raw) {
                Ok(logger) => {
                    logger.log_event(
                        "logger_started",
                        json!({
                            "path": path.to_string_lossy().to_string(),
                            "raw": cli.input_log_raw,
                            "shell": shell.clone(),
                        }),
                    );
                    Some(logger)
                }
                Err(err) => {
                    debug.set_error(format!(
                        "failed to open input log file {}: {err}",
                        path.to_string_lossy()
                    ));
                    None
                }
            },
            None => None,
        };

        let mut this = Self {
            focus_handle,
            term,
            processor,
            sixel_parser: SixelStreamParser::default(),
            grid_size,
            snapshot: ScreenSnapshot::default(),
            images: Vec::new(),
            cursor_shape: CursorShape::Block,
            force_vertical_cursor: cli.force_vertical_cursor,
            cursor_slide_enabled: !cli.no_cursor_slide,
            cursor_trail_enabled: cli.cursor_trail,
            option_as_meta: !cli.no_option_as_meta,
            cursor_visual_initialized: false,
            cursor_visual_row: 0,
            cursor_anim_from_col: 0.0,
            cursor_anim_to_col: 0.0,
            cursor_anim_started_at: None,
            shell,
            terminal_title: terminal_title.clone(),
            show_title_bar: options.show_title_bar,
            show_status_bar: cli.show_status_bar,
            theme: cli.theme,
            font_family: cli.font_family.clone(),
            font_fallbacks,
            forced_double_width_chars,
            font_size,
            cell_width,
            pty_pixel_size,
            master,
            writer,
            child,
            input_line: String::new(),
            input_cursor_utf16: 0,
            ime_marked_text: None,
            last_ax_published_line: String::new(),
            last_ax_published_cursor_utf16: 0,
            input_trace: is_input_trace_enabled(),
            input_logger,
            last_local_key_event_at: None,
            last_focus_in_at: None,
            pty_sample_started_at: Instant::now(),
            pty_sample_bytes: 0,
            pty_sample_chunks: 0,
            last_pty_chunk_at: None,
            enter_latency_seq: 0,
            enter_latency_probe: None,
            mouse_scroll_accum_x: 0.0,
            mouse_scroll_accum_y: 0.0,
            last_mouse_report: None,
            selection_mode_active: false,
            selection_button: None,
            selection_anchor: None,
            selection_focus: None,
            canvas_bounds: Arc::new(Mutex::new(None)),
            paste_guard_prompt_open: false,
            shell_exited: false,
            debug,
            pending_term_events,
            _window_bounds_sub: None,
            _focus_in_sub: None,
            _focus_out_sub: None,
            _pump_task: Task::ready(Ok(())),
        };

        this.refresh_snapshot();
        this._window_bounds_sub = Some(cx.observe_window_bounds(window, |this, window, cx| {
            this.sync_grid_to_window(window);
            cx.notify();
        }));
        this._focus_in_sub = Some(
            cx.on_focus(&this.focus_handle, window, |this, _window, _cx| {
                this.last_focus_in_at = Some(Instant::now());
                if this.term.mode().contains(TermMode::FOCUS_IN_OUT) {
                    this.write_bytes(b"\x1b[I");
                }
            }),
        );
        this._focus_out_sub =
            Some(
                cx.on_focus_out(&this.focus_handle, window, |this, _event, _window, _cx| {
                    if this.term.mode().contains(TermMode::FOCUS_IN_OUT) {
                        this.write_bytes(b"\x1b[O");
                    }
                }),
            );
        this.sync_grid_to_window(window);

        if let Some(rx) = output_rx {
            this._pump_task = cx.spawn(async move |this, cx| {
                while let Ok(bytes) = rx.recv().await {
                    let mut batch = vec![bytes];
                    let mut batch_bytes = batch[0].len();

                    // Low-latency batching: process first PTY chunk immediately,
                    // then drain currently queued chunks in one UI update.
                    while batch.len() < MAX_PTY_BATCH_CHUNKS && batch_bytes < MAX_PTY_BATCH_BYTES {
                        match rx.try_recv() {
                            Ok(next) => {
                                batch_bytes = batch_bytes.saturating_add(next.len());
                                batch.push(next);
                            }
                            Err(async_channel::TryRecvError::Empty) => break,
                            Err(async_channel::TryRecvError::Closed) => break,
                        }
                    }

                    this.update(cx, |this, cx| {
                        this.ingest_batch(cx, &batch);
                        this.mark_enter_latency_first_paint();
                        cx.notify();
                    })?;
                }
                let _ = this.update(cx, |this, cx| {
                    this.shell_exited = true;
                    this.debug.set_note(Some("shell exited".to_string()));
                    cx.emit(TerminalExitedEvent);
                    cx.notify();
                });
                Ok(())
            });
        }

        this
    }

    pub(crate) fn ingest_batch(&mut self, cx: &mut Context<Self>, chunks: &[Vec<u8>]) {
        if chunks.is_empty() {
            return;
        }

        self.mark_enter_latency_first_pty();
        for chunk in chunks {
            let actions = self.sixel_parser.advance(chunk);
            for action in actions {
                match action {
                    SixelStreamAction::Bytes(bytes) => {
                        self.advance_text_bytes(&bytes);
                    }
                    SixelStreamAction::Sixel(payload) => {
                        let (row, col) = self.current_cursor_anchor();
                        if let Some(image) = decode_sixel_payload(row, col, &payload) {
                            self.store_sixel_image(image);
                        }
                    }
                    SixelStreamAction::TmuxPassthrough { payload, raw } => {
                        let (row, col) = self.current_cursor_anchor();
                        if let Some(image) = decode_tmux_passthrough_sixel(row, col, &payload) {
                            self.store_sixel_image(image);
                        } else {
                            self.advance_text_bytes(&raw);
                        }
                    }
                    SixelStreamAction::UnknownDcs(raw) => {
                        self.advance_text_bytes(&raw);
                    }
                }
            }
            self.debug.record_bytes_from_pty(chunk.len());
            self.record_pty_ingest_diagnostics(chunk.len());
        }
        self.process_pending_terminal_events(cx);
        self.refresh_snapshot();
    }

    fn advance_text_bytes(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
    }

    fn process_pending_terminal_events(&mut self, cx: &mut Context<Self>) {
        let pending_events = {
            let mut pending = self.pending_term_events.lock();
            std::mem::take(&mut *pending)
        };

        for event in pending_events {
            match event {
                PendingTerminalEvent::ClipboardStore(clipboard, text) => {
                    self.store_osc52_clipboard(cx, clipboard, text);
                }
                PendingTerminalEvent::ClipboardLoad(clipboard, format) => {
                    let text = self.load_osc52_clipboard(cx, clipboard);
                    self.write_bytes(format(&text).as_bytes());
                }
                PendingTerminalEvent::Scroll {
                    region_top,
                    region_bottom,
                    delta,
                } => {
                    scroll_images_in_region(&mut self.images, region_top, region_bottom, delta);
                }
                PendingTerminalEvent::Erase {
                    region_top,
                    region_bottom,
                } => {
                    erase_images_in_region(&mut self.images, region_top, region_bottom);
                }
            }
        }
    }

    fn store_sixel_image(&mut self, image: SixelImage) {
        let row = image.row;
        let col = image.col;
        let width = image.width;
        let height = image.height;
        let line_height = self.line_height();
        let occupied_cols = sixel_occupied_cols(width, self.cell_width);
        let occupied_rows = sixel_occupied_rows(height, line_height);
        let Some(render_image) = render_image_from_sixel(&image) else {
            self.debug.set_error(format!(
                "invalid sixel image {}x{} at {},{}",
                width, height, row, col
            ));
            return;
        };

        self.images.push(TerminalImage {
            row: row as isize,
            col,
            cols: occupied_cols,
            rows: occupied_rows,
            image: render_image,
        });
        if self.images.len() > 128 {
            let remove_count = self.images.len() - 128;
            self.images.drain(0..remove_count);
        }
        reserve_sixel_layout(
            &mut self.processor,
            &mut self.term,
            occupied_cols,
            occupied_rows,
            row,
        );
        self.debug.set_note(Some(format!(
            "sixel image {}x{} at {},{}",
            width, height, row, col
        )));
    }

    fn current_cursor_anchor(&self) -> (usize, usize) {
        let content = self.term.renderable_content();
        let row = (content.cursor.point.line.0 + content.display_offset as i32).max(0) as usize;
        let col = content.cursor.point.column.0;
        (
            row.min(self.grid_size.rows.saturating_sub(1) as usize),
            col.min(self.grid_size.cols.saturating_sub(1) as usize),
        )
    }

    fn store_osc52_clipboard(
        &mut self,
        cx: &mut Context<Self>,
        clipboard: ClipboardType,
        text: String,
    ) {
        match clipboard {
            ClipboardType::Clipboard => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            ClipboardType::Selection => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
        self.debug
            .set_note(Some(format!("osc52 copied to {clipboard:?}")));
    }

    fn load_osc52_clipboard(&mut self, cx: &mut Context<Self>, clipboard: ClipboardType) -> String {
        let text = match clipboard {
            ClipboardType::Clipboard => cx.read_from_clipboard().and_then(|item| item.text()),
            ClipboardType::Selection => cx.read_from_clipboard().and_then(|item| item.text()),
        }
        .unwrap_or_default();

        self.debug
            .set_note(Some(format!("osc52 loaded from {clipboard:?}")));

        text
    }

    pub(crate) fn refresh_snapshot(&mut self) {
        let content = self.term.renderable_content();
        let rows = self.grid_size.rows as usize;
        let cols = self.grid_size.cols as usize;
        let alt_screen = content.mode.contains(TermMode::ALT_SCREEN);
        let mut cells = vec![
            vec![
                CellSnapshot {
                    ch: ' ',
                    zerowidth: Vec::new(),
                    fg: ansi_to_hsla(
                        AnsiColor::Named(NamedColor::Foreground),
                        content.colors,
                        Flags::empty(),
                        true,
                    ),
                    bg: None,
                    link: None,
                    bold: false,
                    italic: false,
                    underline: false,
                    undercurl: false,
                    strikethrough: false,
                    width_cols: 1,
                    spans_next_col: false,
                    expands_layout: false,
                };
                cols
            ];
            rows
        ];

        for indexed in content.display_iter {
            let row = indexed.point.line.0;
            let col = indexed.point.column.0;
            if row < 0 || col >= cols {
                continue;
            }

            let row = row as usize;
            if row >= rows {
                continue;
            }

            if indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            let mut fg = indexed.cell.fg;
            let mut bg = indexed.cell.bg;
            if indexed.cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }

            let ch = if indexed.cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                indexed.cell.c
            };
            let zerowidth = if indexed.cell.flags.contains(Flags::HIDDEN) {
                Vec::new()
            } else {
                indexed.cell.zerowidth().unwrap_or(&[]).to_vec()
            };
            let spans_next_col = indexed.cell.flags.contains(Flags::WIDE_CHAR);
            let expands_layout = !spans_next_col && self.forced_double_width_chars.contains(&ch);
            let width_cols = if spans_next_col || expands_layout {
                2
            } else {
                1
            };
            let (bold, italic, underline, undercurl, strikethrough) =
                cell_style_flags(indexed.cell.flags);

            cells[row][col] = CellSnapshot {
                ch,
                zerowidth,
                fg: ansi_to_hsla(fg, content.colors, indexed.cell.flags, true),
                bg: ansi_bg_to_hsla(bg, content.colors),
                link: indexed.cell.hyperlink().map(|link| link.uri().to_string()),
                bold,
                italic,
                underline,
                undercurl,
                strikethrough,
                width_cols,
                spans_next_col,
                expands_layout,
            };
        }
        annotate_plain_text_links(&mut cells);

        let cursor = content.cursor;
        let cursor_row = (cursor.point.line.0 + content.display_offset as i32).max(0) as usize;
        let cursor_col = cursor.point.column.0.min(cols.saturating_sub(1));
        let effective_cursor_shape =
            if self.force_vertical_cursor && cursor.shape != CursorShape::Hidden {
                CursorShape::Beam
            } else {
                cursor.shape
            };
        self.cursor_shape = effective_cursor_shape;
        self.update_cursor_visual_target(cursor_row.min(rows.saturating_sub(1)), cursor_col);

        self.snapshot = ScreenSnapshot {
            cells,
            cursor_row: cursor_row.min(rows.saturating_sub(1)),
            cursor_col,
            cursor_visible: cursor.shape != CursorShape::Hidden,
            alt_screen,
        };

        self.debug.update_screen_snapshot(
            self.grid_size,
            self.snapshot.cursor_row,
            self.snapshot.cursor_col,
            snapshot_to_lines(&self.snapshot),
        );
    }

    pub(crate) fn sync_grid_to_window(&mut self, window: &mut Window) {
        let cell_width = measure_cell_width(
            window,
            &self.font_family,
            self.font_fallbacks.as_ref(),
            self.font_size,
        );
        let viewport = window.viewport_size();
        let new_grid = compute_grid_size(
            viewport,
            cell_width,
            self.line_height(),
            self.show_status_bar,
        );
        self.cell_width = cell_width;
        let pty_pixel_size = pty_pixel_size_for_grid(new_grid, cell_width, self.line_height());
        self.apply_grid_size(new_grid, pty_pixel_size);
    }

    pub(crate) fn line_height(&self) -> Pixels {
        line_height_for(self.font_size)
    }

    pub(crate) fn adjust_font_size(&mut self, delta: Pixels, window: &mut Window) {
        let next_size = (self.font_size + delta)
            .max(MIN_FONT_SIZE)
            .min(MAX_FONT_SIZE);
        if next_size == self.font_size {
            return;
        }

        self.font_size = next_size;
        self.sync_grid_to_window(window);
    }

    pub(crate) fn apply_grid_size(&mut self, new_grid: GridSize, pty_pixel_size: PtyPixelSize) {
        if new_grid == self.grid_size && pty_pixel_size == self.pty_pixel_size {
            return;
        }

        let grid_changed = new_grid != self.grid_size;
        self.grid_size = new_grid;
        self.pty_pixel_size = pty_pixel_size;
        if grid_changed {
            self.term.resize(new_grid);
        }

        if let Some(master) = &self.master
            && let Err(err) = master.lock().resize(PtySize {
                rows: new_grid.rows,
                cols: new_grid.cols,
                pixel_width: pty_pixel_size.width,
                pixel_height: pty_pixel_size.height,
            })
        {
            self.debug.set_error(format!("pty resize failed: {err:#}"));
        }

        self.debug.record_resize();
        self.refresh_snapshot();
    }

    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) {
        let Some(writer) = &self.writer else {
            self.debug
                .set_error("write skipped because PTY writer is unavailable");
            return;
        };

        match write_to_pty(writer, bytes) {
            Ok(()) => {
                self.debug.record_bytes_to_pty(bytes.len(), false);
            }
            Err(err) => {
                self.debug
                    .set_error(format!("failed to write input: {err:#}"));
            }
        }
    }

    pub(crate) fn tab_title(&self) -> String {
        self.terminal_title
            .lock()
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| self.shell.clone())
    }

    pub(crate) fn capture_snapshot_data(&self, title: String) -> SnapshotTabData {
        let grid = self.term.grid();
        let colors = self.term.colors();
        let cols = grid.columns();
        let top = grid.topmost_line().0;
        let bottom = grid.bottommost_line().0;
        let default_fg = ansi_to_hsla(
            AnsiColor::Named(NamedColor::Foreground),
            colors,
            Flags::empty(),
            true,
        );

        let mut lines = Vec::with_capacity((bottom - top + 1).max(0) as usize);
        for line_index in top..=bottom {
            let mut row = vec![
                CellSnapshot {
                    ch: ' ',
                    zerowidth: Vec::new(),
                    fg: default_fg,
                    bg: None,
                    link: None,
                    bold: false,
                    italic: false,
                    underline: false,
                    undercurl: false,
                    strikethrough: false,
                    width_cols: 1,
                    spans_next_col: false,
                    expands_layout: false,
                };
                cols
            ];

            for col in 0..cols {
                let cell = &grid[Line(line_index)][Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }

                let mut fg = cell.fg;
                let mut bg = cell.bg;
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }

                let ch = if cell.flags.contains(Flags::HIDDEN) {
                    ' '
                } else {
                    cell.c
                };
                let zerowidth = if cell.flags.contains(Flags::HIDDEN) {
                    Vec::new()
                } else {
                    cell.zerowidth().unwrap_or(&[]).to_vec()
                };
                let spans_next_col = cell.flags.contains(Flags::WIDE_CHAR);
                let expands_layout =
                    !spans_next_col && self.forced_double_width_chars.contains(&ch);
                let width_cols = if spans_next_col || expands_layout {
                    2
                } else {
                    1
                };
                let (bold, italic, underline, undercurl, strikethrough) =
                    cell_style_flags(cell.flags);

                row[col] = CellSnapshot {
                    ch,
                    zerowidth,
                    fg: ansi_to_hsla(fg, colors, cell.flags, true),
                    bg: ansi_bg_to_hsla(bg, colors),
                    link: cell.hyperlink().map(|link| link.uri().to_string()),
                    bold,
                    italic,
                    underline,
                    undercurl,
                    strikethrough,
                    width_cols,
                    spans_next_col,
                    expands_layout,
                };
            }
            annotate_plain_text_links_for_row(&mut row);

            lines.push(row);
        }

        SnapshotTabData {
            title,
            lines,
            cols,
            font_family: self.font_family.clone(),
            font_fallbacks: self.font_fallbacks.clone(),
            font_size: self.font_size,
            theme: self.theme,
        }
    }

    pub(crate) fn cursor_visual_state(&self) -> (usize, f32, bool) {
        if !self.cursor_slide_enabled {
            return (
                self.snapshot.cursor_row,
                self.snapshot.cursor_col as f32,
                false,
            );
        }

        if !self.cursor_visual_initialized {
            return (
                self.snapshot.cursor_row,
                self.snapshot.cursor_col as f32,
                false,
            );
        }

        let now = Instant::now();
        (
            self.cursor_visual_row,
            self.cursor_visual_col_at(now),
            self.cursor_animation_active_at(now),
        )
    }

    fn update_cursor_visual_target(&mut self, row: usize, col: usize) {
        let target_col = col as f32;
        let now = Instant::now();

        if !self.cursor_slide_enabled {
            self.cursor_visual_initialized = true;
            self.cursor_visual_row = row;
            self.cursor_anim_from_col = target_col;
            self.cursor_anim_to_col = target_col;
            self.cursor_anim_started_at = None;
            return;
        }

        if !self.cursor_visual_initialized {
            self.cursor_visual_initialized = true;
            self.cursor_visual_row = row;
            self.cursor_anim_from_col = target_col;
            self.cursor_anim_to_col = target_col;
            self.cursor_anim_started_at = None;
            return;
        }

        let current_col = self.cursor_visual_col_at(now);
        let row_changed = self.cursor_visual_row != row;
        let large_delta = (target_col - current_col).abs() > CURSOR_SLIDE_MAX_COL_DELTA;
        if row_changed || large_delta {
            self.cursor_visual_row = row;
            self.cursor_anim_from_col = target_col;
            self.cursor_anim_to_col = target_col;
            self.cursor_anim_started_at = None;
            return;
        }

        if (target_col - self.cursor_anim_to_col).abs() < f32::EPSILON {
            if !self.cursor_animation_active_at(now) {
                self.cursor_anim_from_col = target_col;
                self.cursor_anim_to_col = target_col;
                self.cursor_anim_started_at = None;
            }
            self.cursor_visual_row = row;
            return;
        }

        self.cursor_visual_row = row;
        self.cursor_anim_from_col = current_col;
        self.cursor_anim_to_col = target_col;
        self.cursor_anim_started_at = Some(now);
    }

    fn cursor_visual_col_at(&self, now: Instant) -> f32 {
        let Some(started_at) = self.cursor_anim_started_at else {
            return self.cursor_anim_to_col;
        };

        let elapsed = now.saturating_duration_since(started_at);
        let duration_ms = CURSOR_SLIDE_DURATION.as_millis().max(1) as f32;
        let progress = (elapsed.as_millis() as f32 / duration_ms).clamp(0.0, 1.0);
        let eased = progress * (2.0 - progress); // ease-out quad
        self.cursor_anim_from_col + (self.cursor_anim_to_col - self.cursor_anim_from_col) * eased
    }

    fn cursor_animation_active_at(&self, now: Instant) -> bool {
        let Some(started_at) = self.cursor_anim_started_at else {
            return false;
        };
        now.saturating_duration_since(started_at) < CURSOR_SLIDE_DURATION
            && (self.cursor_anim_to_col - self.cursor_anim_from_col).abs() >= f32::EPSILON
    }

    pub(crate) fn start_enter_latency_probe(&mut self, input_line: &str) -> u64 {
        if let Some(previous) = self.enter_latency_probe.take() {
            self.log_enter_latency_event(
                "enter_latency_abandoned",
                previous.id,
                &previous,
                Instant::now(),
                json!({ "reason": "superseded_by_new_enter" }),
            );
        }

        self.enter_latency_seq = self.enter_latency_seq.saturating_add(1);
        let probe = EnterLatencyProbe {
            id: self.enter_latency_seq,
            keydown_at: Instant::now(),
            write_done_at: None,
            first_pty_at: None,
        };

        self.log_enter_latency_event(
            "enter_latency_start",
            probe.id,
            &probe,
            probe.keydown_at,
            json!({
                "input_line": summarize_text_for_trace(input_line),
            }),
        );
        self.enter_latency_probe = Some(probe);
        self.enter_latency_seq
    }

    pub(crate) fn mark_enter_latency_write_done(&mut self, probe_id: u64, bytes_len: usize) {
        let now = Instant::now();
        let Some(probe) = self.enter_latency_probe.as_mut() else {
            return;
        };
        if probe.id != probe_id || probe.write_done_at.is_some() {
            return;
        }

        probe.write_done_at = Some(now);
        let snapshot = probe.clone();
        self.log_enter_latency_event(
            "enter_latency_write_done",
            probe_id,
            &snapshot,
            now,
            json!({
                "bytes_len": bytes_len,
            }),
        );
    }

    pub(crate) fn mark_enter_latency_first_pty(&mut self) {
        let now = Instant::now();
        let Some(probe) = self.enter_latency_probe.as_mut() else {
            return;
        };
        if probe.write_done_at.is_none() || probe.first_pty_at.is_some() {
            return;
        }

        probe.first_pty_at = Some(now);
        let snapshot = probe.clone();
        self.log_enter_latency_event(
            "enter_latency_first_pty",
            snapshot.id,
            &snapshot,
            now,
            json!({}),
        );
    }

    pub(crate) fn mark_enter_latency_first_paint(&mut self) {
        let now = Instant::now();
        let Some(probe) = self.enter_latency_probe.take() else {
            return;
        };
        if probe.first_pty_at.is_none() {
            self.enter_latency_probe = Some(probe);
            return;
        }

        self.log_enter_latency_event(
            "enter_latency_first_paint",
            probe.id,
            &probe,
            now,
            json!({}),
        );
    }

    fn log_enter_latency_event(
        &self,
        event: &str,
        probe_id: u64,
        probe: &EnterLatencyProbe,
        at: Instant,
        extra: serde_json::Value,
    ) {
        let Some(logger) = &self.input_logger else {
            return;
        };

        let keydown_to_now_ms = at.saturating_duration_since(probe.keydown_at).as_millis();
        let keydown_to_write_ms = probe
            .write_done_at
            .map(|t| t.saturating_duration_since(probe.keydown_at).as_millis());
        let write_to_now_ms = probe
            .write_done_at
            .map(|t| at.saturating_duration_since(t).as_millis());
        let keydown_to_first_pty_ms = probe
            .first_pty_at
            .map(|t| t.saturating_duration_since(probe.keydown_at).as_millis());
        let first_pty_to_now_ms = probe
            .first_pty_at
            .map(|t| at.saturating_duration_since(t).as_millis());

        logger.log_event(
            event,
            json!({
                "probe_id": probe_id,
                "keydown_to_now_ms": keydown_to_now_ms,
                "keydown_to_write_ms": keydown_to_write_ms,
                "write_to_now_ms": write_to_now_ms,
                "keydown_to_first_pty_ms": keydown_to_first_pty_ms,
                "first_pty_to_now_ms": first_pty_to_now_ms,
                "extra": extra,
            }),
        );
    }

    fn record_pty_ingest_diagnostics(&mut self, chunk_len: usize) {
        let Some(logger) = &self.input_logger else {
            return;
        };

        let now = Instant::now();
        if let Some(last_chunk_at) = self.last_pty_chunk_at {
            let gap = now.saturating_duration_since(last_chunk_at);
            if gap >= Duration::from_millis(800) {
                logger.log_event(
                    "pty_ingest_gap",
                    json!({
                        "gap_ms": gap.as_millis(),
                        "chunk_len": chunk_len,
                    }),
                );
            }
        }
        self.last_pty_chunk_at = Some(now);

        self.pty_sample_bytes += chunk_len;
        self.pty_sample_chunks += 1;

        let window = now.saturating_duration_since(self.pty_sample_started_at);
        if window < Duration::from_millis(500) {
            return;
        }

        let window_ms = window.as_millis().max(1);
        let bytes = self.pty_sample_bytes as u128;
        let chunks = self.pty_sample_chunks as u128;

        logger.log_event(
            "pty_ingest_sample",
            json!({
                "window_ms": window_ms,
                "bytes": bytes,
                "chunks": chunks,
                "bytes_per_sec": bytes.saturating_mul(1000) / window_ms,
                "chunks_per_sec": chunks.saturating_mul(1000) / window_ms,
            }),
        );

        self.pty_sample_started_at = now;
        self.pty_sample_bytes = 0;
        self.pty_sample_chunks = 0;
    }
}

impl EventEmitter<TerminalExitedEvent> for AgentTerminal {}

impl Drop for AgentTerminal {
    fn drop(&mut self) {
        if let Some(child) = &self.child {
            let _ = child.lock().kill();
        }
    }
}

pub(crate) fn compute_grid_size(
    viewport: gpui::Size<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    show_status_bar: bool,
) -> GridSize {
    let mut usable_width = viewport.width - (TEXT_PADDING_X * 2.0);
    let status_height = if show_status_bar {
        STATUS_BAR_HEIGHT
    } else {
        px(0.0)
    };
    let mut usable_height =
        viewport.height - CUSTOM_TITLE_BAR_HEIGHT - status_height - (TEXT_PADDING_Y * 2.0);

    if usable_width < cell_width {
        usable_width = cell_width;
    }
    if usable_height < line_height {
        usable_height = line_height;
    }

    let cols = ((usable_width / cell_width).floor() as u32).max(MIN_COLS as u32) as u16;
    let rows = ((usable_height / line_height).floor() as u32).max(MIN_ROWS as u32) as u16;

    GridSize { cols, rows }
}

pub(crate) fn snapshot_to_lines(snapshot: &ScreenSnapshot) -> Vec<String> {
    snapshot
        .cells
        .iter()
        .map(|row| {
            let mut line = row_text_without_wide_spacers(row);
            while line.ends_with(' ') {
                line.pop();
            }
            line
        })
        .collect()
}

fn annotate_plain_text_links(cells: &mut [Vec<CellSnapshot>]) {
    for row in cells {
        annotate_plain_text_links_for_row(row);
    }
}

fn annotate_plain_text_links_for_row(row: &mut [CellSnapshot]) {
    let mut text = String::with_capacity(row.len());
    let mut char_cols = Vec::with_capacity(row.len());
    let mut col = 0usize;
    while col < row.len() {
        let cell = &row[col];
        let before = text.chars().count();
        cell.push_text_to(&mut text);
        let after = text.chars().count();
        char_cols.extend(std::iter::repeat_n(col, after.saturating_sub(before)));
        col = col.saturating_add(cell_advance_cols(cell));
    }

    for (start, end, uri) in find_plain_text_links(&text) {
        for text_col in start..end.min(char_cols.len()) {
            let cell_col = char_cols[text_col];
            if row[cell_col].link.is_none() {
                row[cell_col].link = Some(uri.clone());
            }
        }
    }
}

fn find_plain_text_links(text: &str) -> Vec<(usize, usize, String)> {
    let mut links = Vec::new();
    let mut search_start = 0usize;
    while search_start < text.len() {
        let Some((scheme_start, scheme)) = find_next_url_scheme(&text[search_start..]) else {
            break;
        };
        let start = search_start + scheme_start;
        let mut end = start + scheme.len();
        for (offset, ch) in text[end..].char_indices() {
            if is_url_body_char(ch) {
                end = start + scheme.len() + offset + ch.len_utf8();
            } else {
                break;
            }
        }

        while end > start {
            let Some(ch) = text[..end].chars().next_back() else {
                break;
            };
            if is_url_trailing_punctuation(ch) {
                end -= ch.len_utf8();
            } else {
                break;
            }
        }

        if end > start + scheme.len() {
            let start_col = text[..start].chars().count();
            let end_col = text[..end].chars().count();
            links.push((start_col, end_col, text[start..end].to_string()));
        }
        search_start = end.max(start + scheme.len());
    }
    links
}

fn find_next_url_scheme(text: &str) -> Option<(usize, &'static str)> {
    ["https://", "http://", "file://"]
        .into_iter()
        .filter_map(|scheme| text.find(scheme).map(|index| (index, scheme)))
        .min_by_key(|(index, _)| *index)
}

fn is_url_body_char(ch: char) -> bool {
    !ch.is_whitespace() && !ch.is_control() && ch != '<' && ch != '>' && ch != '"' && ch != '\''
}

fn is_url_trailing_punctuation(ch: char) -> bool {
    matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
}

fn is_input_trace_enabled() -> bool {
    std::env::var(INPUT_TRACE_ENV)
        .ok()
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
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
    if occupied_rows > 1 {
        reservation.extend_from_slice(format!("\x1b[{}B", occupied_rows - 1).as_bytes());
    }
    reservation.push(b'\r');
    reservation.push(b'\n');
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PtyPixelSize {
    width: u16,
    height: u16,
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

#[derive(Default)]
struct SixelStreamParser {
    state: SixelStreamState,
}

enum SixelStreamAction {
    Bytes(Vec<u8>),
    Sixel(Vec<u8>),
    TmuxPassthrough { payload: Vec<u8>, raw: Vec<u8> },
    UnknownDcs(Vec<u8>),
}

#[derive(Default)]
enum SixelStreamState {
    #[default]
    Ground,
    Escape,
    DcsEntry {
        raw: Vec<u8>,
    },
    DcsData {
        action: u8,
        raw: Vec<u8>,
        payload: Vec<u8>,
        pending_escape: bool,
    },
}

impl SixelStreamParser {
    fn advance(&mut self, bytes: &[u8]) -> Vec<SixelStreamAction> {
        let mut actions = Vec::new();
        let mut output = Vec::new();

        for byte in bytes.iter().copied() {
            self.advance_byte(byte, &mut output, &mut actions);
        }

        if !output.is_empty() {
            actions.push(SixelStreamAction::Bytes(output));
        }
        actions
    }

    fn advance_byte(
        &mut self,
        byte: u8,
        output: &mut Vec<u8>,
        actions: &mut Vec<SixelStreamAction>,
    ) {
        let state = std::mem::take(&mut self.state);
        self.state = match state {
            SixelStreamState::Ground => {
                if byte == 0x1b {
                    SixelStreamState::Escape
                } else {
                    output.push(byte);
                    SixelStreamState::Ground
                }
            }
            SixelStreamState::Escape => match byte {
                b'P' => SixelStreamState::DcsEntry {
                    raw: vec![0x1b, b'P'],
                },
                0x1b => {
                    output.push(0x1b);
                    SixelStreamState::Escape
                }
                _ => {
                    output.push(0x1b);
                    output.push(byte);
                    SixelStreamState::Ground
                }
            },
            SixelStreamState::DcsEntry { mut raw } => {
                raw.push(byte);
                if (0x40..=0x7e).contains(&byte) {
                    SixelStreamState::DcsData {
                        action: byte,
                        raw,
                        payload: Vec::new(),
                        pending_escape: false,
                    }
                } else {
                    SixelStreamState::DcsEntry { raw }
                }
            }
            SixelStreamState::DcsData {
                action,
                mut raw,
                mut payload,
                mut pending_escape,
            } => {
                raw.push(byte);
                if byte == 0x9c {
                    self.finish_dcs(action, raw, payload, output, actions);
                    SixelStreamState::Ground
                } else if pending_escape {
                    if action == b't' && byte == 0x1b {
                        payload.push(0x1b);
                        payload.push(byte);
                        SixelStreamState::DcsData {
                            action,
                            raw,
                            payload,
                            pending_escape: false,
                        }
                    } else if byte == b'\\' {
                        self.finish_dcs(action, raw, payload, output, actions);
                        SixelStreamState::Ground
                    } else {
                        payload.push(0x1b);
                        payload.push(byte);
                        pending_escape = byte == 0x1b;
                        SixelStreamState::DcsData {
                            action,
                            raw,
                            payload,
                            pending_escape,
                        }
                    }
                } else if byte == 0x1b {
                    pending_escape = true;
                    SixelStreamState::DcsData {
                        action,
                        raw,
                        payload,
                        pending_escape,
                    }
                } else {
                    payload.push(byte);
                    SixelStreamState::DcsData {
                        action,
                        raw,
                        payload,
                        pending_escape,
                    }
                }
            }
        };
    }

    fn finish_dcs(
        &self,
        action: u8,
        raw: Vec<u8>,
        payload: Vec<u8>,
        output: &mut Vec<u8>,
        actions: &mut Vec<SixelStreamAction>,
    ) {
        if !output.is_empty() {
            actions.push(SixelStreamAction::Bytes(std::mem::take(output)));
        }

        match action {
            b'q' => actions.push(SixelStreamAction::Sixel(payload)),
            b't' => actions.push(SixelStreamAction::TmuxPassthrough { payload, raw }),
            _ => actions.push(SixelStreamAction::UnknownDcs(raw)),
        }
    }
}

fn parse_font_fallbacks(raw: &[String]) -> Option<FontFallbacks> {
    let fallbacks = font_fallback_families(raw);
    if fallbacks.is_empty() {
        None
    } else {
        Some(FontFallbacks::from_fonts(fallbacks))
    }
}

fn cell_advance_cols(cell: &CellSnapshot) -> usize {
    if cell.spans_next_col {
        usize::from(cell.width_cols.max(1))
    } else {
        1
    }
}

fn row_text_without_wide_spacers(row: &[CellSnapshot]) -> String {
    let mut text = String::new();
    let mut col = 0usize;
    while col < row.len() {
        let cell = &row[col];
        cell.push_text_to(&mut text);
        col = col.saturating_add(cell_advance_cols(cell));
    }
    text
}

fn parse_double_width_chars(raw: &[String]) -> HashSet<char> {
    raw.iter()
        .flat_map(|entry| entry.chars())
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::Osc52;
    use std::io::{Result as IoResult, Write};

    struct RecordingWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
            self.bytes.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    fn recording_writer() -> (SharedPtyWriter, Arc<Mutex<Vec<u8>>>) {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: SharedPtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter {
            bytes: bytes.clone(),
        })));
        (writer, bytes)
    }

    fn dummy_render_image() -> Arc<RenderImage> {
        let pixels = image::RgbaImage::from_raw(1, 1, vec![0, 0, 0, 255]).unwrap();
        Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]))
    }

    #[test]
    fn device_status_report_writes_cursor_position_to_pty() {
        let (writer, bytes) = recording_writer();
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let mut term = Term::new(
            Config::default(),
            &GridSize { cols: 80, rows: 24 },
            TitleTrackingListener {
                title,
                writer: Some(writer),
                pending_events,
            },
        );
        let mut processor = Processor::<StdSyncHandler>::new();

        processor.advance(&mut term, b"abc");
        processor.advance(&mut term, b"\x1b[6n");

        assert_eq!(&*bytes.lock(), b"\x1b[1;4R");
    }

    #[test]
    fn osc52_copy_sequence_is_queued_for_clipboard_store() {
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let mut term = Term::new(
            Config::default(),
            &GridSize { cols: 80, rows: 24 },
            TitleTrackingListener {
                title,
                writer: None,
                pending_events: pending_events.clone(),
            },
        );
        let mut processor = Processor::<StdSyncHandler>::new();

        processor.advance(&mut term, b"\x1b]52;c;SGVsbG8=\x07");

        let events = pending_events.lock().clone();
        assert_eq!(events.len(), 1);
        match &events[0] {
            PendingTerminalEvent::ClipboardStore(ClipboardType::Clipboard, text) => {
                assert_eq!(text, "Hello");
            }
            _ => panic!("expected clipboard store event"),
        }
    }

    #[test]
    fn osc52_paste_query_is_queued_for_clipboard_load() {
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            osc52: Osc52::CopyPaste,
            ..Config::default()
        };
        let mut term = Term::new(
            config,
            &GridSize { cols: 80, rows: 24 },
            TitleTrackingListener {
                title,
                writer: None,
                pending_events: pending_events.clone(),
            },
        );
        let mut processor = Processor::<StdSyncHandler>::new();

        processor.advance(&mut term, b"\x1b]52;c;?\x07");

        let events = pending_events.lock().clone();
        assert_eq!(events.len(), 1);
        match &events[0] {
            PendingTerminalEvent::ClipboardLoad(ClipboardType::Clipboard, _) => {}
            _ => panic!("expected clipboard load event"),
        }
    }

    #[test]
    fn sixel_stream_parser_extracts_sixel_and_preserves_text() {
        let mut parser = SixelStreamParser::default();
        let actions = parser.advance(b"before\x1bPq~\x1b\\after");

        assert_eq!(actions.len(), 3);
        match &actions[0] {
            SixelStreamAction::Bytes(bytes) => assert_eq!(bytes, b"before"),
            _ => panic!("expected leading text"),
        }
        match &actions[1] {
            SixelStreamAction::Sixel(payload) => assert_eq!(payload, b"~"),
            _ => panic!("expected sixel payload"),
        }
        match &actions[2] {
            SixelStreamAction::Bytes(bytes) => assert_eq!(bytes, b"after"),
            _ => panic!("expected trailing text"),
        }
    }

    #[test]
    fn sixel_stream_parser_keeps_tmux_escaped_st_until_outer_terminator() {
        let mut parser = SixelStreamParser::default();
        let actions = parser.advance(b"before\x1bPtmux;\x1b\x1bPq~\x1b\x1b\\\x1b\\after");

        assert_eq!(actions.len(), 3);
        match &actions[0] {
            SixelStreamAction::Bytes(bytes) => assert_eq!(bytes, b"before"),
            _ => panic!("expected leading text"),
        }
        match &actions[1] {
            SixelStreamAction::TmuxPassthrough { payload, .. } => {
                assert_eq!(payload, b"mux;\x1b\x1bPq~\x1b\x1b\\");
                assert!(decode_tmux_passthrough_sixel(0, 0, payload).is_some());
            }
            _ => panic!("expected tmux passthrough payload"),
        }
        match &actions[2] {
            SixelStreamAction::Bytes(bytes) => assert_eq!(bytes, b"after"),
            _ => panic!("expected trailing text"),
        }
    }

    #[test]
    fn sixel_layout_reservation_places_following_text_below_image() {
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let mut term = Term::new(
            Config::default(),
            &GridSize { cols: 80, rows: 6 },
            TitleTrackingListener {
                title,
                writer: None,
                pending_events,
            },
        );
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
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let mut term = Term::new(
            Config::default(),
            &GridSize { cols: 80, rows: 4 },
            TitleTrackingListener {
                title,
                writer: None,
                pending_events,
            },
        );
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
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let mut term = Term::new(
            Config::default(),
            &GridSize { cols: 80, rows: 4 },
            TitleTrackingListener {
                title,
                writer: None,
                pending_events,
            },
        );
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

    #[test]
    fn pty_pixel_size_matches_rendered_grid_cell_area() {
        let pixel_size =
            pty_pixel_size_for_grid(GridSize { cols: 80, rows: 24 }, px(7.5), px(18.0));

        assert_eq!(pixel_size.width, 600);
        assert_eq!(pixel_size.height, 432);
    }

    #[test]
    fn kitty_keyboard_query_is_ignored_when_terminal_config_disables_protocol() {
        let (writer, bytes) = recording_writer();
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let mut term = Term::new(
            Config::default(),
            &GridSize { cols: 80, rows: 24 },
            TitleTrackingListener {
                title,
                writer: Some(writer),
                pending_events,
            },
        );
        let mut processor = Processor::<StdSyncHandler>::new();

        processor.advance(&mut term, b"\x1b[?u");

        assert!(bytes.lock().is_empty());
        assert!(!term.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
    }

    #[test]
    fn kitty_keyboard_runtime_push_enables_mode_when_app_config_allows_protocol() {
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        let mut term = Term::new(
            config,
            &GridSize { cols: 80, rows: 24 },
            TitleTrackingListener {
                title,
                writer: None,
                pending_events,
            },
        );
        let mut processor = Processor::<StdSyncHandler>::new();

        processor.advance(&mut term, b"\x1b[>1u");

        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
    }

    #[test]
    fn kitty_keyboard_set_mode_updates_active_mode_when_enabled() {
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        let mut term = Term::new(
            config,
            &GridSize { cols: 80, rows: 24 },
            TitleTrackingListener {
                title,
                writer: None,
                pending_events,
            },
        );
        let mut processor = Processor::<StdSyncHandler>::new();

        processor.advance(&mut term, b"\x1b[=1;1u");

        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
    }

    #[test]
    fn kitty_keyboard_query_reports_pushed_mode_when_enabled() {
        let (writer, bytes) = recording_writer();
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        let mut term = Term::new(
            config,
            &GridSize { cols: 80, rows: 24 },
            TitleTrackingListener {
                title,
                writer: Some(writer),
                pending_events,
            },
        );
        let mut processor = Processor::<StdSyncHandler>::new();

        processor.advance(&mut term, b"\x1b[>1u");
        processor.advance(&mut term, b"\x1b[?u");

        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert_eq!(&*bytes.lock(), b"\x1b[?1u");
    }

    #[test]
    fn kitty_keyboard_push_and_pop_modes_when_enabled() {
        let title = Arc::new(Mutex::new(None));
        let pending_events = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        let mut term = Term::new(
            config,
            &GridSize { cols: 80, rows: 24 },
            TitleTrackingListener {
                title,
                writer: None,
                pending_events,
            },
        );
        let mut processor = Processor::<StdSyncHandler>::new();

        processor.advance(&mut term, b"\x1b[>1u");
        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert!(!term.mode().contains(TermMode::REPORT_EVENT_TYPES));

        processor.advance(&mut term, b"\x1b[>3u");
        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert!(term.mode().contains(TermMode::REPORT_EVENT_TYPES));

        processor.advance(&mut term, b"\x1b[<u");
        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert!(!term.mode().contains(TermMode::REPORT_EVENT_TYPES));

        processor.advance(&mut term, b"\x1b[<u");
        assert!(!term.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
    }

    #[test]
    fn plain_text_link_detection_trims_trailing_punctuation() {
        let links = find_plain_text_links("open https://example.com/path?q=1). next");
        assert_eq!(
            links,
            vec![(5, 33, "https://example.com/path?q=1".to_string())]
        );
    }

    #[test]
    fn cell_style_flags_preserve_terminal_text_styles() {
        let (bold, italic, underline, undercurl, strikethrough) = cell_style_flags(
            Flags::BOLD | Flags::ITALIC | Flags::UNDERLINE | Flags::UNDERCURL | Flags::STRIKEOUT,
        );

        assert!(bold);
        assert!(italic);
        assert!(underline);
        assert!(undercurl);
        assert!(strikethrough);
    }

    #[test]
    fn plain_text_link_annotation_preserves_osc8_links() {
        let mut row: Vec<CellSnapshot> = "go https://fallback.test"
            .chars()
            .map(|ch| CellSnapshot {
                ch,
                ..CellSnapshot::default()
            })
            .collect();
        row[3].link = Some("https://osc8.test".to_string());

        annotate_plain_text_links_for_row(&mut row);

        assert_eq!(row[3].link.as_deref(), Some("https://osc8.test"));
        assert_eq!(row[4].link.as_deref(), Some("https://fallback.test"));
    }

    #[test]
    fn snapshot_to_lines_preserves_cell_zerowidth_sequence() {
        let snapshot = ScreenSnapshot {
            cells: vec![vec![
                CellSnapshot {
                    ch: '\u{1f4c1}',
                    zerowidth: vec!['\u{fe0f}'],
                    ..CellSnapshot::default()
                },
                CellSnapshot {
                    ch: ' ',
                    ..CellSnapshot::default()
                },
            ]],
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            alt_screen: false,
        };

        assert_eq!(snapshot_to_lines(&snapshot), vec!["\u{1f4c1}\u{fe0f}"]);
    }

    #[test]
    fn font_fallbacks_keep_user_fonts_first_and_deduped() {
        let fallbacks = parse_font_fallbacks(&[
            "Custom Symbols".to_string(),
            "Apple Color Emoji".to_string(),
            "custom symbols".to_string(),
        ])
        .expect("fallbacks");
        let fallback_list = fallbacks.fallback_list();

        assert_eq!(
            fallback_list.get(0).map(String::as_str),
            Some("Custom Symbols")
        );
        assert_eq!(
            fallback_list.get(1).map(String::as_str),
            Some("Apple Color Emoji")
        );
        assert_eq!(
            fallback_list
                .iter()
                .filter(|font| font.eq_ignore_ascii_case("Custom Symbols"))
                .count(),
            1
        );
    }
}

pub(crate) fn run_self_check() -> Result<()> {
    let enter = gpui::Keystroke::parse("enter").context("parse enter")?;
    ensure!(
        encode_keystroke(&enter) == Some(vec![b'\r']),
        "enter keystroke encoding mismatch"
    );

    let alt_x = gpui::Keystroke::parse("alt-x").context("parse alt-x")?;
    ensure!(
        encode_keystroke(&alt_x) == Some(vec![0x1b, b'x']),
        "alt-x keystroke encoding mismatch"
    );

    let ctrl_c = gpui::Keystroke::parse("ctrl-c").context("parse ctrl-c")?;
    ensure!(
        encode_keystroke(&ctrl_c) == Some(vec![3]),
        "ctrl-c keystroke encoding mismatch"
    );

    let chinese = gpui::Keystroke {
        modifiers: gpui::Modifiers::none(),
        key: "x".to_string(),
        key_char: Some("你".to_string()),
    };
    ensure!(
        encode_keystroke(&chinese) == Some("你".as_bytes().to_vec()),
        "chinese ime keystroke encoding mismatch"
    );

    let ime_in_progress = gpui::Keystroke {
        modifiers: gpui::Modifiers::none(),
        key: "a".to_string(),
        key_char: None,
    };
    ensure!(
        encode_keystroke(&ime_in_progress).is_none(),
        "ime in-progress keystroke should not emit bytes"
    );

    let cmd_v = gpui::Keystroke {
        modifiers: gpui::Modifiers {
            platform: true,
            ..gpui::Modifiers::none()
        },
        key: "v".to_string(),
        key_char: Some("v".to_string()),
    };
    ensure!(
        encode_keystroke(&cmd_v).is_none(),
        "cmd-v should not be forwarded as raw character"
    );

    let default_colors = alacritty_terminal::term::color::Colors::default();
    ensure!(
        indexed_to_rgb(16, &default_colors) == (0, 0, 0),
        "indexed color 16 mismatch"
    );
    ensure!(
        indexed_to_rgb(231, &default_colors) == (255, 255, 255),
        "indexed color 231 mismatch"
    );

    let grid = compute_grid_size(
        gpui::size(gpui::px(1000.0), gpui::px(520.0)),
        gpui::px(8.0),
        line_height_for(DEFAULT_FONT_SIZE),
        false,
    );
    ensure!(
        grid.cols >= 80,
        "computed columns too small for 1000px viewport"
    );
    ensure!(
        grid.rows >= 20,
        "computed rows too small for 520px viewport"
    );

    println!(
        "self-check passed: keyboard/color/grid invariants OK (cols={}, rows={})",
        grid.cols, grid.rows
    );

    Ok(())
}
