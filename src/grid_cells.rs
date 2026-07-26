//! 网格 cell 语义的唯一实现点。
//!
//! 这里收纳的都是"曾经被复制过 2–4 份"的东西：cell 快照类型、网格 cell → 快照的
//! 转换（INVERSE/HIDDEN/宽字符/zerowidth/link 全套语义）、列宽推进、选区归一化与
//! 文本提取、宽字符的视觉列偏移。宽字符类 bug（COR-4 / COR-5 / DSP-4 一族）之所以
//! 能各自独立写错，就是因为这些语义散落多处——**任何新的列宽/选区逻辑必须写在这里，
//! 不要在渲染或输入模块里再抄一份。**

use std::collections::HashSet;

use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;

use crate::color::{ansi_bg_to_hsla, ansi_to_hsla};

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

/// 一行的空白填充 cell：只有前景色需要跟随主题，其余取默认。
pub(crate) fn blank_cell(default_fg: gpui::Hsla) -> CellSnapshot {
    CellSnapshot {
        fg: default_fg,
        ..Default::default()
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

/// 网格 cell → 快照 cell 的唯一转换点。
///
/// WIDE_CHAR_SPACER 返回 None：占位列不携带内容，由前一格的 `width_cols` 覆盖，
/// 调用方应保留该列的空白填充。
pub(crate) fn snapshot_cell(
    cell: &Cell,
    colors: &Colors,
    forced_double_width_chars: &HashSet<char>,
) -> Option<CellSnapshot> {
    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
        return None;
    }

    // INVERSE 交换的是**解析后**的颜色（DSP-7）：BOLD/DIM 变体属于"文本色"，
    // 要先施加到前景上、再随交换搬到背景去。此前先交换原始 AnsiColor 再解析，
    // 亮/暗变体被错误地施加到原背景色上——`\e[1;7;31m` 的反色块应为亮红背景，
    // 实际渲染成普通红。
    let resolved_fg = ansi_to_hsla(cell.fg, colors, cell.flags, true);
    let (fg, bg) = if cell.flags.contains(Flags::INVERSE) {
        (
            ansi_to_hsla(cell.bg, colors, Flags::empty(), false),
            Some(resolved_fg),
        )
    } else {
        (resolved_fg, ansi_bg_to_hsla(cell.bg, colors))
    };

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
    let expands_layout = !spans_next_col && forced_double_width_chars.contains(&ch);
    let width_cols = if spans_next_col || expands_layout {
        2
    } else {
        1
    };
    let (bold, italic, underline, undercurl, strikethrough) = cell_style_flags(cell.flags);

    Some(CellSnapshot {
        ch,
        zerowidth,
        fg,
        bg,
        link: cell.hyperlink().map(|link| link.uri().to_string()),
        bold,
        italic,
        underline,
        undercurl,
        strikethrough,
        width_cols,
        spans_next_col,
        expands_layout,
    })
}

/// 这个 cell 在网格里占几个逻辑列（宽字符为 2，其余为 1）。
///
/// 注意 `expands_layout`（强制双宽渲染）**不占**额外逻辑列——它只在视觉上加宽，
/// 网格坐标不变；视觉偏移由 [`visual_extra_cols_before`] 单独计算。
pub(crate) fn cell_advance_cols(cell: &CellSnapshot) -> usize {
    if cell.spans_next_col {
        usize::from(cell.width_cols.max(1))
    } else {
        1
    }
}

/// 提取整行文本，跳过宽字符的占位列。
pub(crate) fn row_text_without_wide_spacers(cells: &[CellSnapshot]) -> String {
    let mut text = String::new();
    let mut col = 0usize;
    while col < cells.len() {
        let cell = &cells[col];
        cell.push_text_to(&mut text);
        col = col.saturating_add(cell_advance_cols(cell));
    }
    text
}

/// 逻辑列 `logical_col` 之前累计的视觉额外列数（由 `expands_layout` 的强制双宽产生）。
/// 渲染与鼠标坐标换算必须用同一份实现，否则 hover 与点击会指向不同的 cell。
pub(crate) fn visual_extra_cols_before(row: &[CellSnapshot], logical_col: usize) -> f32 {
    let mut covered_until_col = 0usize;
    let mut extra_cols = 0f32;
    for (col_index, cell) in row.iter().enumerate() {
        if col_index >= logical_col {
            break;
        }
        if col_index < covered_until_col {
            continue;
        }
        if cell.expands_layout && cell.width_cols > 1 {
            extra_cols += f32::from(cell.width_cols - 1);
        }
        covered_until_col = col_index.saturating_add(cell_advance_cols(cell));
    }
    extra_cols
}

/// 视觉列 → 逻辑列（[`visual_extra_cols_before`] 的逆映射）。
///
/// 渲染把 cell 画在 `col_index + extra_visual_cols` 的视觉位置；鼠标命中必须走
/// 同一套换算，否则 `--double-width-chars` 下 hover 高亮（走渲染语义，正确）与
/// 点击命中（此前是纯线性除法）会指向不同的 cell（DSP-4）。
pub(crate) fn logical_col_for_visual_col(row: &[CellSnapshot], visual_col: f32) -> usize {
    let visual_col = visual_col.max(0.0);
    let mut covered_until_col = 0usize;
    let mut extra_visual_cols = 0f32;
    let mut last_content_col = 0usize;

    for (col_index, cell) in row.iter().enumerate() {
        if col_index < covered_until_col {
            continue;
        }
        let x_cols = col_index as f32 + extra_visual_cols;
        let visual_width = f32::from(cell.width_cols.max(1));
        if visual_col < x_cols + visual_width {
            return col_index;
        }

        last_content_col = col_index;
        covered_until_col = col_index.saturating_add(cell_advance_cols(cell));
        if cell.expands_layout && cell.width_cols > 1 {
            extra_visual_cols += f32::from(cell.width_cols - 1);
        }
    }

    last_content_col
}

pub(crate) fn normalize_selection_bounds(
    start: SelectionPoint,
    end: SelectionPoint,
) -> (SelectionPoint, SelectionPoint) {
    if (start.row, start.col) <= (end.row, end.col) {
        (start, end)
    } else {
        (end, start)
    }
}

/// 把选区端点吸附到宽字符的起始列：落在占位列上的坐标回退到前一格。
pub(crate) fn normalize_selection_col(cells: &[CellSnapshot], col: usize) -> usize {
    if cells.is_empty() {
        return 0;
    }

    let mut normalized = col.min(cells.len().saturating_sub(1));
    while normalized > 0 {
        let prev = normalized - 1;
        let prev_span = cell_advance_cols(&cells[prev]);
        if prev_span > 1 && prev.saturating_add(prev_span) > normalized {
            normalized = prev;
            continue;
        }
        break;
    }

    normalized
}

pub(crate) fn selection_contains_cell(
    start: SelectionPoint,
    end: SelectionPoint,
    row: usize,
    col: usize,
) -> bool {
    if row < start.row || row > end.row {
        return false;
    }

    if start.row == end.row {
        return row == start.row && col >= start.col && col <= end.col;
    }

    if row == start.row {
        return col >= start.col;
    }

    if row == end.row {
        return col <= end.col;
    }

    true
}

/// 提取选区文本：跳过宽字符占位列、行尾去空白、行间以 `\n` 连接。
pub(crate) fn extract_selection_text(
    lines: &[Vec<CellSnapshot>],
    start: SelectionPoint,
    end: SelectionPoint,
) -> String {
    let mut out = Vec::new();

    for row in start.row..=end.row {
        let Some(cells) = lines.get(row) else {
            break;
        };
        if cells.is_empty() {
            out.push(String::new());
            continue;
        }

        let line_start = if row == start.row {
            normalize_selection_col(cells, start.col)
        } else {
            0
        };
        let line_end = if row == end.row {
            normalize_selection_col(cells, end.col)
        } else {
            cells.len().saturating_sub(1)
        };
        if line_start >= cells.len() {
            out.push(String::new());
            continue;
        }

        let clamped_end = line_end.min(cells.len().saturating_sub(1));
        if line_start > clamped_end {
            out.push(String::new());
            continue;
        }

        let mut text = String::new();
        let mut col = line_start;
        while col <= clamped_end {
            let cell = &cells[col];
            cell.push_text_to(&mut text);
            col = col.saturating_add(cell_advance_cols(cell));
        }
        let trimmed_len = text.trim_end().len();
        text.truncate(trimmed_len);
        out.push(text);
    }

    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn narrow(ch: char) -> CellSnapshot {
        CellSnapshot {
            ch,
            ..Default::default()
        }
    }

    fn wide(ch: char) -> CellSnapshot {
        CellSnapshot {
            ch,
            width_cols: 2,
            spans_next_col: true,
            ..Default::default()
        }
    }

    fn spacer() -> CellSnapshot {
        CellSnapshot::default()
    }

    fn forced_double(ch: char) -> CellSnapshot {
        CellSnapshot {
            ch,
            width_cols: 2,
            spans_next_col: false,
            expands_layout: true,
            ..Default::default()
        }
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
    fn advance_cols_wide_char_covers_two_columns() {
        assert_eq!(cell_advance_cols(&narrow('a')), 1);
        assert_eq!(cell_advance_cols(&wide('中')), 2);
    }

    #[test]
    fn advance_cols_forced_double_width_stays_one_logical_column() {
        // expands_layout 只加宽视觉，不吃掉下一逻辑列。
        assert_eq!(cell_advance_cols(&forced_double('…')), 1);
    }

    #[test]
    fn row_text_skips_wide_spacers() {
        let row = vec![wide('中'), spacer(), narrow('a'), wide('文'), spacer()];
        assert_eq!(row_text_without_wide_spacers(&row), "中a文");
    }

    #[test]
    fn row_text_keeps_zerowidth_attached_to_base_char() {
        let mut cell = narrow('e');
        cell.zerowidth = vec!['\u{301}'];
        let row = vec![cell, narrow('x')];
        assert_eq!(row_text_without_wide_spacers(&row), "e\u{301}x");
    }

    #[test]
    fn visual_extra_cols_counts_only_forced_double_width() {
        // 真宽字符（占两逻辑列）不产生额外视觉列；强制双宽产生 1 列。
        let row = vec![
            wide('中'),
            spacer(),
            forced_double('…'),
            narrow('a'),
            forced_double('—'),
        ];
        assert_eq!(visual_extra_cols_before(&row, 0), 0.0);
        assert_eq!(visual_extra_cols_before(&row, 2), 0.0);
        assert_eq!(visual_extra_cols_before(&row, 3), 1.0);
        assert_eq!(visual_extra_cols_before(&row, 5), 2.0);
    }

    #[test]
    fn logical_col_inverts_visual_offsets() {
        // 布局：中(宽,0-1) spacer …(强制双宽,视觉2-3) a(视觉4) b(视觉5)
        let row = vec![
            wide('中'),
            spacer(),
            forced_double('…'),
            narrow('a'),
            narrow('b'),
        ];
        assert_eq!(logical_col_for_visual_col(&row, 0.0), 0);
        assert_eq!(logical_col_for_visual_col(&row, 1.9), 0);
        assert_eq!(logical_col_for_visual_col(&row, 2.0), 2);
        assert_eq!(logical_col_for_visual_col(&row, 3.9), 2);
        // 强制双宽把后续 cell 视觉右移 1 列：视觉 4 是逻辑 3。
        assert_eq!(logical_col_for_visual_col(&row, 4.0), 3);
        assert_eq!(logical_col_for_visual_col(&row, 5.0), 4);
        // 超出行尾落到最后一个内容列。
        assert_eq!(logical_col_for_visual_col(&row, 99.0), 4);
        assert_eq!(logical_col_for_visual_col(&row, -3.0), 0);
    }

    #[test]
    fn logical_col_is_linear_without_special_widths() {
        let row: Vec<CellSnapshot> = "abcdef".chars().map(narrow).collect();
        for col in 0..6 {
            assert_eq!(logical_col_for_visual_col(&row, col as f32 + 0.5), col);
        }
    }

    #[test]
    fn normalize_selection_col_snaps_to_wide_char_start() {
        let row = vec![narrow('a'), wide('中'), spacer(), narrow('b')];
        assert_eq!(normalize_selection_col(&row, 2), 1);
        assert_eq!(normalize_selection_col(&row, 1), 1);
        assert_eq!(normalize_selection_col(&row, 3), 3);
        assert_eq!(normalize_selection_col(&row, 99), 3);
    }

    #[test]
    fn extract_selection_text_returns_expected_multiline_slice() {
        let lines = vec![
            vec![narrow('h'), narrow('i'), narrow(' '), narrow('x')],
            vec![narrow('y'), narrow('o'), narrow(' '), narrow(' ')],
        ];
        let start = SelectionPoint { row: 0, col: 1 };
        let end = SelectionPoint { row: 1, col: 1 };
        assert_eq!(extract_selection_text(&lines, start, end), "i x\nyo");
    }

    #[test]
    fn extract_selection_text_skips_wide_char_spacers() {
        let lines = vec![vec![wide('中'), spacer(), narrow('a')]];
        let start = SelectionPoint { row: 0, col: 0 };
        let end = SelectionPoint { row: 0, col: 2 };
        assert_eq!(extract_selection_text(&lines, start, end), "中a");
    }

    #[test]
    fn extract_selection_text_preserves_cell_zerowidth_sequence() {
        let mut flag = wide('\u{1F1E8}');
        flag.zerowidth = vec!['\u{1F1F3}'];
        let lines = vec![vec![flag, spacer(), narrow('!')]];
        let start = SelectionPoint { row: 0, col: 0 };
        let end = SelectionPoint { row: 0, col: 2 };
        assert_eq!(
            extract_selection_text(&lines, start, end),
            "\u{1F1E8}\u{1F1F3}!"
        );
    }

    #[test]
    fn selection_bounds_are_normalized() {
        let a = SelectionPoint { row: 4, col: 10 };
        let b = SelectionPoint { row: 1, col: 2 };
        let (start, end) = normalize_selection_bounds(a, b);
        assert_eq!(start, b);
        assert_eq!(end, a);
    }

    #[test]
    fn selection_contains_cell_handles_multi_row_ranges() {
        let start = SelectionPoint { row: 1, col: 2 };
        let end = SelectionPoint { row: 3, col: 1 };
        assert!(!selection_contains_cell(start, end, 0, 5));
        assert!(!selection_contains_cell(start, end, 1, 1));
        assert!(selection_contains_cell(start, end, 1, 2));
        assert!(selection_contains_cell(start, end, 2, 0));
        assert!(selection_contains_cell(start, end, 3, 1));
        assert!(!selection_contains_cell(start, end, 3, 2));
    }

    #[test]
    fn snapshot_cell_wide_char_and_spacer_semantics() {
        use alacritty_terminal::term::cell::Cell;

        let colors = Colors::default();
        let forced = HashSet::new();

        let wide_cell = Cell {
            c: '中',
            flags: Flags::WIDE_CHAR,
            ..Cell::default()
        };
        let snap = snapshot_cell(&wide_cell, &colors, &forced).expect("wide char is content");
        assert_eq!(snap.ch, '中');
        assert_eq!(snap.width_cols, 2);
        assert!(snap.spans_next_col);
        assert!(!snap.expands_layout);

        let spacer_cell = Cell {
            flags: Flags::WIDE_CHAR_SPACER,
            ..Cell::default()
        };
        assert!(snapshot_cell(&spacer_cell, &colors, &forced).is_none());
    }

    #[test]
    fn snapshot_cell_hidden_blanks_content_and_inverse_swaps_colors() {
        use alacritty_terminal::term::cell::Cell;
        use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

        let colors = Colors::default();
        let forced = HashSet::new();

        let hidden = Cell {
            c: 'x',
            flags: Flags::HIDDEN,
            ..Cell::default()
        };
        let snap = snapshot_cell(&hidden, &colors, &forced).expect("hidden cell keeps its slot");
        assert_eq!(snap.ch, ' ');
        assert!(snap.zerowidth.is_empty());

        let inverse = Cell {
            c: 'y',
            fg: AnsiColor::Named(NamedColor::Red),
            bg: AnsiColor::Named(NamedColor::Background),
            flags: Flags::INVERSE,
            ..Cell::default()
        };
        let snap = snapshot_cell(&inverse, &colors, &forced).expect("inverse cell is content");
        // INVERSE 交换后：原背景成为前景、红色成为背景。
        assert_eq!(
            snap.bg,
            Some(ansi_to_hsla(
                AnsiColor::Named(NamedColor::Red),
                &colors,
                Flags::empty(),
                true
            ))
        );
    }

    /// 回归（DSP-7）：BOLD 的亮色变体先落到前景、再随 INVERSE 交换搬到背景——
    /// `\e[1;7;31m` 的反色块是**亮红**背景，不是普通红。
    #[test]
    fn inverse_swaps_resolved_colors_after_bold_variant() {
        use alacritty_terminal::term::cell::Cell;
        use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

        let colors = Colors::default();
        let forced = HashSet::new();

        let cell = Cell {
            c: 'z',
            fg: AnsiColor::Named(NamedColor::Red),
            bg: AnsiColor::Named(NamedColor::Background),
            flags: Flags::INVERSE | Flags::BOLD,
            ..Cell::default()
        };
        let snap = snapshot_cell(&cell, &colors, &forced).expect("content cell");

        let bright_red = ansi_to_hsla(
            AnsiColor::Named(NamedColor::Red),
            &colors,
            Flags::BOLD,
            true,
        );
        let plain_red = ansi_to_hsla(
            AnsiColor::Named(NamedColor::Red),
            &colors,
            Flags::empty(),
            true,
        );
        assert_eq!(snap.bg, Some(bright_red));
        assert_ne!(snap.bg, Some(plain_red));
        // 前景拿到的是解析后的默认背景色，不带任何变体。
        assert_eq!(
            snap.fg,
            ansi_to_hsla(
                AnsiColor::Named(NamedColor::Background),
                &colors,
                Flags::empty(),
                false
            )
        );
    }

    #[test]
    fn snapshot_cell_forced_double_width_expands_layout_only() {
        use alacritty_terminal::term::cell::Cell;

        let colors = Colors::default();
        let mut forced = HashSet::new();
        forced.insert('…');

        let cell = Cell {
            c: '…',
            ..Cell::default()
        };
        let snap = snapshot_cell(&cell, &colors, &forced).expect("content cell");
        assert!(snap.expands_layout);
        assert!(!snap.spans_next_col);
        assert_eq!(snap.width_cols, 2);
        assert_eq!(cell_advance_cols(&snap), 1);
    }
}
