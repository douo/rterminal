use alacritty_terminal::term::TermMode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KittyKeyEventType {
    Press,
    Repeat,
    Release,
}

pub(crate) fn encode_keystroke(keystroke: &gpui::Keystroke) -> Option<Vec<u8>> {
    // Reserve platform/function chords for app-level shortcuts such as paste.
    if keystroke.modifiers.platform || keystroke.modifiers.function {
        return None;
    }

    if let Some(bytes) = encode_special_keystroke(keystroke) {
        return Some(bytes);
    }

    encode_printable_keystroke(keystroke)
}

pub(crate) fn encode_keystroke_with_mode(
    keystroke: &gpui::Keystroke,
    mode: TermMode,
    event_type: KittyKeyEventType,
) -> Option<Vec<u8>> {
    // Reserve platform/function chords for app-level shortcuts such as paste.
    if keystroke.modifiers.platform || keystroke.modifiers.function {
        return None;
    }

    if mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
        && let Some(bytes) = encode_kitty_keystroke(keystroke, mode, event_type)
    {
        return Some(bytes);
    }

    match event_type {
        KittyKeyEventType::Release => None,
        KittyKeyEventType::Press | KittyKeyEventType::Repeat => encode_keystroke(keystroke),
    }
}

fn encode_special_keystroke(keystroke: &gpui::Keystroke) -> Option<Vec<u8>> {
    let key = keystroke.key.as_str();
    let modifiers = keystroke.modifiers;
    let has_modifiers = modifiers.shift || modifiers.alt || modifiers.control;

    if !has_modifiers {
        return match key {
            "space" => Some(vec![b' ']),
            "enter" => Some(vec![b'\r']),
            "tab" => Some(vec![b'\t']),
            "backspace" => Some(vec![0x7f]),
            "escape" => Some(vec![0x1b]),
            "left" => Some(b"\x1b[D".to_vec()),
            "right" => Some(b"\x1b[C".to_vec()),
            "up" => Some(b"\x1b[A".to_vec()),
            "down" => Some(b"\x1b[B".to_vec()),
            "home" => Some(b"\x1b[H".to_vec()),
            "end" => Some(b"\x1b[F".to_vec()),
            "insert" => Some(b"\x1b[2~".to_vec()),
            "delete" => Some(b"\x1b[3~".to_vec()),
            "pageup" => Some(b"\x1b[5~".to_vec()),
            "pagedown" => Some(b"\x1b[6~".to_vec()),
            "f1" => Some(b"\x1bOP".to_vec()),
            "f2" => Some(b"\x1bOQ".to_vec()),
            "f3" => Some(b"\x1bOR".to_vec()),
            "f4" => Some(b"\x1bOS".to_vec()),
            "f5" => Some(b"\x1b[15~".to_vec()),
            "f6" => Some(b"\x1b[17~".to_vec()),
            "f7" => Some(b"\x1b[18~".to_vec()),
            "f8" => Some(b"\x1b[19~".to_vec()),
            "f9" => Some(b"\x1b[20~".to_vec()),
            "f10" => Some(b"\x1b[21~".to_vec()),
            "f11" => Some(b"\x1b[23~".to_vec()),
            "f12" => Some(b"\x1b[24~".to_vec()),
            _ => None,
        };
    }

    match (key, modifiers.shift, modifiers.alt, modifiers.control) {
        ("tab", true, false, false) => return Some(b"\x1b[Z".to_vec()),
        ("enter", true, false, false) => return Some(vec![b'\n']),
        ("enter", false, true, false) => return Some(vec![0x1b, b'\r']),
        ("backspace", false, false, true) => return Some(vec![0x08]),
        ("backspace", false, true, false) => return Some(vec![0x1b, 0x7f]),
        ("space", false, false, true) => return Some(vec![0x00]),
        _ => {}
    }

    if let Some(bytes) = encode_modified_special_key(key, modifier_code(keystroke)) {
        return Some(bytes);
    }

    // On macOS with Option-as-Meta enabled, the system may still surface
    // key_char as a transformed glyph (e.g. Option+W -> ∑). For terminal
    // Meta bindings we want the physical printable key instead.
    if modifiers.alt && !modifiers.control {
        if key.chars().count() == 1
            && key.is_ascii()
            && keystroke
                .key_char
                .as_ref()
                .is_some_and(|value| !value.is_empty() && !value.is_ascii())
        {
            let mut bytes = vec![0x1b];
            bytes.extend_from_slice(key.as_bytes());
            return Some(bytes);
        }

        if let Some(key_char) = keystroke
            .key_char
            .as_ref()
            .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        {
            let mut bytes = vec![0x1b];
            bytes.extend_from_slice(key_char.as_bytes());
            return Some(bytes);
        }

        if key.chars().count() == 1 {
            let mut bytes = vec![0x1b];
            bytes.extend_from_slice(key.as_bytes());
            return Some(bytes);
        }
    }

    None
}

