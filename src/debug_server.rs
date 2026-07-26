use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::Instant;

use parking_lot::Mutex;
use serde::Serialize;
use tiny_http::{Header, Response, Server, StatusCode};

use crate::GridSize;
use crate::pty::write_to_pty;

const DEBUG_HTTP_DEFAULT_HOST: &str = "127.0.0.1";
const DEBUG_HTTP_DEFAULT_PORT_START: u16 = 37878;
const DEBUG_HTTP_DEFAULT_PORT_END: u16 = 37977;
const DEBUG_HTTP_LOG_ENV: &str = "AGENT_TUI_DEBUG_HTTP_LOG";
static NEXT_DEBUG_HTTP_PORT: AtomicU16 = AtomicU16::new(DEBUG_HTTP_DEFAULT_PORT_START);

/// 请求体上限。请求循环是单线程串行的，没有上限时一个慢速大 body 就能长时间
/// 占住整个调试接口。
const MAX_DEBUG_BODY_BYTES: u64 = 1024 * 1024;

/// 携带 token 的请求头。
///
/// 用**自定义**头（而不是 query 参数）本身就是一层 CSRF 防护：浏览器给跨源请求
/// 加自定义头会触发 CORS 预检，而我们不回 CORS 头，预检必然失败。原来的接口之所以
/// 能被任意网页打穿，正是因为 `Content-Type: text/plain` 属于 CORS safelisted，
/// 无需预检即可发出 POST。
const DEBUG_TOKEN_HEADER: &str = "X-Debug-Token";

/// debug HTTP 服务的运行配置。
#[derive(Clone)]
pub(crate) struct DebugHttpConfig {
    /// 所有端点都需要它——不只是写端点。`/debug/state` 与 `/debug/screen` 会吐出
    /// 整屏文本（密钥、token、SSH 会话内容），泄露读接口和放开写接口一样严重。
    token: String,
    /// 允许绑定非 loopback 地址。默认 false。
    allow_remote: bool,
}

