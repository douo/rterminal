//! Splits the PTY byte stream into SIXEL / tmux-passthrough DCS payloads and
//! plain terminal bytes.
//!
//! The parser is deliberately transparent: anything that is not a DCS sequence
//! (CSI, OSC, plain text, …) is re-emitted byte-for-byte as [`SixelStreamAction::Bytes`]
//! so the downstream `alacritty_terminal` processor still sees an unmodified stream.

/// 单个未终止 DCS 能吃进来的字节上限。
///
/// 没有上限时 `printf '\x1bP'; cat 大文件` 会让整个会话的后续输出全部被吞进
/// `payload` + `raw` 两个 Vec（双份缓冲）：屏幕停止更新、内存无界增长，而且没有
/// 任何恢复手段。纯 ASCII 文本永远不含 0x9c，所以这不是理论问题。
///
/// 超限后我们放弃自己解析，把已经吃进来的字节原样交给下游的 alacritty VTE。
/// 那才是正统的 DCS 实现：它对未知 DCS 只是丢弃 put()，不会无界缓冲，
/// 而且如果流里后面真出现了 ST，它能自己退出 DCS 状态。
const MAX_DCS_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct SixelStreamParser {
    state: SixelStreamState,
    max_dcs_bytes: usize,
}

impl Default for SixelStreamParser {
    fn default() -> Self {
        Self {
            state: SixelStreamState::default(),
            max_dcs_bytes: MAX_DCS_BYTES,
        }
    }
}

/// CAN / SUB：ECMA-48 规定它们中止当前控制序列。
fn aborts_control_sequence(byte: u8) -> bool {
    byte == 0x18 || byte == 0x1a
}

/// 放弃这段 DCS：把已吃进来的原始字节按原顺序交回下游，回到 Ground。
///
/// 追加到 `output`（而不是单独 push 一个 action）是为了保持字节顺序——`output`
/// 就是普通文本的累积缓冲，之后会作为一个 `Bytes` action 被吐出去。
fn abandon_dcs(raw: &[u8], output: &mut Vec<u8>) -> SixelStreamState {
    output.extend_from_slice(raw);
    SixelStreamState::Ground
}

pub(crate) enum SixelStreamAction {
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
        /// 还差几个 UTF-8 连续字节没读完。裸 0x9c（C1 ST）只有在**不是**连续字节
        /// 时才是终止符（DSP-12）：SIXEL 载荷是纯 ASCII 不受影响，但含 UTF-8 的
        /// 未知 DCS / tmux passthrough 里，多字节字符的第二字节完全可能是 0x9c
        ///（如 U+011C 'Ĝ' = C4 9C），按终止符处理会把序列拦腰截断。
        utf8_continuations: u8,
    },
}

/// 该字节作为 UTF-8 首字节时后面跟几个连续字节（非首字节返回 0）。
fn utf8_continuation_len(byte: u8) -> u8 {
    match byte {
        0xc2..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf4 => 3,
        _ => 0,
    }
}

