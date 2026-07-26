use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum AmbiguousWidth {
    Single,
    Double,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Theme {
    Default,
    EyeCare,
}

#[derive(Clone, Parser)]
#[command(name = "agent_terminal")]
pub(crate) struct CliOptions {
    #[arg(long)]
    pub(crate) self_check: bool,
    #[arg(long)]
    pub(crate) show_status_bar: bool,
    #[arg(long, default_value = "Menlo")]
    pub(crate) font_family: String,
    #[arg(long = "font-fallback", value_delimiter = ',')]
    pub(crate) font_fallbacks: Vec<String>,
    #[arg(long = "double-width-char", value_delimiter = ',')]
    pub(crate) double_width_chars: Vec<String>,
    #[arg(long, value_enum, default_value_t = AmbiguousWidth::Single)]
    pub(crate) ambiguous_width: AmbiguousWidth,
    #[arg(long, value_enum, default_value_t = Theme::Default)]
    pub(crate) theme: Theme,
    #[arg(long)]
    pub(crate) input_log_file: Option<PathBuf>,
    #[arg(long)]
    pub(crate) input_log_raw: bool,
    #[arg(long, help = "Disable smooth cursor slide animation")]
    pub(crate) no_cursor_slide: bool,
    #[arg(
        long,
        help = "Force a vertical beam cursor regardless of app cursor mode"
    )]
    pub(crate) force_vertical_cursor: bool,
    #[arg(long, help = "Enable subtle trailing effect for vertical beam cursor")]
    pub(crate) cursor_trail: bool,
    #[arg(
        long,
        help = "Treat macOS Option key as plain text input instead of Meta/Alt"
    )]
    pub(crate) no_option_as_meta: bool,
    /// 默认关闭。这个接口能往 PTY 写任意字节，等于在用户 shell 里执行任意命令，
    /// 所以必须显式开启，而且开启后强制要求 token（见 --debug-http-token）。
    #[arg(
        long,
        help = "Enable the local debug HTTP server (can inject input into your shell)"
    )]
    pub(crate) debug_http: bool,
    #[arg(
        long,
        value_name = "TOKEN",
        help = "Token required by the debug HTTP server; a random one is generated when omitted"
    )]
    pub(crate) debug_http_token: Option<String>,
    /// AGENT_TUI_DEBUG_ADDR 指向非 loopback 地址时必须同时给这个 flag，
    /// 否则拒绝启动——把"任意命令执行"接口绑到 0.0.0.0 不该是一次手滑就能做到的事。
    #[arg(
        long,
        help = "Allow the debug HTTP server to bind a non-loopback address (dangerous)"
    )]
    pub(crate) debug_http_allow_remote: bool,
}

#[cfg(test)]
pub(crate) fn parse_cli_options_from<I>(args: I) -> CliOptions
where
    I: IntoIterator<Item = String>,
{
    CliOptions::parse_from(std::iter::once("agent_terminal".to_string()).chain(args))
}

pub(crate) fn parse_cli_options() -> CliOptions {
    CliOptions::parse()
}

#[cfg(test)]
mod tests {
    use super::{AmbiguousWidth, Theme, parse_cli_options_from};

    #[test]
    fn ambiguous_width_defaults_to_single() {
        let cli = parse_cli_options_from(Vec::<String>::new());
        assert_eq!(cli.ambiguous_width, AmbiguousWidth::Single);
    }

    #[test]
    fn ambiguous_width_accepts_double() {
        let cli =
            parse_cli_options_from(vec!["--ambiguous-width".to_string(), "double".to_string()]);
        assert_eq!(cli.ambiguous_width, AmbiguousWidth::Double);
    }

    #[test]
    fn theme_defaults_to_default() {
        let cli = parse_cli_options_from(Vec::<String>::new());
        assert_eq!(cli.theme, Theme::Default);
    }

    #[test]
    fn theme_accepts_eye_care() {
        let cli = parse_cli_options_from(vec!["--theme".to_string(), "eye-care".to_string()]);
        assert_eq!(cli.theme, Theme::EyeCare);
    }

    #[test]
    fn input_log_file_argument_parses() {
        let cli = parse_cli_options_from(vec![
            "--input-log-file".to_string(),
            "/tmp/agent-input.jsonl".to_string(),
        ]);
        assert_eq!(
            cli.input_log_file.as_deref(),
            Some(std::path::Path::new("/tmp/agent-input.jsonl"))
        );
    }

    /// 这个接口能往用户 shell 注入任意命令，默认必须是关的。
    #[test]
    fn debug_http_is_disabled_by_default() {
        let cli = parse_cli_options_from(Vec::<String>::new());
        assert!(!cli.debug_http);
        assert!(cli.debug_http_token.is_none());
        assert!(!cli.debug_http_allow_remote);
    }

    #[test]
    fn debug_http_flags_parse() {
        let cli = parse_cli_options_from(vec![
            "--debug-http".to_string(),
            "--debug-http-token".to_string(),
            "s3cret".to_string(),
            "--debug-http-allow-remote".to_string(),
        ]);
        assert!(cli.debug_http);
        assert_eq!(cli.debug_http_token.as_deref(), Some("s3cret"));
        assert!(cli.debug_http_allow_remote);
    }

    #[test]
    fn cursor_slide_enabled_by_default() {
        let cli = parse_cli_options_from(Vec::<String>::new());
        assert!(!cli.no_cursor_slide);
    }

    #[test]
    fn cursor_slide_can_be_disabled() {
        let cli = parse_cli_options_from(vec!["--no-cursor-slide".to_string()]);
        assert!(cli.no_cursor_slide);
    }

    #[test]
    fn force_vertical_cursor_defaults_to_disabled() {
        let cli = parse_cli_options_from(Vec::<String>::new());
        assert!(!cli.force_vertical_cursor);
    }

    #[test]
    fn force_vertical_cursor_flag_parses() {
        let cli = parse_cli_options_from(vec!["--force-vertical-cursor".to_string()]);
        assert!(cli.force_vertical_cursor);
    }

    #[test]
    fn cursor_trail_defaults_to_disabled() {
        let cli = parse_cli_options_from(Vec::<String>::new());
        assert!(!cli.cursor_trail);
    }

    #[test]
    fn cursor_trail_flag_parses() {
        let cli = parse_cli_options_from(vec!["--cursor-trail".to_string()]);
        assert!(cli.cursor_trail);
    }

    #[test]
    fn font_fallback_accepts_repeated_and_comma_separated_values() {
        let cli = parse_cli_options_from(vec![
            "--font-fallback".to_string(),
            "Symbols Nerd Font Mono,Apple Symbols".to_string(),
            "--font-fallback".to_string(),
            "Noto Sans Symbols".to_string(),
        ]);
        assert_eq!(
            cli.font_fallbacks,
            vec![
                "Symbols Nerd Font Mono".to_string(),
                "Apple Symbols".to_string(),
                "Noto Sans Symbols".to_string()
            ]
        );
    }

    #[test]
    fn double_width_char_accepts_repeated_and_comma_separated_values() {
        let cli = parse_cli_options_from(vec![
            "--double-width-char".to_string(),
            "↑,↓".to_string(),
            "--double-width-char".to_string(),
            "↕".to_string(),
        ]);
        assert_eq!(
            cli.double_width_chars,
            vec!["↑".to_string(), "↓".to_string(), "↕".to_string()]
        );
    }
}