fn encode_kitty_keystroke(
    keystroke: &gpui::Keystroke,
    mode: TermMode,
    event_type: KittyKeyEventType,
) -> Option<Vec<u8>> {
    if event_type != KittyKeyEventType::Press && !mode.contains(TermMode::REPORT_EVENT_TYPES) {
        return None;
    }

    if keystroke.is_ime_in_progress() {
        return None;
    }

    let key = keystroke.key.as_str();
    let report_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let disambiguate = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    let report_events = mode.contains(TermMode::REPORT_EVENT_TYPES);

    if report_all {
        if let Some(bytes) = encode_kitty_csi_u(keystroke, mode, event_type) {
            return Some(bytes);
        }
        return encode_kitty_functional_key(
            key,
            modifier_code(keystroke),
            report_events,
            event_type,
        );
    }

    if report_events
        && event_type != KittyKeyEventType::Press
        && let Some(bytes) =
            encode_kitty_functional_key(key, modifier_code(keystroke), true, event_type)
    {
        return Some(bytes);
    }

    if disambiguate && should_disambiguate_as_csi_u(keystroke) {
        return encode_kitty_csi_u(keystroke, mode, event_type);
    }

    if report_events
        && event_type == KittyKeyEventType::Press
        && let Some(bytes) =
            encode_kitty_functional_key(key, modifier_code(keystroke), true, event_type)
    {
        return Some(bytes);
    }

    None
}

fn should_disambiguate_as_csi_u(keystroke: &gpui::Keystroke) -> bool {
    let key = keystroke.key.as_str();
    let modifiers = keystroke.modifiers;

    if key == "escape" {
        return true;
    }

    // Shift alone is deliberately excluded: Shift+Tab has an unambiguous legacy
    // encoding (CSI Z), so disambiguate mode has no reason to promote it to CSI-u.
    if matches!(key, "tab" | "enter" | "backspace") {
        return modifiers.control || modifiers.alt;
    }

    (modifiers.alt || modifiers.control) && kitty_key_code(keystroke).is_some()
}

fn encode_kitty_csi_u(
    keystroke: &gpui::Keystroke,
    mode: TermMode,
    event_type: KittyKeyEventType,
) -> Option<Vec<u8>> {
    let key_code = kitty_key_code(keystroke)?;
    let first_field = kitty_key_code_field(keystroke, key_code, mode);
    let modifier_field = kitty_modifier_field(modifier_code(keystroke), mode, event_type);
    let text_field = kitty_associated_text_field(keystroke, mode);

    let mut seq = format!("\x1b[{first_field};{modifier_field}");
    if let Some(text_field) = text_field {
        seq.push(';');
        seq.push_str(&text_field);
    }
    seq.push('u');
    Some(seq.into_bytes())
}

