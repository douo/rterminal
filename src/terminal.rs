use std::collections::HashSet;
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
    Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, FontFallbacks, Pixels, Subscription,
    Task, Window, px,
};
use parking_lot::Mutex;
use portable_pty::{Child, MasterPty, PtySize};
use serde::Serialize;
use serde_json::json;

use crate::cli::{AmbiguousWidth, CliOptions, Theme};
use crate::color::ansi_to_hsla;
use crate::color::indexed_to_rgb;
use crate::convenience::{
    ConvenienceState,
    input_method::{self, InputModeChangeListener},
};
use crate::debug_server::{
    DebugHttpConfig, DebugInputSink, DebugTabHandle, SharedDebugState, register_debug_http_tab,
};
use crate::font_fallback::font_fallback_families;
pub(crate) use crate::grid_cells::{CellSnapshot, ScreenSnapshot, SelectionPoint};
use crate::grid_cells::{
    blank_cell, cell_advance_cols, row_text_without_wide_spacers, snapshot_cell,
};
use crate::input::{ExternalFileDragState, FocusActivationMouseGuard};
use crate::input_log::InputLogger;
use crate::input_mirror::InputLineMirror;
use crate::keyboard::encode_keystroke;
use crate::pty::{PtySession, PtyWriteHandle};
use crate::render::{
    CUSTOM_TITLE_BAR_HEIGHT, STATUS_BAR_HEIGHT, TEXT_PADDING_X, TEXT_PADDING_Y, line_height_for,
    measure_cell_width,
};
use crate::sixel::{
    PtyPixelSize, SixelStreamAction, SixelStreamParser, TerminalImages, pty_pixel_size_for_grid,
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
/// AX 同步节拍。上限即外部 AX 覆写（语音工具改写命令行）的最大发现延迟；
/// 每拍只有几次 objc 调用，无内容变化时不触发重绘。
const AX_SYNC_INTERVAL: Duration = Duration::from_millis(100);

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
    pub(crate) writer: Option<PtyWriteHandle>,
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
                    writer.write(text.as_bytes());
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
                self.pending_events
                    .lock()
                    .push(PendingTerminalEvent::Erase {
                        region_top,
                        region_bottom,
                    });
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EnterLatencyProbe {
    pub(crate) id: u64,
    pub(crate) keydown_at: Instant,
    pub(crate) write_done_at: Option<Instant>,
    pub(crate) first_pty_at: Option<Instant>,
}

/// 光标滑动动画的全部状态（ARCH-3）。
///
/// 字段私有：动画目标只能经 [`Self::update_target`] 推进，别处不得直改
/// 中间量——那正是这组字段平铺在 `AgentTerminal` 顶层时发生过的事。
pub(crate) struct CursorSlide {
    enabled: bool,
    initialized: bool,
    visual_row: usize,
    anim_from_col: f32,
    anim_to_col: f32,
    anim_started_at: Option<Instant>,
}

impl CursorSlide {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            initialized: false,
            visual_row: 0,
            anim_from_col: 0.0,
            anim_to_col: 0.0,
            anim_started_at: None,
        }
    }

    /// 当前应绘制的 (row, col, 是否在动画中)；未初始化或未启用时用真实光标位置。
    pub(crate) fn visual_state(
        &self,
        fallback_row: usize,
        fallback_col: usize,
    ) -> (usize, f32, bool) {
        if !self.enabled || !self.initialized {
            return (fallback_row, fallback_col as f32, false);
        }

        let now = Instant::now();
        (
            self.visual_row,
            self.visual_col_at(now),
            self.animation_active_at(now),
        )
    }

    /// 渲染拖尾需要知道动画起点列。
    pub(crate) fn anim_from_col(&self) -> f32 {
        self.anim_from_col
    }

    fn snap_to(&mut self, row: usize, col: f32) {
        self.initialized = true;
        self.visual_row = row;
        self.anim_from_col = col;
        self.anim_to_col = col;
        self.anim_started_at = None;
    }

    fn update_target(&mut self, row: usize, col: usize) {
        let target_col = col as f32;
        let now = Instant::now();

        if !self.enabled || !self.initialized {
            self.snap_to(row, target_col);
            return;
        }

        let current_col = self.visual_col_at(now);
        let row_changed = self.visual_row != row;
        let large_delta = (target_col - current_col).abs() > CURSOR_SLIDE_MAX_COL_DELTA;
        if row_changed || large_delta {
            self.snap_to(row, target_col);
            return;
        }

        if (target_col - self.anim_to_col).abs() < f32::EPSILON {
            if !self.animation_active_at(now) {
                self.anim_from_col = target_col;
                self.anim_to_col = target_col;
                self.anim_started_at = None;
            }
            self.visual_row = row;
            return;
        }

        self.visual_row = row;
        self.anim_from_col = current_col;
        self.anim_to_col = target_col;
        self.anim_started_at = Some(now);
    }

    fn visual_col_at(&self, now: Instant) -> f32 {
        let Some(started_at) = self.anim_started_at else {
            return self.anim_to_col;
        };

        let elapsed = now.saturating_duration_since(started_at);
        let duration_ms = CURSOR_SLIDE_DURATION.as_millis().max(1) as f32;
        let progress = (elapsed.as_millis() as f32 / duration_ms).clamp(0.0, 1.0);
        let eased = progress * (2.0 - progress); // ease-out quad
        self.anim_from_col + (self.anim_to_col - self.anim_from_col) * eased
    }

    fn animation_active_at(&self, now: Instant) -> bool {
        let Some(started_at) = self.anim_started_at else {
            return false;
        };
        now.saturating_duration_since(started_at) < CURSOR_SLIDE_DURATION
            && (self.anim_to_col - self.anim_from_col).abs() >= f32::EPSILON
    }
}