impl DebugHttpConfig {
    /// `token` 为 None 时随机生成一个。
    ///
    /// 拿不到系统随机数时返回 None（调用方据此拒绝启动），而不是退化成一个可猜的
    /// 弱 token——那样比不开这个接口更危险，因为用户会以为自己有保护。
    pub(crate) fn new(token: Option<String>, allow_remote: bool) -> Option<Self> {
        let token = match token.map(|value| value.trim().to_string()) {
            Some(value) if !value.is_empty() => value,
            _ => random_token()?,
        };

        Some(Self {
            token,
            allow_remote,
        })
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}

fn random_token() -> Option<String> {
    use std::io::Read;

    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .ok()?;

    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// 常量时间比较，避免用响应时间逐字节猜 token。
fn token_matches(expected: &str, provided: &str) -> bool {
    let expected = expected.as_bytes();
    let provided = provided.as_bytes();
    if expected.len() != provided.len() {
        return false;
    }

    expected
        .iter()
        .zip(provided)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn header_value<'a>(request: &'a tiny_http::Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
}

/// 校验 `Host` 头指向 loopback。
///
/// 这是 DNS rebinding 防护：攻击者控制的域名可以解析到 127.0.0.1，此时浏览器发出的
/// 请求确实打到本机，但 `Host` 头会是攻击者的域名。
fn host_is_loopback(request: &tiny_http::Request) -> bool {
    let Some(host) = header_value(request, "Host") else {
        // HTTP/1.1 要求带 Host；缺失就拒绝，不猜。
        return false;
    };

    let host = host.trim();
    let hostname = match host.rsplit_once(':') {
        // IPv6 字面量形如 [::1]:37878
        Some((name, port)) if port.chars().all(|ch| ch.is_ascii_digit()) => name,
        _ => host,
    };

    let hostname = hostname.trim_start_matches('[').trim_end_matches(']');
    hostname.eq_ignore_ascii_case("localhost")
        || hostname
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct DebugCounters {
    bytes_from_pty: u64,
    bytes_to_pty: u64,
    key_events: u64,
    injected_events: u64,
    resize_events: u64,
    http_requests: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct DebugState {
    started_at: Instant,
    listening_addr: Option<String>,
    shell: String,
    status: String,
    note: Option<String>,
    grid_size: GridSize,
    cursor_row: usize,
    cursor_col: usize,
    screen_lines: Vec<String>,
    counters: DebugCounters,
    last_error: Option<String>,
}

#[derive(Serialize)]
struct DebugStateSnapshot {
    shell: String,
    status: String,
    note: Option<String>,
    listening_addr: Option<String>,
    grid_size: GridSize,
    cursor_row: usize,
    cursor_col: usize,
    screen_lines: Vec<String>,
    counters: DebugCounters,
    uptime_ms: u128,
    last_error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct SharedDebugState {
    inner: Arc<Mutex<DebugState>>,
}

impl SharedDebugState {
    pub(crate) fn new(shell: String, status: String, grid_size: GridSize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DebugState {
                started_at: Instant::now(),
                listening_addr: None,
                shell,
                status,
                note: None,
                grid_size,
                cursor_row: 0,
                cursor_col: 0,
                screen_lines: Vec::new(),
                counters: DebugCounters::default(),
                last_error: None,
            })),
        }
    }

    pub(crate) fn set_listening_addr(&self, addr: String) {
        self.inner.lock().listening_addr = Some(addr);
    }

    pub(crate) fn set_error(&self, err: impl Into<String>) {
        self.inner.lock().last_error = Some(err.into());
    }

    pub(crate) fn set_note(&self, note: Option<String>) {
        self.inner.lock().note = note;
    }

    pub(crate) fn note(&self) -> Option<String> {
        self.inner.lock().note.clone()
    }

    pub(crate) fn record_http_request(&self) {
        self.inner.lock().counters.http_requests += 1;
    }

    pub(crate) fn record_bytes_from_pty(&self, bytes: usize) {
        self.inner.lock().counters.bytes_from_pty += bytes as u64;
    }

    pub(crate) fn record_bytes_to_pty(&self, bytes: usize, injected: bool) {
        let mut state = self.inner.lock();
        state.counters.bytes_to_pty += bytes as u64;
        if injected {
            state.counters.injected_events += 1;
        }
    }

    pub(crate) fn record_key_event(&self) {
        self.inner.lock().counters.key_events += 1;
    }

    pub(crate) fn record_resize(&self) {
        self.inner.lock().counters.resize_events += 1;
    }

    pub(crate) fn update_screen_snapshot(
        &self,
        grid_size: GridSize,
        cursor_row: usize,
        cursor_col: usize,
        screen_lines: Vec<String>,
    ) {
        let mut state = self.inner.lock();
        state.grid_size = grid_size;
        state.cursor_row = cursor_row;
        state.cursor_col = cursor_col;
        state.screen_lines = screen_lines;
    }

    pub(crate) fn status_summary(&self) -> String {
        let state = self.inner.lock();
        let uptime = state.started_at.elapsed().as_secs();
        let addr = state.listening_addr.as_deref().unwrap_or("starting");
        format!(
            "{} | {}x{} | in:{} out:{} key:{} inj:{} req:{} resize:{} up:{}s dbg:{}",
            state.status,
            state.grid_size.cols,
            state.grid_size.rows,
            state.counters.bytes_from_pty,
            state.counters.bytes_to_pty,
            state.counters.key_events,
            state.counters.injected_events,
            state.counters.http_requests,
            state.counters.resize_events,
            uptime,
            addr,
        )
    }

    pub(crate) fn state_json(&self) -> String {
        let state = self.inner.lock();
        let snapshot = DebugStateSnapshot {
            shell: state.shell.clone(),
            status: state.status.clone(),
            note: state.note.clone(),
            listening_addr: state.listening_addr.clone(),
            grid_size: state.grid_size,
            cursor_row: state.cursor_row,
            cursor_col: state.cursor_col,
            screen_lines: state.screen_lines.clone(),
            counters: state.counters.clone(),
            uptime_ms: state.started_at.elapsed().as_millis(),
            last_error: state.last_error.clone(),
        };

        serde_json::to_string_pretty(&snapshot)
            .unwrap_or_else(|_| "{\"error\":\"serialize failed\"}".to_string())
    }

    pub(crate) fn screen_text(&self) -> String {
        let state = self.inner.lock();
        if state.screen_lines.is_empty() {
            return "<empty screen>\n".to_string();
        }

        let mut out = state.screen_lines.join("\n");
        out.push('\n');
        out
    }
}

pub(crate) fn start_debug_http_server(
    debug: SharedDebugState,
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    config: DebugHttpConfig,
) {
    if let Some(addr) = std::env::var("AGENT_TUI_DEBUG_ADDR")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        if !config.allow_remote && !addr_is_loopback(&addr) {
            let message = format!(
                "refusing to start debug server on non-loopback address {addr}: \
                 this endpoint can execute arbitrary commands in your shell. \
                 Pass --debug-http-allow-remote if you really mean it."
            );
            eprintln!("{message}");
            debug.set_error(message);
            return;
        }

        start_debug_http_server_at_addr(debug, writer, addr, config);
        return;
    }

    start_debug_http_server_on_default_port_range(debug, writer, config);
}

/// 解析 `AGENT_TUI_DEBUG_ADDR` 并要求它落在 loopback 上。
///
/// 先按 `SocketAddr` 解析；失败则走 DNS 解析，并要求**所有**解析结果都是 loopback
/// （任一条不是就拒绝，避免一个既解析到 127.0.0.1 又解析到外网地址的名字混过去）。
fn addr_is_loopback(addr: &str) -> bool {
    use std::net::ToSocketAddrs;

    if let Ok(parsed) = addr.parse::<std::net::SocketAddr>() {
        return parsed.ip().is_loopback();
    }

    match addr.to_socket_addrs() {
        Ok(mut resolved) => {
            let mut saw_any = false;
            let all_loopback = resolved.all(|candidate| {
                saw_any = true;
                candidate.ip().is_loopback()
            });
            saw_any && all_loopback
        }
        // 解析不了就交给 Server::http 去报错，但不当作 loopback。
        Err(_) => false,
    }
}

fn start_debug_http_server_on_default_port_range(
    debug: SharedDebugState,
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    config: DebugHttpConfig,
) {
    let _ = thread::Builder::new()
        .name("agent-debug-http".to_string())
        .spawn(move || {
            let mut last_error = None;
            for _ in DEBUG_HTTP_DEFAULT_PORT_START..=DEBUG_HTTP_DEFAULT_PORT_END {
                let addr = next_default_debug_http_addr();
                match Server::http(&addr) {
                    Ok(server) => {
                        serve_debug_http(server, debug, writer, addr, config);
                        return;
                    }
                    Err(err) => {
                        last_error = Some(format!("failed to start debug server on {addr}: {err}"));
                    }
                }
            }

            debug.set_error(last_error.unwrap_or_else(|| {
                format!(
                    "failed to start debug server in {DEBUG_HTTP_DEFAULT_HOST}:{}-{}",
                    DEBUG_HTTP_DEFAULT_PORT_START, DEBUG_HTTP_DEFAULT_PORT_END
                )
            }));
        });
}

fn next_default_debug_http_addr() -> String {
    let port = NEXT_DEBUG_HTTP_PORT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(
                if (DEBUG_HTTP_DEFAULT_PORT_START..DEBUG_HTTP_DEFAULT_PORT_END).contains(&current) {
                    current + 1
                } else {
                    DEBUG_HTTP_DEFAULT_PORT_START
                },
            )
        })
        .unwrap_or(DEBUG_HTTP_DEFAULT_PORT_START);
    let port = if (DEBUG_HTTP_DEFAULT_PORT_START..=DEBUG_HTTP_DEFAULT_PORT_END).contains(&port) {
        port
    } else {
        DEBUG_HTTP_DEFAULT_PORT_START
    };

    format!("{DEBUG_HTTP_DEFAULT_HOST}:{port}")
}