fn kitty_key_code(keystroke: &gpui::Keystroke) -> Option<u32> {
    match keystroke.key.as_str() {
        "escape" => return Some(27),
        "enter" => return Some(13),
        "tab" => return Some(9),
        "backspace" => return Some(127),
        "space" => return Some(32),
        "shift" => return Some(57441),
        "control" | "ctrl" => return Some(57442),
        "alt" | "option" => return Some(57443),
        "super" | "cmd" | "command" => return Some(57444),
        _ => {}
    }

    let mut key_chars = keystroke.key.chars();
    if let (Some(ch), None) = (key_chars.next(), key_chars.next()) {
        return Some(normalize_key_code_char(ch) as u32);
    }

    let key_char = keystroke.key_char.as_deref()?;
    let mut chars = key_char.chars();
    let ch = chars.next()?;
    if chars.next().is_none() && !ch.is_control() {
        return Some(normalize_key_code_char(ch) as u32);
    }

    None
}

fn normalize_key_code_char(ch: char) -> char {
    if ch.is_ascii_alphabetic() {
        ch.to_ascii_lowercase()
    } else {
        ch
    }
}

fn kitty_key_code_field(keystroke: &gpui::Keystroke, key_code: u32, mode: TermMode) -> String {
    if !mode.contains(TermMode::REPORT_ALTERNATE_KEYS) || !keystroke.modifiers.shift {
        return key_code.to_string();
    }

    let Some(shifted) = shifted_single_codepoint(keystroke) else {
        return key_code.to_string();
    };

    if shifted == key_code {
        key_code.to_string()
    } else {
        format!("{key_code}:{shifted}")
    }
}

fn shifted_single_codepoint(keystroke: &gpui::Keystroke) -> Option<u32> {
    let value = keystroke.key_char.as_deref()?;
    let mut chars = value.chars();
    let ch = chars.next()?;
    if chars.next().is_none() && !ch.is_control() {
        Some(ch as u32)
    } else {
        None
    }
}

fn kitty_modifier_field(
    modifier_code: u8,
    mode: TermMode,
    event_type: KittyKeyEventType,
) -> String {
    if mode.contains(TermMode::REPORT_EVENT_TYPES) {
        format!("{modifier_code}:{}", kitty_event_type_code(event_type))
    } else {
        modifier_code.to_string()
    }
}

fn kitty_event_type_code(event_type: KittyKeyEventType) -> u8 {
    match event_type {
        KittyKeyEventType::Press => 1,
        KittyKeyEventType::Repeat => 2,
        KittyKeyEventType::Release => 3,
    }
}

fn kitty_associated_text_field(keystroke: &gpui::Keystroke, mode: TermMode) -> Option<String> {
    if !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)
        || !mode.contains(TermMode::REPORT_ASSOCIATED_TEXT)
    {
        return None;
    }

    let text = keystroke.key_char.as_deref()?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }

    Some(
        text.chars()
            .map(|ch| (ch as u32).to_string())
            .collect::<Vec<_>>()
            .join(":"),
    )
}