/// 键入 → 回显的延迟探针与 PTY 摄取采样（ARCH-3 分组，纯诊断数据）。
#[derive(Debug)]
pub(crate) struct LatencyDiagnostics {
    pty_sample_started_at: Instant,
    pty_sample_bytes: usize,
    pty_sample_chunks: usize,
    last_pty_chunk_at: Option<Instant>,
    enter_latency_seq: u64,
    enter_latency_probe: Option<EnterLatencyProbe>,
}

impl LatencyDiagnostics {
    fn new() -> Self {
        Self {
            pty_sample_started_at: Instant::now(),
            pty_sample_bytes: 0,
            pty_sample_chunks: 0,
            last_pty_chunk_at: None,
            enter_latency_seq: 0,
            enter_latency_probe: None,
        }
    }
}

/// 鼠标选区状态机（ARCH-3）。字段私有：选区只能整体开始 / 推进 / 清空，
/// 不允许别处单改某一半端点把状态机改出中间态。
pub(crate) struct SelectionState {
    mode_active: bool,
    button: Option<gpui::MouseButton>,
    anchor: Option<SelectionPoint>,
    focus: Option<SelectionPoint>,
}

impl SelectionState {
    fn new() -> Self {
        Self {
            mode_active: false,
            button: None,
            anchor: None,
            focus: None,
        }
    }

    /// 是否处于拖拽选择中（按下未松开）。
    pub(crate) fn is_dragging(&self) -> bool {
        self.mode_active
    }

    pub(crate) fn drag_button(&self) -> Option<gpui::MouseButton> {
        self.button
    }

    pub(crate) fn start(&mut self, point: SelectionPoint, button: gpui::MouseButton) {
        self.mode_active = true;
        self.button = Some(button);
        self.anchor = Some(point);
        self.focus = Some(point);
    }

    /// 推进焦点端；返回是否真的移动了（用于决定要不要重绘）。
    pub(crate) fn update_focus(&mut self, point: SelectionPoint) -> bool {
        if self.focus == Some(point) {
            return false;
        }
        self.focus = Some(point);
        true
    }

    pub(crate) fn clear(&mut self) {
        self.mode_active = false;
        self.button = None;
        self.anchor = None;
        self.focus = None;
    }

    pub(crate) fn bounds(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        let anchor = self.anchor?;
        let focus = self.focus?;
        Some(crate::grid_cells::normalize_selection_bounds(anchor, focus))
    }
}