pub(crate) fn start_debug_http_server_at_addr(
    debug: SharedDebugState,
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    addr: String,
    config: DebugHttpConfig,
) {
    let _ = thread::Builder::new()
        .name("agent-debug-http".to_string())
        .spawn(move || {
            let server = match Server::http(&addr) {
                Ok(server) => server,
                Err(err) => {
                    debug.set_error(format!("failed to start debug server on {addr}: {err}"));
                    return;
                }
            };

            serve_debug_http(server, debug, writer, addr, config);
        });
}

fn serve_debug_http(
    server: Server,
    debug: SharedDebugState,
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    addr: String,
    config: DebugHttpConfig,
) {
    debug.set_listening_addr(addr.clone());
    if should_log_debug_http_start() {
        eprintln!("debug http listening on http://{addr}");
    }

    for mut request in server.incoming_requests() {
        debug.record_http_request();
        let method = request.method().as_str().to_string();
        let path = request.url().split('?').next().unwrap_or("/").to_string();

        let response = match reject_unauthorized(&request, &config) {
            Some(rejection) => rejection,
            None => handle_debug_request(&mut request, &method, &path, &debug, writer.as_ref()),
        };

        if let Err(err) = request.respond(response) {
            debug.set_error(format!("failed to send HTTP response: {err}"));
        }
    }
}