impl SixelStreamParser {
    pub(crate) fn advance(&mut self, bytes: &[u8]) -> Vec<SixelStreamAction> {
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
                if aborts_control_sequence(byte) || raw.len() > self.max_dcs_bytes {
                    abandon_dcs(&raw, output)
                } else if (0x40..=0x7e).contains(&byte) {
                    SixelStreamState::DcsData {
                        action: byte,
                        raw,
                        payload: Vec::new(),
                        pending_escape: false,
                        utf8_continuations: 0,
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
                mut utf8_continuations,
            } => {
                raw.push(byte);
                let inside_utf8_char = utf8_continuations > 0;
                if inside_utf8_char {
                    utf8_continuations -= 1;
                } else {
                    utf8_continuations = utf8_continuation_len(byte);
                }
                if aborts_control_sequence(byte) || raw.len() > self.max_dcs_bytes {
                    abandon_dcs(&raw, output)
                } else if byte == 0x9c && !inside_utf8_char {
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
                            utf8_continuations,
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
                            utf8_continuations,
                        }
                    }
                } else if byte == 0x1b {
                    pending_escape = true;
                    SixelStreamState::DcsData {
                        action,
                        raw,
                        payload,
                        pending_escape,
                        utf8_continuations,
                    }
                } else {
                    payload.push(byte);
                    SixelStreamState::DcsData {
                        action,
                        raw,
                        payload,
                        pending_escape,
                        utf8_continuations,
                    }
                }
            }
        };
    }

    #[cfg(test)]
    fn with_max_dcs_bytes(max_dcs_bytes: usize) -> Self {
        Self {
            state: SixelStreamState::default(),
            max_dcs_bytes,
        }
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

#[cfg(test)]
mod tests {
    use alacritty_terminal::sixel::decode_tmux_passthrough_sixel;

    use super::{SixelStreamAction, SixelStreamParser};

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

    /// 回归：未终止的 DCS 曾经会把整个会话的输出吞进内部缓冲，屏幕永久停止更新。
    /// 现在超过上限就放弃解析、原样吐回字节，并且解析器回到 Ground——
    /// 所以后面一个合法的 SIXEL 仍然能被识别。
    #[test]
    fn sixel_stream_parser_abandons_unterminated_dcs_past_limit() {
        let mut parser = SixelStreamParser::with_max_dcs_bytes(64);
        let flood = vec![b'A'; 512];

        let mut passed_through = Vec::new();
        for action in parser.advance(b"\x1bPq") {
            if let SixelStreamAction::Bytes(bytes) = action {
                passed_through.extend_from_slice(&bytes);
            }
        }
        for action in parser.advance(&flood) {
            match action {
                SixelStreamAction::Bytes(bytes) => passed_through.extend_from_slice(&bytes),
                SixelStreamAction::Sixel(_) => panic!("unterminated DCS must not yield a sixel"),
                _ => {}
            }
        }

        // 吃进去的字节没有被吞掉，而是原样交给了下游。
        assert!(passed_through.starts_with(b"\x1bPq"));
        assert_eq!(passed_through.len(), 3 + flood.len());

        // 解析器已回到 Ground：后续合法 sixel 仍可识别。
        let actions = parser.advance(b"\x1bPq~\x1b\\");
        assert!(actions.iter().any(|action| matches!(
            action,
            SixelStreamAction::Sixel(payload) if payload == b"~"
        )));
    }

    #[test]
    fn sixel_stream_parser_treats_can_and_sub_as_dcs_abort() {
        for abort in [0x18u8, 0x1a] {
            let mut parser = SixelStreamParser::default();
            let mut input = Vec::from(b"\x1bPq~".as_slice());
            input.push(abort);
            input.extend_from_slice(b"after");

            let actions = parser.advance(&input);

            assert!(
                !actions
                    .iter()
                    .any(|action| matches!(action, SixelStreamAction::Sixel(_))),
                "aborted DCS must not produce a sixel (abort byte {abort:#04x})"
            );

            let mut bytes = Vec::new();
            for action in &actions {
                if let SixelStreamAction::Bytes(chunk) = action {
                    bytes.extend_from_slice(chunk);
                }
            }
            // 中止字节本身也原样透传，让下游 VTE 自己处理中止语义。
            assert_eq!(bytes, input);
        }
    }

    /// 回归（DSP-12）：UTF-8 连续字节里的 0x9c 不是 ST。
    /// 'Ĝ'（U+011C）编码为 C4 9C——按裸终止符处理会把含 UTF-8 的 DCS 拦腰截断。
    #[test]
    fn sixel_stream_parser_ignores_9c_inside_utf8_sequences() {
        let mut parser = SixelStreamParser::default();
        let mut input = Vec::from(b"\x1bPx".as_slice());
        input.extend_from_slice("aĜb".as_bytes());
        input.extend_from_slice(b"\x1b\\");

        let actions = parser.advance(&input);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SixelStreamAction::UnknownDcs(raw) => {
                assert_eq!(raw, &input, "DCS 应完整走到 ESC \\ 终止，而不是被 9C 截断");
            }
            _ => panic!("expected a single unknown DCS action"),
        }

        // 真正的裸 0x9c（不在 UTF-8 序列内）仍然是有效终止符。
        let mut parser = SixelStreamParser::default();
        let actions = parser.advance(b"\x1bPq~\x9cafter");
        assert!(actions.iter().any(|action| matches!(
            action,
            SixelStreamAction::Sixel(payload) if payload == b"~"
        )));
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
}
