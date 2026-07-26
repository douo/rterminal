use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

pub(crate) fn ansi_bg_to_hsla(color: AnsiColor, colors: &Colors) -> Option<gpui::Hsla> {
    match color {
        AnsiColor::Named(NamedColor::Background) => None,
        other => Some(ansi_to_hsla(other, colors, Flags::empty(), false)),
    }
}

pub(crate) fn ansi_to_hsla(
    color: AnsiColor,
    colors: &Colors,
    flags: Flags,
    is_foreground: bool,
) -> gpui::Hsla {
    let resolved = ansi_to_rgb(color, colors, flags, is_foreground);
    gpui::rgb(((resolved.0 as u32) << 16) | ((resolved.1 as u32) << 8) | resolved.2 as u32).into()
}

fn ansi_to_rgb(
    color: AnsiColor,
    colors: &Colors,
    flags: Flags,
    is_foreground: bool,
) -> (u8, u8, u8) {
    match color {
        AnsiColor::Spec(rgb) => {
            let mut value = (rgb.r, rgb.g, rgb.b);
            // DIM 对 BOLD+DIM 同样生效（DSP-5）：alacritty 把两者并存按 DIM 处理。
            if is_foreground && flags.contains(Flags::DIM) {
                value = dim_rgb(value);
            }
            value
        }
        AnsiColor::Named(named) => {
            named_to_rgb(named_color_variant(named, flags, is_foreground), colors)
        }
        AnsiColor::Indexed(index) => indexed_fg_to_rgb(index, colors, flags, is_foreground),
    }
}

/// Indexed 前景色的 DIM 语义（DSP-6），对齐 alacritty：
/// 亮色 8–15 变暗回落到 0–7，标准色 0–7 落到专门的 Dim 变体，256 色其余不变。
/// 此前 `\e[38;5;1m\e[2m` 完全不变暗，与 `Spec`/`Named` 分支行为不一致。
fn indexed_fg_to_rgb(
    index: u8,
    colors: &Colors,
    flags: Flags,
    is_foreground: bool,
) -> (u8, u8, u8) {
    if !is_foreground || !flags.contains(Flags::DIM) {
        return indexed_to_rgb(index, colors);
    }

    match index {
        8..=15 => indexed_to_rgb(index - 8, colors),
        0..=7 => named_to_rgb(dim_variant_of_standard_index(index), colors),
        _ => indexed_to_rgb(index, colors),
    }
}

fn dim_variant_of_standard_index(index: u8) -> NamedColor {
    match index {
        0 => NamedColor::DimBlack,
        1 => NamedColor::DimRed,
        2 => NamedColor::DimGreen,
        3 => NamedColor::DimYellow,
        4 => NamedColor::DimBlue,
        5 => NamedColor::DimMagenta,
        6 => NamedColor::DimCyan,
        _ => NamedColor::DimWhite,
    }
}

fn named_color_variant(named: NamedColor, flags: Flags, is_foreground: bool) -> NamedColor {
    if !is_foreground {
        return named;
    }

    match (
        flags.contains(Flags::BOLD),
        flags.contains(Flags::DIM),
        named,
    ) {
        (true, false, NamedColor::Foreground) => NamedColor::BrightForeground,
        (true, false, value) => value.to_bright(),
        // DIM 压过 BOLD+DIM（DSP-5）：此前落进兜底分支返回原色。
        (_, true, value) => value.to_dim(),
        _ => named,
    }
}

fn named_to_rgb(named: NamedColor, colors: &Colors) -> (u8, u8, u8) {
    if let Some(rgb) = colors[named] {
        return (rgb.r, rgb.g, rgb.b);
    }

    match named {
        NamedColor::Black => (0x1d, 0x1f, 0x21),
        NamedColor::Red => (0xcc, 0x66, 0x66),
        NamedColor::Green => (0xb5, 0xbd, 0x68),
        NamedColor::Yellow => (0xf0, 0xc6, 0x74),
        NamedColor::Blue => (0x81, 0xa2, 0xbe),
        NamedColor::Magenta => (0xb2, 0x94, 0xbb),
        NamedColor::Cyan => (0x8a, 0xbe, 0xb7),
        NamedColor::White => (0xc5, 0xc8, 0xc6),
        NamedColor::BrightBlack => (0x66, 0x66, 0x66),
        NamedColor::BrightRed => (0xd5, 0x4e, 0x53),
        NamedColor::BrightGreen => (0xb9, 0xca, 0x4a),
        NamedColor::BrightYellow => (0xe7, 0xc5, 0x47),
        NamedColor::BrightBlue => (0x7a, 0xa6, 0xda),
        NamedColor::BrightMagenta => (0xc3, 0x97, 0xd8),
        NamedColor::BrightCyan => (0x70, 0xc0, 0xba),
        NamedColor::BrightWhite => (0xea, 0xea, 0xea),
        NamedColor::Foreground => (0xd7, 0xda, 0xe0),
        NamedColor::Background => (0x0f, 0x11, 0x15),
        NamedColor::Cursor => (0x3b, 0x82, 0xf6),
        NamedColor::DimBlack => dim_rgb((0x1d, 0x1f, 0x21)),
        NamedColor::DimRed => dim_rgb((0xcc, 0x66, 0x66)),
        NamedColor::DimGreen => dim_rgb((0xb5, 0xbd, 0x68)),
        NamedColor::DimYellow => dim_rgb((0xf0, 0xc6, 0x74)),
        NamedColor::DimBlue => dim_rgb((0x81, 0xa2, 0xbe)),
        NamedColor::DimMagenta => dim_rgb((0xb2, 0x94, 0xbb)),
        NamedColor::DimCyan => dim_rgb((0x8a, 0xbe, 0xb7)),
        NamedColor::DimWhite => dim_rgb((0xc5, 0xc8, 0xc6)),
        NamedColor::BrightForeground => (0xff, 0xff, 0xff),
        NamedColor::DimForeground => dim_rgb((0xd7, 0xda, 0xe0)),
    }
}