/// 在分派到任何端点之前统一做准入检查。
///
/// 读端点也要过这一关：`/debug/state` 和 `/debug/screen` 会返回整屏文本。
fn reject_unauthorized(
    request: &tiny_http::Request,
    config: &DebugHttpConfig,
) -> Option<Response<std::io::Cursor<Vec<u8>>>> {
    if !config.allow_remote && !host_is_loopback(request) {
        return Some(text_response(
            403,
            "text/plain; charset=utf-8",
            "forbidden: Host header must be loopback\n",
        ));
    }

    let provided = header_value(request, DEBUG_TOKEN_HEADER).unwrap_or_default();
    if !token_matches(&config.token, provided) {
        return Some(text_response(
            401,
            "text/plain; charset=utf-8",
            "unauthorized: missing or invalid X-Debug-Token\n",
        ));
    }

    if request
        .body_length()
        .is_some_and(|length| length as u64 > MAX_DEBUG_BODY_BYTES)
    {
        return Some(text_response(
            413,
            "text/plain; charset=utf-8",
            "payload too large\n",
        ));
    }

    None
}

fn should_log_debug_http_start() -> bool {
    std::env::var(DEBUG_HTTP_LOG_ENV)
        .ok()
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// 读取请求体时的硬上限。
///
/// `reject_unauthorized` 已经按 Content-Length 早退，但那个头可以缺失（chunked）
/// 也可以撒谎，所以实际读取这一层必须自己截断。
fn bounded_reader(request: &mut tiny_http::Request) -> impl std::io::Read + '_ {
    // as_reader() 给的是 &mut dyn Read（unsized），不能直接 .take()；
    // 显式把 Self 定成 &mut dyn Read（它本身是 Sized 且实现了 Read）即可。
    let reader: &mut dyn std::io::Read = request.as_reader();
    std::io::Read::take(reader, MAX_DEBUG_BODY_BYTES)
}

