//! 影子输入行模型（InputLineMirror）。
//!
//! 这是本项目经 macOS AX 对外暴露的"可信输入行"——产品论点的正确性根基
//! （见工作计划元原则 3）。字段私有：行内容与光标只能经这里的方法推进，
//! 任何绕过方法直改字段的路径都会让模型悄悄变脏且无处排查。

use crate::text_utils::{
    delete_next_word_utf16, delete_previous_word_utf16, delete_to_end_utf16, utf16_to_byte_index,
};

#[derive(Default)]
pub(crate) struct InputLineMirror {
    line: String,
    cursor_utf16: usize,
    /// 上一次发布给 AX 的 (line, cursor)，用于判断覆写/发布方向。
    last_published_line: String,
    last_published_cursor_utf16: usize,
}

impl InputLineMirror {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn line(&self) -> &str {
        &self.line
    }

    pub(crate) fn cursor_utf16(&self) -> usize {
        self.cursor_utf16
    }

    pub(crate) fn len_utf16(&self) -> usize {
        self.line.encode_utf16().count()
    }

    pub(crate) fn last_published(&self) -> (&str, usize) {
        (&self.last_published_line, self.last_published_cursor_utf16)
    }

    pub(crate) fn mark_published(&mut self, line: String, cursor_utf16: usize) {
        self.last_published_line = line;
        self.last_published_cursor_utf16 = cursor_utf16;
    }

    /// AX 覆写：外部工具（语音纠错等）整体替换行内容与光标。
    pub(crate) fn set_state(&mut self, line: String, cursor_utf16: usize) {
        self.line = line;
        self.cursor_utf16 = cursor_utf16;
        self.clamp_cursor();
    }

    /// 替换 UTF-16 区间（听写/自动纠错改已提交文本，COR-13）。
    /// 调用方需保证 `start < end && end <= len_utf16()`。
    pub(crate) fn replace_range_utf16(&mut self, start: usize, end: usize, text: &str) {
        let start_byte = utf16_to_byte_index(&self.line, start);
        let end_byte = utf16_to_byte_index(&self.line, end);
        self.line.replace_range(start_byte..end_byte, text);
        self.cursor_utf16 = start + text.encode_utf16().count();
    }

    pub(crate) fn clamp_cursor(&mut self) {
        self.cursor_utf16 = self.cursor_utf16.min(self.len_utf16());
    }