fn encode_kitty_functional_key(
    key: &str,
    modifier_code: u8,
    report_events: bool,
    event_type: KittyKeyEventType,
) -> Option<Vec<u8>> {
    let event_suffix = if report_events {
        format!(":{}", kitty_event_type_code(event_type))
    } else {
        String::new()
    };

    let seq = match key {
        "up" => format!("\x1b[1;{modifier_code}{event_suffix}A"),
        "down" => format!("\x1b[1;{modifier_code}{event_suffix}B"),
        "right" => format!("\x1b[1;{modifier_code}{event_suffix}C"),
        "left" => format!("\x1b[1;{modifier_code}{event_suffix}D"),
        "home" => format!("\x1b[1;{modifier_code}{event_suffix}H"),
        "end" => format!("\x1b[1;{modifier_code}{event_suffix}F"),
        "insert" => format!("\x1b[2;{modifier_code}{event_suffix}~"),
        "delete" => format!("\x1b[3;{modifier_code}{event_suffix}~"),
        "pageup" => format!("\x1b[5;{modifier_code}{event_suffix}~"),
        "pagedown" => format!("\x1b[6;{modifier_code}{event_suffix}~"),
        "f1" => format!("\x1b[1;{modifier_code}{event_suffix}P"),
        "f2" => format!("\x1b[1;{modifier_code}{event_suffix}Q"),
        "f3" => format!("\x1b[13;{modifier_code}{event_suffix}~"),
        "f4" => format!("\x1b[1;{modifier_code}{event_suffix}S"),
        "f5" => format!("\x1b[15;{modifier_code}{event_suffix}~"),
        "f6" => format!("\x1b[17;{modifier_code}{event_suffix}~"),
        "f7" => format!("\x1b[18;{modifier_code}{event_suffix}~"),
        "f8" => format!("\x1b[19;{modifier_code}{event_suffix}~"),
        "f9" => format!("\x1b[20;{modifier_code}{event_suffix}~"),
        "f10" => format!("\x1b[21;{modifier_code}{event_suffix}~"),
        "f11" => format!("\x1b[23;{modifier_code}{event_suffix}~"),
        "f12" => format!("\x1b[24;{modifier_code}{event_suffix}~"),
        _ => return None,
    };

    Some(seq.into_bytes())
}

fn encode_modified_special_key(key: &str, modifier_code: u8) -> Option<Vec<u8>> {
    let seq = match key {
        "up" => format!("\x1b[1;{modifier_code}A"),
        "down" => format!("\x1b[1;{modifier_code}B"),
        "right" => format!("\x1b[1;{modifier_code}C"),
        "left" => format!("\x1b[1;{modifier_code}D"),
        "home" => format!("\x1b[1;{modifier_code}H"),
        "end" => format!("\x1b[1;{modifier_code}F"),
        "insert" => format!("\x1b[2;{modifier_code}~"),
        "delete" => format!("\x1b[3;{modifier_code}~"),
        "pageup" => format!("\x1b[5;{modifier_code}~"),
        "pagedown" => format!("\x1b[6;{modifier_code}~"),
        "f1" => format!("\x1b[1;{modifier_code}P"),
        "f2" => format!("\x1b[1;{modifier_code}Q"),
        "f3" => format!("\x1b[1;{modifier_code}R"),
        "f4" => format!("\x1b[1;{modifier_code}S"),
        "f5" => format!("\x1b[15;{modifier_code}~"),
        "f6" => format!("\x1b[17;{modifier_code}~"),
        "f7" => format!("\x1b[18;{modifier_code}~"),
        "f8" => format!("\x1b[19;{modifier_code}~"),
        "f9" => format!("\x1b[20;{modifier_code}~"),
        "f10" => format!("\x1b[21;{modifier_code}~"),
        "f11" => format!("\x1b[23;{modifier_code}~"),
        "f12" => format!("\x1b[24;{modifier_code}~"),
        _ => return None,
    };

    Some(seq.into_bytes())
}

fn modifier_code(keystroke: &gpui::Keystroke) -> u8 {
    let mut code = 0u8;
    if keystroke.modifiers.shift {
        code |= 1;
    }
    if keystroke.modifiers.alt {
        code |= 1 << 1;
    }
    if keystroke.modifiers.control {
        code |= 1 << 2;
    }
    code + 1
}

fn encode_printable_keystroke(keystroke: &gpui::Keystroke) -> Option<Vec<u8>> {
    let key = keystroke.key.as_str();
    let ctrl = keystroke.modifiers.control;

    // IME composition in progress (e.g. pinyin typing) should not leak intermediate ASCII
    // keystrokes into PTY before key_char is committed.
    if keystroke.is_ime_in_progress() {
        return None;
    }

    if ctrl {
        if key.len() == 1 && key.is_ascii() {
            let mut ch = key.as_bytes()[0];
            ch = ch.to_ascii_lowercase() & 0x1f;
            return Some(vec![ch]);
        }
        return None;
    }

    if let Some(key_char) = keystroke
        .key_char
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        return Some(key_char.as_bytes().to_vec());
    }

    if key.chars().count() == 1 {
        return Some(key.as_bytes().to_vec());
    }

    None
}