pub(crate) fn handle_debug_request(
    request: &mut tiny_http::Request,
    method: &str,
    path: &str,
    debug: &SharedDebugState,
    writer: Option<&Arc<Mutex<Box<dyn Write + Send>>>>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    match (method, path) {
        ("GET", "/debug") => text_response(
            200,
            "text/plain; charset=utf-8",
            "available endpoints:\nGET /debug/state\nGET /debug/screen\nPOST /debug/input (raw body)\nPOST /debug/replace-line (text body)\nPOST /debug/note (text body)\n",
        ),
        ("GET", "/debug/state") => {
            let json = debug.state_json();
            text_response(200, "application/json; charset=utf-8", json)
        }
        ("GET", "/debug/screen") => {
            let text = debug.screen_text();
            text_response(200, "text/plain; charset=utf-8", text)
        }
        ("POST", "/debug/note") => {
            let mut body = String::new();
            if let Err(err) = bounded_reader(request).read_to_string(&mut body) {
                debug.set_error(format!("failed to read note body: {err}"));
                return text_response(400, "text/plain; charset=utf-8", "invalid note body\n");
            }

            let note = body.trim();
            if note.is_empty() {
                debug.set_note(None);
                text_response(200, "text/plain; charset=utf-8", "note cleared\n")
            } else {
                debug.set_note(Some(note.to_string()));
                text_response(200, "text/plain; charset=utf-8", "note set\n")
            }
        }
        ("POST", "/debug/input") => {
            let Some(writer) = writer else {
                return text_response(503, "text/plain; charset=utf-8", "pty writer unavailable\n");
            };

            let mut body = Vec::new();
            if let Err(err) = bounded_reader(request).read_to_end(&mut body) {
                debug.set_error(format!("failed to read input body: {err}"));
                return text_response(400, "text/plain; charset=utf-8", "invalid input body\n");
            }

            if body.is_empty() {
                return text_response(400, "text/plain; charset=utf-8", "input body is empty\n");
            }

            match write_to_pty(writer, &body) {
                Ok(()) => {
                    debug.record_bytes_to_pty(body.len(), true);
                    text_response(200, "text/plain; charset=utf-8", "input injected\n")
                }
                Err(err) => {
                    debug.set_error(format!("debug input write failed: {err:#}"));
                    text_response(500, "text/plain; charset=utf-8", "failed to write to pty\n")
                }
            }
        }
        ("POST", "/debug/replace-line") => {
            let Some(writer) = writer else {
                return text_response(503, "text/plain; charset=utf-8", "pty writer unavailable\n");
            };

            let mut body = Vec::new();
            if let Err(err) = bounded_reader(request).read_to_end(&mut body) {
                debug.set_error(format!("failed to read replace-line body: {err}"));
                return text_response(
                    400,
                    "text/plain; charset=utf-8",
                    "invalid replace-line body\n",
                );
            }

            let mut payload = Vec::with_capacity(body.len() + 1);
            payload.push(0x15);
            payload.extend_from_slice(&body);

            match write_to_pty(writer, &payload) {
                Ok(()) => {
                    debug.record_bytes_to_pty(payload.len(), true);
                    text_response(200, "text/plain; charset=utf-8", "input line replaced\n")
                }
                Err(err) => {
                    debug.set_error(format!("debug replace-line write failed: {err:#}"));
                    text_response(500, "text/plain; charset=utf-8", "failed to write to pty\n")
                }
            }
        }
        _ => text_response(404, "text/plain; charset=utf-8", "not found\n"),
    }
}

