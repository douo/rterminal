//! Splits the PTY byte stream into SIXEL / tmux-passthrough DCS payloads and
//! plain terminal bytes.
//!
//! The parser is deliberately transparent: anything that is not a DCS sequence
//! (CSI, OSC, plain text, …) is re-emitted byte-for-byte as [`SixelStreamAction::Bytes`]
//! so the downstream `alacritty_terminal` processor still sees an unmodified stream.

#[derive(Default)]
pub(crate) struct SixelStreamParser {
    state: SixelStreamState,
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
    },
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
