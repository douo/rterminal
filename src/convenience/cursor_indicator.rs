use gpui::{Hsla, rgba};

use crate::convenience::InputMode;

pub(crate) fn cursor_color_for_focus(base: Hsla, input_mode: InputMode, focused: bool) -> Hsla {
    if focused {
        cursor_color_for_input_mode(base, input_mode)
    } else {
        rgba(0x8a8f98aa).into()
    }
}

pub(crate) fn cursor_color_for_input_mode(base: Hsla, input_mode: InputMode) -> Hsla {
    match input_mode {
        InputMode::Latin | InputMode::Unknown => base,
        InputMode::Cjk => rgba(0x2ee6a6cc).into(),
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Hsla, rgba};

    use super::{cursor_color_for_focus, cursor_color_for_input_mode};
    use crate::convenience::InputMode;

    #[test]
    fn latin_cursor_uses_theme_color() {
        let base: Hsla = rgba(0xffea00a6).into();
        assert_eq!(cursor_color_for_input_mode(base, InputMode::Latin), base);
    }

    #[test]
    fn cjk_cursor_overrides_theme_color() {
        let base: Hsla = rgba(0xffea00a6).into();
        assert_ne!(cursor_color_for_input_mode(base, InputMode::Cjk), base);
    }

    #[test]
    fn unfocused_cursor_uses_gray() {
        let base: Hsla = rgba(0xffea00a6).into();
        assert_eq!(
            cursor_color_for_focus(base, InputMode::Cjk, false),
            rgba(0x8a8f98aa).into()
        );
    }
}