pub(crate) fn indexed_to_rgb(index: u8, colors: &Colors) -> (u8, u8, u8) {
    if let Some(rgb) = colors[index as usize] {
        return (rgb.r, rgb.g, rgb.b);
    }

    match index {
        0 => named_to_rgb(NamedColor::Black, &Default::default()),
        1 => named_to_rgb(NamedColor::Red, &Default::default()),
        2 => named_to_rgb(NamedColor::Green, &Default::default()),
        3 => named_to_rgb(NamedColor::Yellow, &Default::default()),
        4 => named_to_rgb(NamedColor::Blue, &Default::default()),
        5 => named_to_rgb(NamedColor::Magenta, &Default::default()),
        6 => named_to_rgb(NamedColor::Cyan, &Default::default()),
        7 => named_to_rgb(NamedColor::White, &Default::default()),
        8 => named_to_rgb(NamedColor::BrightBlack, &Default::default()),
        9 => named_to_rgb(NamedColor::BrightRed, &Default::default()),
        10 => named_to_rgb(NamedColor::BrightGreen, &Default::default()),
        11 => named_to_rgb(NamedColor::BrightYellow, &Default::default()),
        12 => named_to_rgb(NamedColor::BrightBlue, &Default::default()),
        13 => named_to_rgb(NamedColor::BrightMagenta, &Default::default()),
        14 => named_to_rgb(NamedColor::BrightCyan, &Default::default()),
        15 => named_to_rgb(NamedColor::BrightWhite, &Default::default()),
        16..=231 => {
            let index = index - 16;
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            (cube_value(r), cube_value(g), cube_value(b))
        }
        232..=255 => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    }
}

fn cube_value(step: u8) -> u8 {
    match step {
        0 => 0,
        n => 55 + n * 40,
    }
}

fn dim_rgb((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    (
        ((r as f32) * 0.66) as u8,
        ((g as f32) * 0.66) as u8,
        ((b as f32) * 0.66) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Rgb;

    fn hsla(color: AnsiColor, flags: Flags) -> gpui::Hsla {
        ansi_to_hsla(color, &Colors::default(), flags, true)
    }

    /// 回归（DSP-5）：BOLD+DIM 并存按 DIM 处理（对齐 alacritty），不再返回原色。
    #[test]
    fn bold_dim_named_color_resolves_to_dim_variant() {
        let bold_dim = hsla(AnsiColor::Named(NamedColor::Red), Flags::BOLD | Flags::DIM);
        let dim = hsla(AnsiColor::Named(NamedColor::Red), Flags::DIM);
        let plain = hsla(AnsiColor::Named(NamedColor::Red), Flags::empty());

        assert_eq!(bold_dim, dim);
        assert_ne!(bold_dim, plain);
    }

    /// 回归（DSP-5）：Spec 真彩色的 DIM 同样压过 BOLD。
    #[test]
    fn bold_dim_spec_color_is_dimmed() {
        let spec = AnsiColor::Spec(Rgb {
            r: 200,
            g: 100,
            b: 50,
        });
        assert_eq!(hsla(spec, Flags::BOLD | Flags::DIM), hsla(spec, Flags::DIM));
        assert_ne!(
            hsla(spec, Flags::BOLD | Flags::DIM),
            hsla(spec, Flags::empty())
        );
    }

    /// 回归（DSP-6）：Indexed 前景不再忽略 DIM——标准色落 Dim 变体、亮色回落基色。
    #[test]
    fn indexed_foreground_honors_dim() {
        let dim_red = hsla(AnsiColor::Indexed(1), Flags::DIM);
        let named_dim_red = hsla(AnsiColor::Named(NamedColor::DimRed), Flags::empty());
        assert_eq!(dim_red, named_dim_red);

        let dim_bright_red = hsla(AnsiColor::Indexed(9), Flags::DIM);
        let plain_red = hsla(AnsiColor::Indexed(1), Flags::empty());
        assert_eq!(dim_bright_red, plain_red);

        // 256 色区间不做变暗（对齐 alacritty）。
        assert_eq!(
            hsla(AnsiColor::Indexed(120), Flags::DIM),
            hsla(AnsiColor::Indexed(120), Flags::empty())
        );
    }

    /// 背景色不受 DIM/BOLD 变体影响。
    #[test]
    fn background_ignores_dim_and_bold() {
        let colors = Colors::default();
        let bg = ansi_bg_to_hsla(AnsiColor::Indexed(1), &colors);
        assert!(bg.is_some());
        assert_eq!(
            bg,
            Some(ansi_to_hsla(
                AnsiColor::Indexed(1),
                &colors,
                Flags::empty(),
                false
            ))
        );
    }
}