pub(crate) fn should_defer_to_text_input(
    keystroke: &gpui::Keystroke,
    option_as_meta: bool,
) -> bool {
    // On macOS, route printable text keys through NSTextInputClient callbacks
    // (insertText / setMarkedText) to preserve IME and accessibility behavior.
    if !cfg!(target_os = "macos") {
        return false;
    }

    let modifiers = keystroke.modifiers;
    if modifiers.control || modifiers.platform || modifiers.function {
        return false;
    }
    // When option_as_meta is true, Alt+key is handled by encode_keystroke as
    // ESC+key.  When false, let macOS text input produce native characters
    // (e.g. Option+D → ∂).
    if modifiers.alt && option_as_meta {
        return false;
    }

    if is_terminal_control_key_name(keystroke.key.as_str()) {
        return false;
    }

    keystroke
        .key_char
        .as_ref()
        .is_some_and(|ch| !ch.is_empty() && !ch.chars().any(|c| c.is_control()))
}

fn is_terminal_control_key_name(key: &str) -> bool {
    matches!(
        key,
        "enter"
            | "tab"
            | "backspace"
            | "escape"
            | "left"
            | "right"
            | "up"
            | "down"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "delete"
            | "insert"
    )
}

pub(crate) fn is_paste_shortcut(keystroke: &gpui::Keystroke) -> bool {
    let is_v = keystroke.key.eq_ignore_ascii_case("v");
    let modifiers = keystroke.modifiers;
    let mac_paste = modifiers.platform && !modifiers.control && is_v;
    let ctrl_shift_paste = modifiers.control && modifiers.shift && is_v;
    mac_paste || ctrl_shift_paste
}

pub(crate) fn is_select_all_shortcut(keystroke: &gpui::Keystroke) -> bool {
    let is_a = keystroke.key.eq_ignore_ascii_case("a");
    let modifiers = keystroke.modifiers;
    cfg!(target_os = "macos")
        && modifiers.platform
        && !modifiers.control
        && !modifiers.alt
        && !modifiers.shift
        && !modifiers.function
        && is_a
}

pub(crate) fn is_zoom_in_shortcut(keystroke: &gpui::Keystroke) -> bool {
    if !is_platform_shortcut(keystroke.modifiers) {
        return false;
    }

    if key_matches(&keystroke.key, &["=", "equal", "plus", "+"]) {
        return true;
    }

    keystroke
        .key_char
        .as_ref()
        .is_some_and(|ch| ch == "=" || ch == "+")
}

pub(crate) fn is_zoom_out_shortcut(keystroke: &gpui::Keystroke) -> bool {
    if !is_platform_shortcut(keystroke.modifiers) {
        return false;
    }

    if key_matches(&keystroke.key, &["-", "minus", "_"]) {
        return true;
    }

    keystroke
        .key_char
        .as_ref()
        .is_some_and(|ch| ch == "-" || ch == "_")
}

fn is_platform_shortcut(modifiers: gpui::Modifiers) -> bool {
    modifiers.platform && !modifiers.control && !modifiers.alt && !modifiers.function
}