pub(crate) struct AgentTerminal {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) term: Term<TitleTrackingListener>,
    pub(crate) processor: Processor<StdSyncHandler>,
    sixel_parser: SixelStreamParser,
    pub(crate) grid_size: GridSize,
    /// Arc 包裹（PERF-1a）：render 每帧要把快照递进两层 canvas 闭包，
    /// 深拷贝一个 300×80 网格是每帧约 5 万次 cell 级堆克隆，Arc 克隆是 8 字节。
    pub(crate) snapshot: Arc<ScreenSnapshot>,
    pub(crate) images: TerminalImages,
    pub(crate) cursor_shape: CursorShape,
    pub(crate) force_vertical_cursor: bool,
    pub(crate) cursor_trail_enabled: bool,
    pub(crate) convenience_state: ConvenienceState,
    pub(crate) option_as_meta: bool,
    pub(crate) cursor_slide: CursorSlide,
    pub(crate) shell: String,
    pub(crate) terminal_title: Arc<Mutex<Option<String>>>,
    pub(crate) show_title_bar: bool,
    pub(crate) show_status_bar: bool,
    pub(crate) theme: Theme,
    pub(crate) font_family: String,
    pub(crate) font_fallbacks: Option<FontFallbacks>,
    /// CLI 传入的 fallback 原始表；后台字体扫描完成后用它重建完整 fallback 链。
    cli_font_fallbacks: Vec<String>,
    /// 首窗口用的是保守 fallback 表（PERF-2）；扫描完成后换装一次并复位。
    font_fallbacks_pending_rescan: bool,
    pub(crate) forced_double_width_chars: HashSet<char>,
    pub(crate) font_size: Pixels,
    pub(crate) cell_width: Pixels,
    pty_pixel_size: PtyPixelSize,
    pub(crate) master: Option<Arc<Mutex<Box<dyn MasterPty + Send>>>>,
    pub(crate) writer: Option<PtyWriteHandle>,
    pub(crate) child: Option<Arc<Mutex<Box<dyn Child + Send>>>>,
    pub(crate) input_mirror: InputLineMirror,
    pub(crate) ime_marked_text: Option<String>,
    pub(crate) input_trace: bool,
    pub(crate) input_logger: Option<InputLogger>,
    pub(crate) last_local_key_event_at: Option<Instant>,
    pub(crate) last_focus_in_at: Option<Instant>,
    latency: LatencyDiagnostics,
    pub(crate) mouse_scroll_accum_x: f32,
    pub(crate) mouse_scroll_accum_y: f32,
    pub(crate) last_mouse_report: Option<(usize, usize, u8)>,
    pub(crate) focus_activation_mouse: FocusActivationMouseGuard,
    pub(crate) external_file_drag: ExternalFileDragState,
    pub(crate) selection: SelectionState,
    pub(crate) canvas_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    pub(crate) paste_guard_prompt_open: bool,
    pub(crate) shell_exited: bool,
    pub(crate) debug: SharedDebugState,
    /// debug HTTP 是否真的在跑：为 false 时跳过每 batch 的整屏文本行重建（PERF-3）。
    debug_http_enabled: bool,
    /// 进程级 debug server 上本 tab 的会话句柄，Drop 即注销（SEC-4）。
    _debug_http_tab: Option<DebugTabHandle>,
    pending_term_events: Arc<Mutex<Vec<PendingTerminalEvent>>>,
    pub(crate) _window_bounds_sub: Option<Subscription>,
    pub(crate) _window_activation_sub: Option<Subscription>,
    pub(crate) _focus_in_sub: Option<Subscription>,
    pub(crate) _focus_out_sub: Option<Subscription>,
    pub(crate) input_method_watch_task: Option<Task<Result<()>>>,
    pub(crate) _pump_task: Task<Result<()>>,
    /// 主线程消费 debug 注入的任务（仅在 --debug-http 开启时存在）。
    pub(crate) _debug_input_task: Option<Task<Result<()>>>,
    /// AX 影子输入行与输入法状态的周期同步（ARCH-4：这些副作用不属于 render）。
    pub(crate) _ax_sync_task: Task<Result<()>>,
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
        // 首次调用时后台扫描大概率还没完成，先用保守表；周期任务里检测完成后换装。
        let font_fallbacks_pending_rescan = !crate::font_fallback::background_scan_complete();
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
                    // 起专职写线程，之后所有写入都是投递，不在 UI 线程上阻塞。
                    Some(PtyWriteHandle::spawn(session.writer)),
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

        // 默认不启动：这个接口能往 PTY 写任意字节，等于在用户 shell 里执行任意命令。
        let mut debug_input_rx = None;
        let mut debug_http_enabled = false;
        let mut debug_http_tab = None;
        if cli.debug_http {
            match DebugHttpConfig::new(cli.debug_http_token.clone(), cli.debug_http_allow_remote) {
                Some(config) => {
                    // token 必须让用户看得到，否则开了也用不了。
                    eprintln!(
                        "debug http enabled; authenticate with header \"X-Debug-Token: {}\"",
                        config.token()
                    );
                    // 注入走主线程（见 DebugInputSink），不让 HTTP 线程自己写 PTY。
                    let (sink, rx) = DebugInputSink::channel();
                    debug_input_rx = Some(rx);
                    debug_http_enabled = true;
                    // 句柄随本终端存活，Drop 即从进程级 server 注销会话（SEC-4）。
                    debug_http_tab =
                        Some(register_debug_http_tab(debug.clone(), Some(sink), config));
                }
                None => {
                    let message = "refusing to start debug server: could not read /dev/urandom \
                                   to generate an auth token (pass --debug-http-token to supply one)";
                    eprintln!("{message}");
                    debug.set_error(message);
                }
            }
        }
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
            snapshot: Arc::new(ScreenSnapshot::default()),
            images: TerminalImages::new(),
            cursor_shape: CursorShape::Block,
            force_vertical_cursor: cli.force_vertical_cursor,
            cursor_slide: CursorSlide::new(!cli.no_cursor_slide),
            cursor_trail_enabled: cli.cursor_trail,
            convenience_state: ConvenienceState::new(),
            option_as_meta: !cli.no_option_as_meta,
            shell,
            terminal_title: terminal_title.clone(),
            show_title_bar: options.show_title_bar,
            show_status_bar: cli.show_status_bar,
            theme: cli.theme,
            font_family: cli.font_family.clone(),
            font_fallbacks,
            cli_font_fallbacks: cli.font_fallbacks.clone(),
            font_fallbacks_pending_rescan,
            forced_double_width_chars,
            font_size,
            cell_width,
            pty_pixel_size,
            master,
            writer,
            child,
            input_mirror: InputLineMirror::new(),
            ime_marked_text: None,
            input_trace: is_input_trace_enabled(),
            input_logger,
            last_local_key_event_at: None,
            last_focus_in_at: None,
            latency: LatencyDiagnostics::new(),
            mouse_scroll_accum_x: 0.0,
            mouse_scroll_accum_y: 0.0,
            last_mouse_report: None,
            focus_activation_mouse: FocusActivationMouseGuard::default(),
            external_file_drag: ExternalFileDragState::default(),
            selection: SelectionState::new(),
            canvas_bounds: Arc::new(Mutex::new(None)),
            paste_guard_prompt_open: false,
            shell_exited: false,
            debug,
            debug_http_enabled,
            _debug_http_tab: debug_http_tab,
            pending_term_events,
            _window_bounds_sub: None,
            _window_activation_sub: None,
            _focus_in_sub: None,
            _focus_out_sub: None,
            input_method_watch_task: None,
            _pump_task: Task::ready(Ok(())),
            _debug_input_task: None,
            _ax_sync_task: Task::ready(Ok(())),
        };

        this.refresh_snapshot();
        this._window_bounds_sub = Some(cx.observe_window_bounds(window, |this, window, cx| {
            this.sync_grid_to_window(window);
            cx.notify();
        }));
        this._window_activation_sub = Some(cx.observe_window_activation(window, |_, _, cx| {
            cx.notify();
        }));
        this._focus_in_sub = Some(cx.on_focus(&this.focus_handle, window, |this, window, cx| {
            this.last_focus_in_at = Some(Instant::now());
            this.start_input_method_listener(cx);
            if this.term.mode().contains(TermMode::FOCUS_IN_OUT) {
                this.write_bytes(b"\x1b[I");
            }
            this.refresh_ime_context(window, cx);
        }));
        this._focus_out_sub =
            Some(
                cx.on_focus_out(&this.focus_handle, window, |this, _event, window, cx| {
                    this.stop_input_method_listener();
                    // 失焦时也清掉激活点击 guard：下一次获得焦点会带来新的 first_mouse，
                    // 留着旧的只会吞掉之后某次正常点击的 release。
                    this.focus_activation_mouse.clear();
                    this.flush_ime_context(window, cx);
                    if this.term.mode().contains(TermMode::FOCUS_IN_OUT) {
                        this.write_bytes(b"\x1b[O");
                    }
                }),
            );
        this.sync_grid_to_window(window);

        // ARCH-4：AX 同步是"轮询外部状态 + 回写模型"的双向副作用，此前放在
        // Render::render 里——渲染函数因此成了事件循环钩子，且外部 AX 写入要等到
        // 恰好有一帧渲染才被发现。改成独立的周期任务后 render 恢复只读，
        // AX 覆写的发现延迟也从"不确定（取决于是否有帧）"变成固定上限。
        this._ax_sync_task = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(AX_SYNC_INTERVAL).await;
                this.update_in(cx, |this, window, cx| {
                    this.sync_ax_and_input_state(window, cx);
                })?;
            }
        });

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
                    // shell 自己退出（用户输入 exit）而 tab 还开着时，也要收尸，
                    // 否则它就一直是僵尸。用 try_wait 而不是 wait：这里在 UI 线程上，
                    // 而"channel 关闭"并不百分百等于"进程已退出"（例如 reader 侧
                    // 因为读错误提前收摊）。
                    if let Some(child) = &this.child {
                        let _ = child.lock().try_wait();
                    }
                    cx.emit(TerminalExitedEvent);
                    cx.notify();
                });
                Ok(())
            });
        }

        if let Some(rx) = debug_input_rx {
            // debug 注入在主线程上应用，走的是和键盘输入**完全相同**的路径：
            // write_bytes + apply_terminal_bytes_to_input_line。此前 HTTP 线程绕过主线程
            // 直接写 PTY，注入的字节因此不更新影子输入行模型——而那个模型正是本项目
            // 通过 AX 暴露出去的"可信输入行"，外部工具会读到错的内容。
            this._debug_input_task = Some(cx.spawn(async move |this, cx| {
                while let Ok(bytes) = rx.recv().await {
                    this.update(cx, |this, cx| {
                        this.write_bytes(&bytes);
                        this.apply_terminal_bytes_to_input_line(&bytes);
                        cx.notify();
                    })?;
                }
                Ok(())
            }));
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
                    let alt = self.term.mode().contains(TermMode::ALT_SCREEN);
                    self.images
                        .scroll_region(region_top, region_bottom, delta, alt);
                }
                PendingTerminalEvent::Erase {
                    region_top,
                    region_bottom,
                } => {
                    let alt = self.term.mode().contains(TermMode::ALT_SCREEN);
                    self.images.erase_region(region_top, region_bottom, alt);
                }
            }
        }
    }

    /// 只结算影响图片版图的 Scroll/Erase 事件，Clipboard 类事件保持排队。
    ///
    /// 图片入库前（DSP-1）与 resize 后（DSP-3）必须立刻结算：`ingest_batch` 是逐
    /// chunk 处理、批末才统一 drain 的，如果不在入库前把**之前**积累的 Scroll 先
    /// 结掉，`seq 30; img2sixel foo.png` 合并进同一批时那些滚动增量会被错误地施加
    /// 到新图片上——图片额外上移 N 行甚至被判定滚出而丢弃。
    fn drain_pending_image_events(&mut self) {
        let alt = self.term.mode().contains(TermMode::ALT_SCREEN);
        let pending = {
            let mut guard = self.pending_term_events.lock();
            std::mem::take(&mut *guard)
        };
        let mut kept = Vec::new();
        for event in pending {
            match event {
                PendingTerminalEvent::Scroll {
                    region_top,
                    region_bottom,
                    delta,
                } => {
                    self.images
                        .scroll_region(region_top, region_bottom, delta, alt);
                }
                PendingTerminalEvent::Erase {
                    region_top,
                    region_bottom,
                } => {
                    self.images.erase_region(region_top, region_bottom, alt);
                }
                other => kept.push(other),
            }
        }
        if !kept.is_empty() {
            let mut guard = self.pending_term_events.lock();
            let arrived_meanwhile = std::mem::take(&mut *guard);
            *guard = kept;
            guard.extend(arrived_meanwhile);
        }
    }

    fn store_sixel_image(&mut self, image: SixelImage) {
        // DSP-1：先把入库前积累的 Scroll/Erase 结算掉，它们属于旧内容，
        // 不该作用到马上要入库的这张图片上。
        self.drain_pending_image_events();
        let alt_screen = self.term.mode().contains(TermMode::ALT_SCREEN);
        let (width, height, row, col) = (image.width, image.height, image.row, image.col);
        let cell_width = self.cell_width;
        let line_height = self.line_height();
        let stored = self.images.store(
            &image,
            cell_width,
            line_height,
            alt_screen,
            &mut self.processor,
            &mut self.term,
        );
        if stored {
            self.debug.set_note(Some(format!(
                "sixel image {}x{} at {},{}",
                width, height, row, col
            )));
        } else {
            self.debug.set_error(format!(
                "invalid sixel image {}x{} at {},{}",
                width, height, row, col
            ));
        }
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
        let default_fg = ansi_to_hsla(
            AnsiColor::Named(NamedColor::Foreground),
            content.colors,
            Flags::empty(),
            true,
        );
        let mut cells = vec![vec![blank_cell(default_fg); cols]; rows];

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

            if let Some(snapshot) = snapshot_cell(
                indexed.cell,
                content.colors,
                &self.forced_double_width_chars,
            ) {
                cells[row][col] = snapshot;
            }
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
        self.cursor_slide
            .update_target(cursor_row.min(rows.saturating_sub(1)), cursor_col);

        self.snapshot = Arc::new(ScreenSnapshot {
            cells,
            cursor_row: cursor_row.min(rows.saturating_sub(1)),
            cursor_col,
            cursor_visible: cursor.shape != CursorShape::Hidden,
            alt_screen,
        });

        // 整屏文本行重建只服务 /debug/state；没开 debug HTTP 就别每个 batch
        // 都做一遍 O(rows×cols) 的字符串拼装（PERF-3）。
        let screen_lines = if self.debug_http_enabled {
            snapshot_to_lines(&self.snapshot)
        } else {
            Vec::new()
        };
        self.debug.update_screen_snapshot(
            self.grid_size,
            self.snapshot.cursor_row,
            self.snapshot.cursor_col,
            screen_lines,
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

    pub(crate) fn refresh_convenience_state(&mut self, cx: &mut Context<Self>) {
        if self.convenience_state.refresh_input_mode() {
            cx.notify();
        }
    }

    /// 双向 AX 同步 + 输入法状态刷新（周期任务驱动，见 `_ax_sync_task`）。
    ///
    /// 读方向：发现外部工具经 AX 改写的输入行并回写模型；写方向：把影子输入行
    /// 发布给 AX。二者都可能改 `self`，所以它们必须待在 update 路径而不是 render 里。
    fn sync_ax_and_input_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model_line = self.input_mirror.line().to_string();
        let model_cursor_utf16 = self.input_mirror.cursor_utf16();
        let allow_ax_override = self.allow_ax_override();
        let (last_published_line, last_published_cursor) = {
            let (line, cursor) = self.input_mirror.last_published();
            (line.to_string(), cursor)
        };
        let sync_result = crate::macos_ax::sync_native_ax_input_view(
            window,
            &model_line,
            model_cursor_utf16,
            &last_published_line,
            last_published_cursor,
            allow_ax_override,
        );
        if let Some(state) = sync_result.override_from_ax
            && self.apply_external_ax_input_state(state)
        {
            cx.notify();
        }
        if sync_result.published_model {
            self.input_mirror
                .mark_published(model_line, model_cursor_utf16);
        }
        if self.convenience_state.refresh_input_mode_if_due() {
            cx.notify();
        }

        // PERF-2：首窗口用保守 fallback 表先开画，后台字体扫描完成后在这里
        // 换装一次完整表（同一节拍任务，无需额外调度）。
        if self.font_fallbacks_pending_rescan && crate::font_fallback::background_scan_complete() {
            self.font_fallbacks_pending_rescan = false;
            self.font_fallbacks = parse_font_fallbacks(&self.cli_font_fallbacks);
            self.sync_grid_to_window(window);
            cx.notify();
        }
    }

    pub(crate) fn restore_focus_and_ime_context(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.start_input_method_listener(cx);
        self.refresh_ime_context(window, cx);
    }

    fn refresh_ime_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 提交而不是丢弃：这条路径也覆盖"组合中点击终端"（on_mouse_down →
        // restore_focus_and_ime_context），而 macOS 的惯例（Terminal.app / TextEdit）
        // 是点击时提交已敲的内容。下面的 sync(…, true) 会 discardMarkedText，
        // 所以平台侧不会再重复提交一次。
        self.commit_ime_marked_text();
        input_method::sync_text_input_context_to_current_source(window, true);
        window.invalidate_character_coordinates();
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.refresh_convenience_state(cx);
            input_method::sync_text_input_context_to_current_source(window, false);
            window.invalidate_character_coordinates();
            cx.notify();
        });
    }

    /// 失焦时收尾 IME：先提交悬挂的组合文本，再让平台丢掉它的组合状态。
    ///
    /// 命名上曾经叫 discard，行为也真的是丢弃——但失焦丢掉用户已敲的一段拼音是数据
    /// 丢失，macOS 的文本控件在失焦时是提交的。
    fn flush_ime_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_ime_marked_text();
        input_method::discard_text_input_context_marked_text(window);
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn start_input_method_listener(&mut self, cx: &mut Context<Self>) {
        self.refresh_convenience_state(cx);

        if self
            .input_method_watch_task
            .as_ref()
            .is_some_and(|task| !task.is_ready())
        {
            return;
        }

        let Some(listener) = InputModeChangeListener::start() else {
            self.input_method_watch_task = None;
            return;
        };

        self.input_method_watch_task = Some(cx.spawn(async move |this, cx| {
            while listener.changed().await {
                this.update(cx, |this, cx| {
                    this.refresh_convenience_state(cx);
                })?;
            }
            Ok(())
        }));
    }

    fn stop_input_method_listener(&mut self) {
        self.input_method_watch_task.take();
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
            // DSP-3：resize 触发行 reflow / 行进出 scrollback 时会发 Scroll/Erase
            // 事件。立刻结算而不是等下一次 PTY ingest，否则图片与其预留空白区在
            // 下一次输出到来之前一直脱节。
            self.drain_pending_image_events();
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

        // 投递即返回：真正的 write_all/flush 在专职写线程上，UI 线程不会被
        // "前台程序不读 stdin" 拖死。
        if writer.write(bytes) {
            self.debug.record_bytes_to_pty(bytes.len(), false);
        } else {
            self.debug
                .set_error("failed to enqueue input for the PTY writer");
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
            let mut row = vec![blank_cell(default_fg); cols];

            for col in 0..cols {
                let cell = &grid[Line(line_index)][Column(col)];
                if let Some(snapshot) = snapshot_cell(cell, colors, &self.forced_double_width_chars)
                {
                    row[col] = snapshot;
                }
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
        self.cursor_slide
            .visual_state(self.snapshot.cursor_row, self.snapshot.cursor_col)
    }

    pub(crate) fn start_enter_latency_probe(&mut self, input_line: &str) -> u64 {
        if let Some(previous) = self.latency.enter_latency_probe.take() {
            self.log_enter_latency_event(
                "enter_latency_abandoned",
                previous.id,
                &previous,
                Instant::now(),
                json!({ "reason": "superseded_by_new_enter" }),
            );
        }

        self.latency.enter_latency_seq = self.latency.enter_latency_seq.saturating_add(1);
        let probe = EnterLatencyProbe {
            id: self.latency.enter_latency_seq,
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
        self.latency.enter_latency_probe = Some(probe);
        self.latency.enter_latency_seq
    }

    pub(crate) fn mark_enter_latency_write_done(&mut self, probe_id: u64, bytes_len: usize) {
        let now = Instant::now();
        let Some(probe) = self.latency.enter_latency_probe.as_mut() else {
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
        let Some(probe) = self.latency.enter_latency_probe.as_mut() else {
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
        let Some(probe) = self.latency.enter_latency_probe.take() else {
            return;
        };
        if probe.first_pty_at.is_none() {
            self.latency.enter_latency_probe = Some(probe);
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
        if let Some(last_chunk_at) = self.latency.last_pty_chunk_at {
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
        self.latency.last_pty_chunk_at = Some(now);

        self.latency.pty_sample_bytes += chunk_len;
        self.latency.pty_sample_chunks += 1;

        let window = now.saturating_duration_since(self.latency.pty_sample_started_at);
        if window < Duration::from_millis(500) {
            return;
        }

        let window_ms = window.as_millis().max(1);
        let bytes = self.latency.pty_sample_bytes as u128;
        let chunks = self.latency.pty_sample_chunks as u128;

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

        self.latency.pty_sample_started_at = now;
        self.latency.pty_sample_bytes = 0;
        self.latency.pty_sample_chunks = 0;
    }
}

impl EventEmitter<TerminalExitedEvent> for AgentTerminal {}

impl Drop for AgentTerminal {
    fn drop(&mut self) {
        if let Some(child) = &self.child {
            let mut child = child.lock();
            let _ = child.kill();
            // kill 之后必须 wait：否则子进程以僵尸形式挂在进程表里直到 app 退出。
            // 反复开关 tab 会一个个攒起来。
            let _ = child.wait();
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
    let mut col = 0usize;
    while col < row.len() {
        let cell = &row[col];
        cell.push_text_to(&mut text);
        col = col.saturating_add(cell_advance_cols(cell));
    }

    // 绝大多数行没有 URL：先做一次纯扫描，没有 scheme 就不再构建 char→col
    // 映射表（PERF-3，此前每行每 batch 都分配一个 Vec<usize>）。
    if find_next_url_scheme(&text).is_none() {
        return;
    }

    let mut char_cols = Vec::with_capacity(row.len());
    let mut col = 0usize;
    while col < row.len() {
        let cell = &row[col];
        // push_text_to 恰好贡献 1 + zerowidth.len() 个字符。
        char_cols.extend(std::iter::repeat_n(col, 1 + cell.zerowidth.len()));
        col = col.saturating_add(cell_advance_cols(cell));
    }

    for (start, end, uri) in find_plain_text_links(&text) {
        let end = end.min(char_cols.len());
        for &cell_col in &char_cols[start.min(end)..end] {
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

fn parse_font_fallbacks(raw: &[String]) -> Option<FontFallbacks> {
    let fallbacks = font_fallback_families(raw);
    if fallbacks.is_empty() {
        None
    } else {
        Some(FontFallbacks::from_fonts(fallbacks))
    }
}

fn parse_double_width_chars(raw: &[String]) -> HashSet<char> {
    raw.iter()
        .flat_map(|entry| entry.chars())
        .filter(|ch| !ch.is_whitespace())
        .collect()
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

    fn recording_writer() -> (PtyWriteHandle, Arc<Mutex<Vec<u8>>>) {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: crate::pty::SharedPtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter {
            bytes: bytes.clone(),
        })));
        (PtyWriteHandle::spawn(writer), bytes)
    }

    /// 写入现在是投递到专职线程，断言必须等它落地（见 ROB-2）。
    fn wait_for_written_bytes(recorded: &Arc<Mutex<Vec<u8>>>, expected: &[u8]) {
        for _ in 0..200 {
            if recorded.lock().as_slice() == expected {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!(
            "expected {:?} to be written to the PTY, got {:?}",
            expected,
            recorded.lock()
        );
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

        wait_for_written_bytes(&bytes, b"\x1b[1;4R");
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

        // 反向断言：给写线程一点时间，确认确实什么都没写出去。
        std::thread::sleep(std::time::Duration::from_millis(50));
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
        wait_for_written_bytes(&bytes, b"\x1b[?1u");
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
            fallback_list.first().map(String::as_str),
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