fn text_response(
    status: u16,
    content_type: &str,
    body: impl Into<Vec<u8>>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response = Response::from_data(body.into()).with_status_code(StatusCode(status));
    if let Ok(header) = Header::from_bytes("Content-Type", content_type) {
        response = response.with_header(header);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    const TEST_TOKEN: &str = "test-token";

    fn test_config() -> DebugHttpConfig {
        DebugHttpConfig::new(Some(TEST_TOKEN.to_string()), false).expect("explicit token")
    }

    #[test]
    fn debug_http_serves_state_and_note() {
        let addr = reserve_local_addr();
        let debug = SharedDebugState::new(
            "test-shell".to_string(),
            "connected".to_string(),
            GridSize { cols: 80, rows: 24 },
        );
        start_debug_http_server_at_addr(debug.clone(), None, addr.clone(), test_config());

        wait_for_server(&addr);

        let state = send_http(&addr, get_request(&addr, "/debug/state", Some(TEST_TOKEN)));
        assert!(state.contains("\"shell\": \"test-shell\""));
        assert!(state.contains("\"status\": \"connected\""));

        let note_response = send_http(
            &addr,
            post_request(&addr, "/debug/note", Some(TEST_TOKEN), "hello from test"),
        );
        assert!(note_response.contains("note set"));

        let state_after_note =
            send_http(&addr, get_request(&addr, "/debug/state", Some(TEST_TOKEN)));
        assert!(state_after_note.contains("\"note\": \"hello from test\""));
    }

    #[test]
    fn debug_http_injects_input_to_writer() {
        let (addr, sink) = start_server_with_writer();

        let payload = "echo injected\n";
        let response = send_http(
            &addr,
            post_request(&addr, "/debug/input", Some(TEST_TOKEN), payload),
        );
        assert!(response.contains("input injected"));

        wait_for_sink(&sink, payload.as_bytes());
    }

    #[test]
    fn debug_http_replaces_input_line_in_writer() {
        let (addr, sink) = start_server_with_writer();

        let payload = "replace with this";
        let response = send_http(
            &addr,
            post_request(&addr, "/debug/replace-line", Some(TEST_TOKEN), payload),
        );
        assert!(response.contains("input line replaced"));

        let mut expected = Vec::from([0x15]);
        expected.extend_from_slice(payload.as_bytes());
        wait_for_sink(&sink, &expected);
    }

    /// 回归：曾经任何本机进程（以及任何网页，靠 CORS safelisted 的 text/plain POST）
    /// 都能无认证往 PTY 注入字节。现在无 token 必须被拒，且**一个字节都不能落到 PTY**。
    #[test]
    fn debug_http_rejects_input_without_token() {
        let (addr, sink) = start_server_with_writer();

        let response = send_http(
            &addr,
            post_request(&addr, "/debug/input", None, "echo pwned\n"),
        );
        assert!(response.starts_with("HTTP/1.1 401"), "response: {response}");

        assert!(
            sink.lock().is_empty(),
            "unauthenticated request must not reach the PTY"
        );
    }

    #[test]
    fn debug_http_rejects_input_with_wrong_token() {
        let (addr, sink) = start_server_with_writer();

        let response = send_http(
            &addr,
            post_request(&addr, "/debug/input", Some("wrong-token"), "echo pwned\n"),
        );
        assert!(response.starts_with("HTTP/1.1 401"), "response: {response}");
        assert!(sink.lock().is_empty(), "wrong token must not reach the PTY");
    }

    /// 读端点同样要认证：/debug/state 会带回整屏文本。
    #[test]
    fn debug_http_rejects_state_without_token() {
        let addr = reserve_local_addr();
        let debug = SharedDebugState::new(
            "secret-shell".to_string(),
            "connected".to_string(),
            GridSize { cols: 80, rows: 24 },
        );
        start_debug_http_server_at_addr(debug, None, addr.clone(), test_config());
        wait_for_server(&addr);

        let response = send_http(&addr, get_request(&addr, "/debug/state", None));
        assert!(response.starts_with("HTTP/1.1 401"), "response: {response}");
        assert!(
            !response.contains("secret-shell"),
            "screen state must not leak to unauthenticated callers"
        );
    }

    /// DNS rebinding 防护：请求真的打到了本机，但 Host 头是攻击者的域名。
    #[test]
    fn debug_http_rejects_foreign_host_header() {
        let (addr, sink) = start_server_with_writer();

        let body = "echo pwned\n";
        let response = send_http(
            &addr,
            format!(
                "POST /debug/input HTTP/1.1\r\nHost: attacker.example.com\r\n\
                 {DEBUG_TOKEN_HEADER}: {TEST_TOKEN}\r\nContent-Type: text/plain\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        );
        assert!(response.starts_with("HTTP/1.1 403"), "response: {response}");
        assert!(
            sink.lock().is_empty(),
            "foreign Host must not reach the PTY"
        );
    }

    #[test]
    fn debug_http_rejects_oversized_body() {
        let (addr, sink) = start_server_with_writer();

        // 只声明超大 Content-Length，不真的发送——服务端应在读取之前就拒绝。
        let response = send_http(
            &addr,
            format!(
                "POST /debug/input HTTP/1.1\r\nHost: {addr}\r\n\
                 {DEBUG_TOKEN_HEADER}: {TEST_TOKEN}\r\nContent-Type: text/plain\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_DEBUG_BODY_BYTES + 1
            ),
        );
        assert!(response.starts_with("HTTP/1.1 413"), "response: {response}");
        assert!(sink.lock().is_empty());
    }

    #[test]
    fn debug_http_returns_404_for_unknown_path() {
        let addr = reserve_local_addr();
        let debug = SharedDebugState::new(
            "test-shell".to_string(),
            "connected".to_string(),
            GridSize { cols: 80, rows: 24 },
        );
        start_debug_http_server_at_addr(debug, None, addr.clone(), test_config());
        wait_for_server(&addr);

        let response = send_http(&addr, get_request(&addr, "/debug/nope", Some(TEST_TOKEN)));
        assert!(response.starts_with("HTTP/1.1 404"), "response: {response}");
    }

    #[test]
    fn generated_token_is_not_predictable() {
        let first = DebugHttpConfig::new(None, false).expect("/dev/urandom available");
        let second = DebugHttpConfig::new(None, false).expect("/dev/urandom available");

        assert_eq!(first.token().len(), 32);
        assert!(first.token().chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_ne!(first.token(), second.token());
    }

    #[test]
    fn blank_explicit_token_falls_back_to_random() {
        let config = DebugHttpConfig::new(Some("   ".to_string()), false).expect("random fallback");
        assert_eq!(config.token().len(), 32);
    }

    #[test]
    fn token_comparison_rejects_mismatches() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", "ab"));
        assert!(!token_matches("abc", "abcd"));
        assert!(!token_matches("abc", ""));
    }

    #[test]
    fn only_loopback_addresses_are_accepted_by_default() {
        assert!(addr_is_loopback("127.0.0.1:37878"));
        assert!(addr_is_loopback("localhost:37878"));
        assert!(addr_is_loopback("[::1]:37878"));

        // 这是 review 里的 S-2：把"任意命令执行"接口暴露到局域网。
        assert!(!addr_is_loopback("0.0.0.0:8080"));
        assert!(!addr_is_loopback("192.168.1.10:8080"));
    }

    fn start_server_with_writer() -> (String, Arc<Mutex<Vec<u8>>>) {
        let addr = reserve_local_addr();
        let debug = SharedDebugState::new(
            "test-shell".to_string(),
            "connected".to_string(),
            GridSize { cols: 80, rows: 24 },
        );
        let sink = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(BufferWriter { sink: sink.clone() })));

        start_debug_http_server_at_addr(debug, Some(writer), addr.clone(), test_config());
        wait_for_server(&addr);
        (addr, sink)
    }

    fn get_request(addr: &str, path: &str, token: Option<&str>) -> String {
        let auth = match token {
            Some(token) => format!("{DEBUG_TOKEN_HEADER}: {token}\r\n"),
            None => String::new(),
        };
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n")
    }

    fn post_request(addr: &str, path: &str, token: Option<&str>, body: &str) -> String {
        let auth = match token {
            Some(token) => format!("{DEBUG_TOKEN_HEADER}: {token}\r\n"),
            None => String::new(),
        };
        format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Content-Type: text/plain\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn wait_for_sink(sink: &Arc<Mutex<Vec<u8>>>, expected: &[u8]) {
        for _ in 0..40 {
            if sink.lock().as_slice() == expected {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "debug bytes were not forwarded to PTY writer; got {:?}",
            sink.lock()
        );
    }

    fn reserve_local_addr() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral address");
        let addr = listener.local_addr().expect("read local addr");
        drop(listener);
        addr.to_string()
    }

    fn wait_for_server(addr: &str) {
        for _ in 0..40 {
            if TcpStream::connect(addr).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("debug HTTP server did not start in time");
    }

    fn send_http(addr: &str, request: String) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect to debug server");
        stream
            .write_all(request.as_bytes())
            .expect("send request to debug server");
        let _ = stream.shutdown(std::net::Shutdown::Write);

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read response from debug server");
        response
    }

    struct BufferWriter {
        sink: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for BufferWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.sink.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