fn key_matches(key: &str, accepted: &[&str]) -> bool {
    accepted
        .iter()
        .any(|candidate| key.eq_ignore_ascii_case(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_shift_tab() {
        let ks = gpui::Keystroke::parse("shift-tab").expect("parse shift-tab");
        assert_eq!(encode_keystroke(&ks), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn encodes_ctrl_left() {
        let ks = gpui::Keystroke::parse("ctrl-left").expect("parse ctrl-left");
        assert_eq!(encode_keystroke(&ks), Some(b"\x1b[1;5D".to_vec()));
    }

    #[test]
    fn encodes_alt_printable_with_escape_prefix() {
        let ks = gpui::Keystroke::parse("alt-x").expect("parse alt-x");
        assert_eq!(encode_keystroke(&ks), Some(vec![0x1b, b'x']));
    }

    #[test]
    fn encodes_alt_ascii_key_instead_of_macos_option_glyph() {
        let ks = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                alt: true,
                ..gpui::Modifiers::none()
            },
            key: "w".to_string(),
            key_char: Some("∑".to_string()),
        };
        assert_eq!(encode_keystroke(&ks), Some(vec![0x1b, b'w']));
    }

    #[test]
    fn preserves_alt_shift_ascii_key_characters() {
        let ks = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                alt: true,
                shift: true,
                ..gpui::Modifiers::none()
            },
            key: "w".to_string(),
            key_char: Some("W".to_string()),
        };
        assert_eq!(encode_keystroke(&ks), Some(vec![0x1b, b'W']));
    }

    #[test]
    fn mode_aware_encoding_keeps_default_legacy_behavior() {
        let ks = gpui::Keystroke::parse("alt-x").expect("parse alt-x");
        assert_eq!(
            encode_keystroke_with_mode(&ks, TermMode::default(), KittyKeyEventType::Press),
            Some(vec![0x1b, b'x'])
        );
    }

    #[test]
    fn kitty_disambiguates_alt_printable_as_csi_u() {
        let ks = gpui::Keystroke::parse("alt-x").expect("parse alt-x");
        assert_eq!(
            encode_keystroke_with_mode(
                &ks,
                TermMode::DISAMBIGUATE_ESC_CODES,
                KittyKeyEventType::Press
            ),
            Some(b"\x1b[120;3u".to_vec())
        );
    }

    #[test]
    fn kitty_reports_all_plain_printable_keys_as_csi_u() {
        let ks = gpui::Keystroke {
            modifiers: gpui::Modifiers::none(),
            key: "x".to_string(),
            key_char: Some("x".to_string()),
        };
        assert_eq!(
            encode_keystroke_with_mode(
                &ks,
                TermMode::REPORT_ALL_KEYS_AS_ESC,
                KittyKeyEventType::Press
            ),
            Some(b"\x1b[120;1u".to_vec())
        );
    }

    #[test]
    fn kitty_reports_repeat_event_type_when_requested() {
        let ks = gpui::Keystroke {
            modifiers: gpui::Modifiers::none(),
            key: "x".to_string(),
            key_char: Some("x".to_string()),
        };
        assert_eq!(
            encode_keystroke_with_mode(
                &ks,
                TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_EVENT_TYPES,
                KittyKeyEventType::Repeat
            ),
            Some(b"\x1b[120;1:2u".to_vec())
        );
    }

    #[test]
    fn kitty_reports_release_event_type_when_requested() {
        let ks = gpui::Keystroke {
            modifiers: gpui::Modifiers::none(),
            key: "x".to_string(),
            key_char: Some("x".to_string()),
        };
        assert_eq!(
            encode_keystroke_with_mode(
                &ks,
                TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_EVENT_TYPES,
                KittyKeyEventType::Release
            ),
            Some(b"\x1b[120;1:3u".to_vec())
        );
    }

    #[test]
    fn kitty_associated_text_embeds_key_char_codepoints() {
        let ks = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                shift: true,
                ..gpui::Modifiers::none()
            },
            key: "a".to_string(),
            key_char: Some("A".to_string()),
        };
        assert_eq!(
            encode_keystroke_with_mode(
                &ks,
                TermMode::REPORT_ALL_KEYS_AS_ESC
                    | TermMode::REPORT_ALTERNATE_KEYS
                    | TermMode::REPORT_ASSOCIATED_TEXT,
                KittyKeyEventType::Press
            ),
            Some(b"\x1b[97:65;2;65u".to_vec())
        );
    }
}
