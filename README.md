# Agent Terminal

A GPU-accelerated terminal emulator built with [GPUI](https://github.com/zed-industries/zed) (Zed's UI framework) and [alacritty_terminal](https://github.com/alacritty/alacritty), designed as a standalone native macOS terminal with first-class accessibility and input method support.

## Background

This project originated from a specific need: building a terminal emulator that treats **accessibility-driven input** as a first-class concern, rather than an afterthought. Traditional terminal emulators expose minimal accessibility semantics — most only forward raw key events to a PTY, leaving assistive technologies (Voice Control, screen readers, accessibility automation tools) unable to read or modify the current command line.

Agent Terminal takes a different approach:

- It maintains a **shadow input-line model** that mirrors what the user is typing in the shell
- It exposes this model to macOS Accessibility APIs as an `AXTextField`, allowing external tools to **read the current input**, **know the cursor position**, and **inject or replace text**
- It bridges bidirectionally between the native accessibility tree and the internal input state on every render frame

The architecture draws from research into how Zed and Ghostty implement their terminal layers (documented in `research/terminal-implementation-research.md`), adopting the pattern of:

1. Reusing `alacritty_terminal` as the VT/ANSI state machine
2. Deriving a renderer-oriented `ScreenSnapshot` from terminal state
3. Painting through GPUI's canvas as a custom drawing surface

This is **not** intended to be a general-purpose terminal replacement. It is an exploration of what a terminal looks like when designed around agent-assisted and accessibility-first workflows.

## Architecture

```
┌─────────────────────────────────────────────────┐
│                   GPUI Window                    │
│  ┌─────────────────────────────────────────────┐│
│  │  TerminalTabs (tab bar + tab management)    ││
│  ├─────────────────────────────────────────────┤│
│  │  AgentTerminal (per-tab terminal instance)  ││
│  │  ┌─────────────┐  ┌──────────────────────┐ ││
│  │  │ PTY Session  │  │  alacritty_terminal  │ ││
│  │  │ (shell I/O)  │◄►│  Term<Listener>      │ ││
│  │  └─────────────┘  │  Processor            │ ││
│  │                    └──────────┬───────────┘ ││
│  │                               ▼             ││
│  │                    ┌──────────────────────┐ ││
│  │                    │   ScreenSnapshot     │ ││
│  │                    │   cells, cursor,     │ ││
│  │                    │   alt_screen, ...    │ ││
│  │                    └──────────┬───────────┘ ││
│  │                               ▼             ││
│  │  ┌─────────────┐  ┌──────────────────────┐ ││
│  │  │ input_line   │  │  GPUI canvas(...)    │ ││
│  │  │ (shadow      │  │  per-cell text       │ ││
│  │  │  model)      │  │  shaping + paint     │ ││
│  │  └──────┬───────┘  └──────────────────────┘ ││
│  │         ▼                                   ││
│  │  ┌──────────────────────┐                   ││
│  │  │  macOS AX Bridge     │                   ││
│  │  │  AXTextField on      │◄► VoiceControl /  ││
│  │  │  NSView              │   axcli / etc.    ││
│  │  └──────────────────────┘                   ││
│  └─────────────────────────────────────────────┘│
└─────────────────────────────────────────────────┘
```

### Key Components

| File | Lines | Responsibility |
|------|------:|----------------|
| `terminal.rs` | ~2050 | Core terminal state: PTY lifecycle, `Term` wiring, snapshot generation, sub-state structs (cursor slide, latency, selection) |
| `input.rs` | ~1950 | Keyboard/mouse/IME/paste handling, AX override logic, selection gestures |
| `debug_server.rs` | ~1110 | Process-level HTTP debug API with per-tab routing (`/debug/tabs/{id}/...`), auth, tests |
| `keyboard.rs` | ~940 | Keystroke-to-terminal-byte encoding (special keys, Ctrl table, Alt, kitty protocol) |
| `render.rs` | ~890 | GPUI `Render` impl (read-only), run-merged text shaping, cursor drawing |
| `grid_cells.rs` | ~660 | Single home for cell semantics: snapshot conversion, column widths, selection extraction |
| `tabs.rs` | ~600 | Multi-tab management, tab bar rendering, Cmd+N shortcuts |
| `snapshot_tab.rs` | ~450 | Read-only snapshot tabs with scrollback, selection, copy |
| `font_fallback.rs` | ~350 | Background system-font scan scoring CJK/emoji/symbol coverage for fallbacks |
| `pty.rs` | ~290 | PTY creation via `portable-pty`, reader thread (EINTR-safe), dedicated writer thread |
| `color.rs` | ~250 | ANSI → HSLA color mapping (named, indexed 256, dim/bright, spec RGB) |
| `input_mirror.rs` | ~240 | Shadow input-line model: the AX-published line/cursor, mutated only via methods |
| `cli.rs` | ~215 | CLI argument parsing via `clap` |
| `text_utils.rs` | ~190 | UTF-16 ↔ byte index conversion, word deletion, AX override heuristics |
| `macos_ax.rs` | ~140 | Native Objective-C bridge: `setAccessibilityValue` / `setAccessibilitySelectedTextRange` |
| `input_log.rs` | ~100 | Structured JSONL input event logger for debugging |

## Features

### Terminal Emulation
- Full VT/ANSI terminal emulation via `alacritty_terminal`
- ANSI color support: named, 256-color indexed palette, 24-bit true color
- Wide character rendering (CJK) with configurable ambiguous-width handling
- Cursor shapes: block, beam, underline, hidden (respects application cursor mode)
- Smooth cursor slide animation with optional trailing effect
- Alt screen buffer support (vim, less, htop, etc.)
- Mouse reporting (click, motion, drag, scroll wheel) for terminal applications
- Bracketed paste mode
- Focus in/out events (`CSI I` / `CSI O`)
- Terminal title tracking via OSC sequences
- Clickable URL support: `Cmd`-click OSC 8 or visible `http://`, `https://`,
  and `file://` links to open them in the default browser
- Kitty keyboard protocol support: terminal applications can enable CSI-u
  keyboard encoding at runtime with the protocol's mode control sequences

### Input & Accessibility
- Full keyboard input: printable text, Ctrl/Alt/Shift chords, function keys, special keys
- macOS IME integration via `NSTextInputClient` (Chinese/Japanese/Korean input)
- Input-line shadow model synchronized to macOS Accessibility tree as `AXTextField`
- Bidirectional AX bridge: external tools can read and modify the current command line
- AX override guard window (250ms) to avoid conflict between local typing and external edits
- Paste support with `Cmd+V` / `Ctrl+Shift+V`
- File drop support: dropped files are inserted as shell-escaped absolute paths
- Large paste guard: confirmation dialog for multi-line or high non-ASCII content
- `\n` → `\r` conversion in paste for correct behavior in tmux/vi

### Multi-Tab
- `Cmd+T` to open new tabs, `Cmd+W` to close
- `Ctrl+Tab` / `Cmd+Shift+]` / `Cmd+Shift+[` for tab navigation
- `Cmd+1` through `Cmd+0` for direct tab switching
- Snapshot tabs: `Cmd+Shift+S` captures a read-only, scrollable copy of the current terminal

### Appearance
- Custom transparent title bar with native traffic light controls
- Two themes: Default (dark) and Eye Care (green-tinted dark)
- Configurable font family and fallback fonts
- Font zoom with `Cmd+` / `Cmd-`
- Configurable double-width character overrides
- Option key behavior: Meta/Alt (default) or native macOS character input (`--no-option-as-meta`)

### Debugging & Observability

**Debug HTTP server — off by default, and for good reason.** `POST /debug/input` writes
raw bytes to the PTY, so a request containing `\n` runs an arbitrary command in your shell.
Enable it only when you need it, and only for as long as you need it.

```bash
cargo run -- --debug-http                        # prints a generated token on stderr
cargo run -- --debug-http --debug-http-token t   # or supply your own
```

Every request — reads included — must carry the token in an `X-Debug-Token` header:

```bash
curl -H "X-Debug-Token: $TOKEN" http://127.0.0.1:37878/debug/state
```

| Endpoint | Description |
|---|---|
| `GET /debug/tabs` | List live tab sessions and their per-tab endpoints |
| `GET /debug/tabs/{id}/state` | JSON snapshot of that tab's state, counters, uptime |
| `GET /debug/tabs/{id}/screen` | Plain-text dump of that tab's visible content |
| `POST /debug/tabs/{id}/input` | Inject raw bytes into that tab's PTY |
| `POST /debug/tabs/{id}/replace-line` | Replace that tab's current shell input line |
| `GET/POST /debug/{state,screen,input,replace-line}` | Legacy paths; route to the lowest-numbered live tab |

Guarantees the server enforces:

- One server per process on `127.0.0.1:37878-37977`; tabs register sessions and
  unregister automatically when closed (a closed tab's endpoints return 404,
  injection can never reach a dead session).
- Requires the token on **all** endpoints — the read endpoints return your screen contents,
  which is as sensitive as write access.
- Requires a loopback `Host` header, which blocks DNS rebinding.
- Rejects bodies over 1 MB.
- `AGENT_TUI_DEBUG_ADDR=127.0.0.1:<port>` forces a specific address, but a non-loopback
  address is refused unless you also pass `--debug-http-allow-remote`.

The token lives in an `X-Debug-Token` header rather than a query parameter on purpose:
sending a custom header cross-origin forces a CORS preflight, which fails, so a web page
cannot reach these endpoints even though it can reach the port.

Other diagnostics:

- Input event tracing: `AGENT_TUI_INPUT_TRACE=1` (writes key/input summaries to stderr)
- Structured JSONL input logging: `--input-log-file <path>`. ⚠️ Adding `--input-log-raw`
  records **verbatim** keystrokes, IME commits, and pasted text to that file — including any
  secrets you type or paste. Even without `--input-log-raw` the log keeps the first 24
  characters of each value.

## Usage

```bash
# Basic launch
cargo run

# With options
cargo run -- \
  --font-family "JetBrains Mono" \
  --font-fallback "Symbols Nerd Font Mono,Apple Symbols" \
  --theme eye-care \
  --force-vertical-cursor \
  --cursor-trail \
  --ambiguous-width double \
  --double-width-char "↑,↓,↕"

# Self-check (verify terminal core initializes correctly)
cargo run -- --self-check

# With input debugging
AGENT_TUI_INPUT_TRACE=1 cargo run -- --input-log-file /tmp/input.jsonl --input-log-raw
```

Drag files from Finder onto the terminal to insert their absolute paths at the
current shell cursor. Multiple files are inserted in drop order with spaces
between shell-escaped paths.

### CLI Options

| Flag | Default | Description |
|------|---------|-------------|
| `--font-family <name>` | `Menlo` | Terminal font family |
| `--font-fallback <name,...>` | — | Comma-separated fallback font families |
| `--double-width-char <char,...>` | — | Characters forced to double-width rendering |
| `--ambiguous-width <single\|double>` | `single` | Width for Unicode ambiguous-width characters |
| `--theme <default\|eye-care>` | `default` | Color theme |
| `--force-vertical-cursor` | off | Always use beam cursor regardless of app mode |
| `--cursor-trail` | off | Enable trailing glow effect on beam cursor |
| `--no-cursor-slide` | off | Disable smooth cursor movement animation |
| `--no-option-as-meta` | off | Treat Option key as native input instead of Meta/Alt |
| `--show-status-bar` | off | Show debug status bar at bottom |
| `--input-log-file <path>` | — | Write structured input events to JSONL file |
| `--input-log-raw` | off | Include full text values in input log (not truncated) — logs secrets verbatim |
| `--debug-http` | off | Enable the debug HTTP server (can execute commands in your shell) |
| `--debug-http-token <token>` | random | Token required by the debug HTTP server |
| `--debug-http-allow-remote` | off | Permit binding a non-loopback debug address (dangerous) |
| `--self-check` | — | Run startup self-check and exit |

### Kitty Keyboard Protocol

The terminal accepts the runtime mode negotiation described by the
[kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/).
Keyboard input remains legacy by default; CSI-u encoding is activated only
after a terminal application sends a mode control sequence such as
`CSI > flags u` or `CSI = flags ; mode u`, and is deactivated again by the
protocol's pop/difference controls.

Supported protocol behavior:
- `CSI ? u` reports the current enhancement flags.
- `CSI = flags ; mode u`, `CSI > flags u`, and `CSI < number u` update, push,
  and pop keyboard enhancement modes through `alacritty_terminal`.
- Active `DISAMBIGUATE_ESC_CODES` and `REPORT_ALL_KEYS_AS_ESC` modes switch key
  encoding to CSI-u where the protocol requires it.
- `REPORT_EVENT_TYPES` sends press, repeat, and release event types. GPUI repeat
  detection is based on key-down `is_held`; release events are emitted from
  key-up.
- `REPORT_ALTERNATE_KEYS` includes shifted text when GPUI exposes it, and
  `REPORT_ASSOCIATED_TEXT` includes text code points with
  `REPORT_ALL_KEYS_AS_ESC`.

Boundary: GPUI exposes logical keystrokes rather than full physical keyboard
layout metadata, so alternate layout key reporting is limited to shifted text
available on the event.

## Tech Stack

- **UI Framework**: [GPUI](https://github.com/zed-industries/zed) — Zed's GPU-accelerated, Rust-native UI framework
- **Terminal Core**: [alacritty_terminal](https://github.com/alacritty/alacritty) (vendored) — VT/ANSI parsing and terminal state machine
- **PTY**: [portable-pty](https://crates.io/crates/portable-pty) — Cross-platform PTY abstraction
- **macOS Interop**: [cocoa](https://crates.io/crates/cocoa) + [objc](https://crates.io/crates/objc) — Native Objective-C bridge for accessibility APIs
- **CLI**: [clap](https://crates.io/crates/clap) — Argument parsing
- **Debug HTTP**: [tiny_http](https://crates.io/crates/tiny_http) — Lightweight HTTP server for debug endpoints

## Building

Requires macOS (GPUI currently targets macOS). The toolchain is pinned in
`rust-toolchain.toml`, so `rustup` picks the right version automatically.

```bash
cargo fetch                          # required before the patch step below
scripts/apply-vendor-patches.sh      # see "Required dependency patch"
cargo build
scripts/check.sh                     # fmt + clippy + tests + self-check
```

### Required dependency patch

`gpui_macos` has a bug where `append_system_fallbacks` builds an iterator chain it never
consumes, so CoreText's locale-aware cascade list is silently discarded and CJK text falls
back to whatever user font happens to cover it — typically a handwriting face.
[Upstream issue](https://github.com/zed-industries/zed/issues/57916).

`patches/gpui_macos-cjk-fallback.patch` fixes it. Because the `gpui` dependency is pinned to
a git rev, the patch has to be applied inside the cargo checkout, which means
**`cargo clean -p gpui_macos` or a fresh clone silently reverts it**: the build still
succeeds, CJK text just renders wrong. `scripts/apply-vendor-patches.sh` is idempotent, and
`--check` verifies without applying — `scripts/check.sh` and CI both run it, so a missing
patch fails loudly instead of degrading quietly.

Use `scripts/check.sh` rather than bare `cargo fmt`: the vendored `alacritty_terminal` is a
workspace member, so an unscoped format run rewrites the whole upstream tree and destroys the
ability to diff against upstream. See `vendor/alacritty_terminal/VENDOR.md`.

## Security notes

Two features intentionally expose the terminal's contents and input to other local processes.
Both matter when deciding what to type into this terminal.

- **Accessibility integration** (`macos_ax.rs`) publishes the current input line as an
  `AXTextField`. This is a core feature — it is what lets voice control and agents read and
  rewrite your command line — but it also means **any process holding macOS Accessibility
  permission can read your current command line and substitute text into it**.
- **Debug HTTP server** is off by default and requires a token; see
  [Debugging & Observability](#debugging--observability) for the full threat model.
- **`--input-log-raw`** writes keystrokes and pasted text verbatim to disk.

## Known Limitations

- **macOS only** — GPUI's platform layer currently targets macOS; Linux/Windows support depends on upstream
- **Input-line model drift** — the shadow `input_line` can desynchronize from the actual shell state in complex scenarios (shell history navigation, tab completion, `Ctrl-R`); byte-sniffing is a heuristic by construction. The real fix is shell-side reporting (OSC 133 / ZLE hooks) — see strategic decision 1 in `docs/project-review/06-work-plan.md`
- **No scrollback UI** — `Cmd+Shift+S` opens a read-only snapshot tab of the current buffer, but the main screen has no scroll-wheel history (see strategic decision 2; requires fixing ARCH-1 first)
- **No search** — no find-in-terminal functionality
- **Kitty keyboard physical-layout detail** — runtime mode negotiation and
  CSI-u event encoding are supported, but alternate layout key reporting is
  limited by GPUI's logical keystroke data
- **Rebuilds reset the Accessibility grant unless you codesign** — TCC remembers
  authorization by code signature; set `CODESIGN_IDENTITY` when running
  `scripts/build-macos-app.sh` to keep it across rebuilds (ad-hoc fallback signs
  validly but with a per-build hash)
- **`block v0.1.6` future-incompat** — pulled in via `cocoa`/gpui; will be rejected
  by a future Rust release. Unfixable here until zed migrates to objc2; pinned
  toolchain (1.95.0) masks it for now

## License

Private, all rights reserved — see [LICENSE](LICENSE). Build artifacts statically
link Apache-2.0 code (alacritty_terminal, gpui); if a bundle is ever distributed
it must carry [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