    pub(crate) fn insert_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        self.clamp_cursor();
        let cursor_byte = utf16_to_byte_index(&self.line, self.cursor_utf16);
        self.line.insert_str(cursor_byte, text);
        self.cursor_utf16 += text.encode_utf16().count();
    }

    pub(crate) fn backspace_char(&mut self) {
        self.clamp_cursor();
        if self.cursor_utf16 == 0 {
            return;
        }

        let cursor_byte = utf16_to_byte_index(&self.line, self.cursor_utf16);
        let Some((start_byte, removed)) = self.line[..cursor_byte].char_indices().last() else {
            return;
        };
        self.line.replace_range(start_byte..cursor_byte, "");
        self.cursor_utf16 = self.cursor_utf16.saturating_sub(removed.len_utf16());
    }

    pub(crate) fn delete_char_at_cursor(&mut self) {
        self.clamp_cursor();
        let cursor_byte = utf16_to_byte_index(&self.line, self.cursor_utf16);
        let Some(ch) = self.line[cursor_byte..].chars().next() else {
            return;
        };
        let end_byte = cursor_byte + ch.len_utf8();
        self.line.replace_range(cursor_byte..end_byte, "");
    }

    pub(crate) fn move_cursor_left(&mut self) {
        self.clamp_cursor();
        if self.cursor_utf16 == 0 {
            return;
        }

        let cursor_byte = utf16_to_byte_index(&self.line, self.cursor_utf16);
        if let Some((_, ch)) = self.line[..cursor_byte].char_indices().last() {
            self.cursor_utf16 = self.cursor_utf16.saturating_sub(ch.len_utf16());
        } else {
            self.cursor_utf16 = 0;
        }
    }

    pub(crate) fn move_cursor_right(&mut self) {
        self.clamp_cursor();
        let len = self.len_utf16();
        if self.cursor_utf16 >= len {
            return;
        }

        let cursor_byte = utf16_to_byte_index(&self.line, self.cursor_utf16);
        if let Some(ch) = self.line[cursor_byte..].chars().next() {
            self.cursor_utf16 += ch.len_utf16();
        } else {
            self.cursor_utf16 = len;
        }
    }

    pub(crate) fn move_cursor_to_start(&mut self) {
        self.cursor_utf16 = 0;
    }

    pub(crate) fn move_cursor_to_end(&mut self) {
        self.cursor_utf16 = self.len_utf16();
    }

    pub(crate) fn clear(&mut self) {
        self.line.clear();
        self.cursor_utf16 = 0;
    }

    pub(crate) fn delete_previous_word(&mut self) {
        delete_previous_word_utf16(&mut self.line, &mut self.cursor_utf16);
        self.clamp_cursor();
    }

    pub(crate) fn delete_next_word(&mut self) {
        delete_next_word_utf16(&mut self.line, &mut self.cursor_utf16);
        self.clamp_cursor();
    }

    pub(crate) fn delete_to_end(&mut self) {
        delete_to_end_utf16(&mut self.line, self.cursor_utf16);
        self.clamp_cursor();
    }

    /// 用写向 PTY 的字节反推行内容变化（启发式，见 COR-10 / 战略决策 1）。
    pub(crate) fn apply_terminal_bytes(&mut self, bytes: &[u8]) {
        match bytes {
            b"\r" => self.clear(),
            [0x7f] => self.backspace_char(),
            [0x08] => self.backspace_char(),        // Ctrl-H
            [0x01] => self.move_cursor_to_start(),  // Ctrl-A
            [0x05] => self.move_cursor_to_end(),    // Ctrl-E
            [0x02] => self.move_cursor_left(),      // Ctrl-B
            [0x06] => self.move_cursor_right(),     // Ctrl-F
            [0x03] => self.clear(),                 // Ctrl-C
            [0x04] => self.delete_char_at_cursor(), // Ctrl-D
            [0x0b] => self.delete_to_end(),         // Ctrl-K
            [0x17] => self.delete_previous_word(),  // Ctrl-W
            [0x15] => self.clear(),                 // Ctrl-U clears current line in common shells.
            b"\x1b[D" => self.move_cursor_left(),
            b"\x1b[C" => self.move_cursor_right(),
            b"\x1b[H" => self.move_cursor_to_start(),
            b"\x1b[F" => self.move_cursor_to_end(),
            b"\x1b[3~" => self.delete_char_at_cursor(),
            b"\x1b\x7f" => self.delete_previous_word(), // Alt-Backspace
            b"\x1bd" => self.delete_next_word(),        // Alt-D
            _ => {
                if bytes.first() == Some(&0x1b) {
                    return;
                }

                if let Ok(text) = std::str::from_utf8(bytes)
                    && !text.chars().any(char::is_control)
                {
                    self.insert_at_cursor(text);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::InputLineMirror;

    #[test]
    fn terminal_bytes_drive_line_and_cursor() {
        let mut mirror = InputLineMirror::new();
        mirror.apply_terminal_bytes(b"echo hi");
        assert_eq!(mirror.line(), "echo hi");
        assert_eq!(mirror.cursor_utf16(), 7);

        mirror.apply_terminal_bytes(b"\x1b[D");
        mirror.apply_terminal_bytes(&[0x7f]);
        assert_eq!(mirror.line(), "echo i");
        assert_eq!(mirror.cursor_utf16(), 5);

        mirror.apply_terminal_bytes(b"\r");
        assert_eq!(mirror.line(), "");
        assert_eq!(mirror.cursor_utf16(), 0);
    }

    #[test]
    fn utf16_editing_respects_char_boundaries() {
        let mut mirror = InputLineMirror::new();
        mirror.insert_at_cursor("a😀中");
        // 'a'=1 + 😀=2 + 中=1 → 4 单元。
        assert_eq!(mirror.cursor_utf16(), 4);

        mirror.move_cursor_left(); // 跨过 中
        mirror.move_cursor_left(); // 跨过 😀（2 单元）
        assert_eq!(mirror.cursor_utf16(), 1);

        mirror.delete_char_at_cursor(); // 删掉 😀
        assert_eq!(mirror.line(), "a中");
    }

    #[test]
    fn replace_range_moves_cursor_after_replacement() {
        let mut mirror = InputLineMirror::new();
        mirror.insert_at_cursor("hello world");
        mirror.replace_range_utf16(6, 11, "there");
        assert_eq!(mirror.line(), "hello there");
        assert_eq!(mirror.cursor_utf16(), 11);
    }
}
