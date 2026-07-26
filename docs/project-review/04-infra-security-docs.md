# 工程基础设施、安全面与文档现状审查

> 子报告 4/4 · 审查基线 `main @ c4301f9`（56 次提交）· 只读分析
> 验证方式：全量 `cargo test`（91 passed）、vendored crate 独立 `cargo test`（135 + 45 + 1 passed）、逐条对照 TODO.md 与当前代码。

---

## 1. 安全问题清单

### 1.1 严重（Critical）

**S-1　调试 HTTP 接口默认开启，无任何认证，可注入任意 PTY 字节 → 本地任意命令执行**

- `src/terminal.rs:429` — `start_debug_http_server(debug.clone(), writer.clone())` 在 `AgentTerminal::new()` 中**无条件调用**，没有任何 CLI/环境变量开关。TODO.md 第 5 条提出的「默认不启动，需 `--debug-http` 显式开启；或增加 token 认证」**完全未实施**。
- `src/debug_server.rs:14-16` — 默认绑定 `127.0.0.1`，端口 `37878-37977`，逐个试探直到成功（`debug_server.rs:213-224`）。
- `src/debug_server.rs:311-406` — `handle_debug_request` 只按 `(method, path)` 分派，**没有 token、没有 Origin/Host 校验、没有 CSRF 防护**。
- `src/debug_server.rs:348-373` — `POST /debug/input` 把请求体原始字节直接 `write_to_pty`，**不过滤控制字符、不过滤 ESC、不做长度限制**。写入 `\n` 即等于在用户 shell 中按回车执行任意命令。
- `src/debug_server.rs:374-403` — `POST /debug/replace-line` 先注入 `0x15`（Ctrl-U）再写入 body，同样无过滤。

风险面比「仅本机」更宽：

1. **同机任意进程**（包括无特权的沙箱内应用、其他用户 session 内的进程）只需扫 100 个端口就能拿到 shell 执行权。
2. **浏览器 CSRF / DNS rebinding**：`Content-Type: text/plain` 属于 CORS safelisted request，任意网页用 `fetch(..., {mode:'no-cors'})` 就能把 POST 打到 `127.0.0.1:37878..37977`。响应读不到无所谓 —— **注入已经发生**。端口空间只有 100 个，足以在一次页面访问内穷举。
3. `GET /debug/screen`（`debug_server.rs:328-331`）与 `GET /debug/state`（含屏幕全文，`debug_server.rs:158-176`）泄露终端可见内容：密钥、token、`git` 输出、SSH 会话内容。同机任意进程可读。

建议修复优先级最高的三点：默认关闭（`--debug-http` 显式开启）→ 随机 token（启动时打印/写入 `0600` 文件，写接口必须携带）→ 校验 `Host`/`Origin` 头拒绝非 `127.0.0.1:<port>` 的 Host（防 DNS rebinding）。

### 1.2 高（High）

**S-2　`AGENT_TUI_DEBUG_ADDR` 可把调试接口绑定到任意地址，包括 `0.0.0.0`**

- `src/debug_server.rs:194-200` — 环境变量值不做任何校验就传给 `Server::http(&addr)`。设置 `AGENT_TUI_DEBUG_ADDR=0.0.0.0:8080` 即把「任意命令执行」接口暴露到局域网/公网。README 第 126 行还把这个变量作为正常用法记录，没有任何风险提示。
- 建议：仅允许 loopback，非 loopback 地址必须额外 `--debug-http-allow-remote` + token。

**S-3　调试接口的写入路径绕过主线程与 shadow input-line 模型**

- HTTP 线程（`debug_server.rs:262-275` 起的独立线程）通过 `src/pty.rs:86-93` `write_to_pty` 直接持锁写 PTY，不经过 GPUI 主线程、不更新 `input_line`/`input_cursor_utf16`。
- 后果：一是可以在主线程处理按键的间隙插入字节，产生交错的半个转义序列；二是注入后 AX 暴露的 shadow model 与真实 shell 状态不一致，而 AX 模型正是本项目对外的「可信输入行」—— 外部辅助工具会读到错的行内容。
- 读方向则相反：`/debug/state` 与 `/debug/screen` 读的是 `SharedDebugState` 里的快照，只在 `src/terminal.rs:842-847`（`refresh_snapshot` 内）更新，即**依赖渲染/刷新时机**，在无输入的空闲帧后读取会拿到过期屏幕。

### 1.3 中（Medium）

**S-4　调试服务器每个 tab 泄漏一个，且无法关闭**

- `src/terminal.rs:429` 每建一个 tab 就 spawn 一个新 HTTP server（`debug_server.rs:209-232`），端口顺序推进（`NEXT_DEBUG_HTTP_PORT`）。`serve_debug_http` 里 `for request in server.incoming_requests()` 是无终止循环（`debug_server.rs:288`），tiny_http 没有 shutdown 通道，**tab 关闭后线程与端口不释放**，且该线程仍持有已关闭 tab 的 `Arc<Mutex<writer>>`，导致 PTY writer 无法析构，注入仍会写向已「关闭」的会话。
- `docs/IMPROVEMENT_PLAN.md` 阶段 3 正是这件事，状态标注为「待开始」，与代码一致。
- 附带：开 100 个 tab 后端口耗尽，只能 `set_error`（`debug_server.rs:226-231`），静默失败，不可见。

**S-5　请求体无大小限制**

- `debug_server.rs:353-357` / `380-387` 用 `read_to_end` / `read_to_string` 读取完整 body，没有上限。单线程串行的请求循环（`debug_server.rs:288-298`）意味着一个慢速大 body 请求即可长时间阻塞调试接口（对同一 tab 也阻塞其他 debug 请求）。`research/api-system-plan.md` 的设计原则里写了 "bounded payload size"，但未落地。

**S-6　输入日志会把用户键入内容写入磁盘（隐私）**

- 默认**关闭**：仅当传 `--input-log-file <path>`（`src/cli.rs:34`）时才创建 `InputLogger`（`src/terminal.rs:430-451`）。这一点是好的。
- 但开启后：
  - `--input-log-raw`（`src/cli.rs:36`）→ `input_log.rs:63-69` 走 `json!(text)`，**逐字记录完整明文**，包括 IME 提交文本、粘贴内容、拖入的文件路径（`input.rs:253/299/1744` 等 20+ 处调用点）。密码在 shell 里通常不回显、也不进 shadow model，但 `sudo` 提示后的键入、以及任何贴到命令行的 token 都会被记录。
  - 非 raw 模式并不等于脱敏：`text_utils.rs:65-76` `summarize_text_for_trace` 保留**前 24 个字符**原文（超出才加 `…`）。24 字符足以泄露大部分 API key 前缀、路径、命令。
  - 文件用 `OpenOptions::new().create(true).append(true)`（`input_log.rs:27`），**未设置 0600 权限**，默认受 umask 影响（通常 0644，同机其他用户可读）；也没有大小上限/轮转，长期追加无界增长。
- 建议：raw 模式加显式二次确认或警告输出；创建文件时用 `OpenOptionsExt::mode(0o600)`；README 的 `--input-log-raw` 说明处补隐私警示。

**S-7　`AGENT_TUI_INPUT_TRACE` 把输入内容打到 stderr**

- `src/terminal.rs:1513-1520` + `src/input.rs:1082-1086` — 打开后 `eprintln!` 输出含 `summarize_text_for_trace` 的按键/输入行摘要。若 App 由带日志收集的父进程启动（或 `.app` 的 stderr 被系统日志捕获），键入内容可能进入系统日志。默认关闭。

### 1.4 低（Low）/ 观察项

- **粘贴防护覆盖面窄**：`src/input.rs:1425-1456` `evaluate_paste_risk` 仅在「≥4 行 且 ≥120 字符 且 非 ASCII 占比 ≥35%」时弹确认（常量在 `input.rs:30-32`）。**纯 ASCII 的多行恶意粘贴（最典型的 `curl … | sh` 攻击形态）不触发任何确认**，且 `write_paste_input`（`input.rs:260-279`）会把 `\n` 全部转成 `\r`，等于逐行回车执行。非 bracketed-paste 分支（`input.rs:276-278`）还**不做 ESC 剥离** —— bracketed 分支剥了（`input.rs:271-272`），非 bracketed 分支没剥。建议把行数阈值单独作为触发条件，与非 ASCII 比例解耦。
- **`macos_ax.rs` 把当前输入行发布为 `AXTextField`**（`macos_ax.rs:111-123`）。这是产品核心特性，但也意味着**任何拥有辅助功能权限的进程都能读走当前命令行并注入替换文本**（`should_accept_ax_override`，`text_utils.rs` + `input.rs:1116-1165`）。这是设计选择而非 bug，但 README 的「Known Limitations」里没有对应的安全说明，建议补一节「Threat model」。
- **`indexed_to_rgb` 的 fallback 分支**（`src/color.rs:104-119`）对索引 0-15 用 `&Default::default()` 取名义色。因为前面 `colors[index]` 已判空（`color.rs:99-101`），这里只在「终端未定义该槽位」时命中，行为正确，非缺陷（TODO 第 6 条可视为已解决）。

---

## 2. TODO.md 逐条状态表

| # | 条目 | 状态 | 代码证据 |
|---|---|---|---|
| 1 | 逐字符渲染 → 批量按行 shape | ❌ **未修复** | `src/render.rs:396` 仍在 per-cell 循环内 `shape_line`；`src/snapshot_tab.rs:429` 同样逐 cell shape。README:223 也承认此限制 |
| 2 | 模块拆分（单文件 2500+ 行） | ✅ **已完成** | `src/main.rs` 现仅 129 行；已拆为 `terminal.rs`(1970) `input.rs`(2057) `render.rs`(741) `keyboard.rs`(698) `tabs.rs`(597) `debug_server.rs`(582) `snapshot_tab.rs`(565) 及 `convenience/`、`sixel/` 子模块 |
| 3 | `AgentTerminal::new()` 消除重复 | ✅ **已完成** | `src/terminal.rs:387-411` 改为 `match` 返回 6 元组，随后 `terminal.rs:454+` 单一 `Self { .. }` 构造，Ok/Err 分支不再各自建实例 |
| 4 | 宽字符光标定位（`rewrite_terminal_input_line`） | ⚠️ **未修复，且原诊断不准确** | `src/input.rs:434-447` 仍按 **UTF-16 单元数**发 `\x1b[D`。readline 的左箭头是按字符移动而非按列移动，所以 CJK（1 utf16 unit）其实正确；真正错位的是**非 BMP 字符（emoji、部分生僻字）**——2 个 utf16 单元 → 多发一次左箭头。应按 `chars().count()` 而非 utf16 长度 |
| 5 | HTTP 调试接口安全加固 | ❌ **未修复（现为本报告 S-1/S-2）** | `src/terminal.rs:429` 无条件启动；`debug_server.rs:311-406` 无认证；`/debug/input` 仍可注入任意字节 |
| 6 | `indexed_to_rgb` 传入正确 colors | ✅ **已完成** | `src/color.rs:98-101` 签名已带 `colors: &Colors` 且优先查表；调用点 `color.rs:39` |
| 7 | `measure_cell_width` 缓存 | ⚠️ **部分完成** | `AgentTerminal.cell_width` 字段已存在（`terminal.rs:300`），但只在 `sync_grid_to_window`（`terminal.rs:864`）更新并用于 PTY 像素尺寸；渲染热路径仍每帧重新测量（`render.rs:307-311`、`render.rs:86-87`、`snapshot_tab.rs:344-349`） |
| 8 | PTY reader 线程错误记录 | ❌ **未修复** | `src/pty.rs:71` 仍为 `Err(_) => break`，无 eprintln、无 channel 通知 |
| 9 | Cmd+C 复制支持 | ⚠️ **部分完成** | 终端 tab 采用「松开鼠标即自动复制选区」（`input.rs:712-718` → `copy_current_selection_to_clipboard`，`input.rs:1208`），**没有 Cmd+C 键绑定**（`keyboard.rs` 只有 `is_paste_shortcut:495`、`is_select_all_shortcut:503`，无 copy）。Snapshot tab 反而实现了 Cmd+C（`snapshot_tab.rs:186-199`） |
| 10 | 滚动回看历史 | ⚠️ **绕道实现** | `input.rs:547-576` 的滚轮只做 mouse-report 与 alt-screen 转发，**主屏不支持 display_offset 滚动**；回看能力改由 Cmd+Shift+S 生成的只读 snapshot tab 提供（`main.rs:74`、`tabs.rs:148-160`、`terminal.rs:1004`）。README:224 仍列为已知限制 |
| 11 | `input_line` 与 shell 状态同步 | ⚠️ **改善但未解决** | `input.rs:399-432` 已覆盖 Ctrl-A/E/B/F/C/D/K/W/U、方向键、Home/End、Delete、Alt-Backspace/Alt-D；**仍未处理 Up/Down 历史导航与 Tab 补全**（`_ =>` 分支对以 `0x1b` 开头的未知序列直接 `return`）。另有 AX 双向纠偏（`input.rs:1116-1165`）作为兜底。README:222 承认此漂移 |

汇总：**已完成 3 条（2/3/6）；部分完成 4 条（7/9/10/11）；未修复 4 条（1/4/5/8）**。TODO.md 本身的行号引用（全部指向 `src/main.rs:xxx`）在模块拆分后**已全部失效**，文件本身需要重写。

---

## 3. 文档 / 资料盘点

### 3.1 `docs/`

| 文件 | 行数 | 内容 | 时效性 |
|---|---|---|---|
| `IMPROVEMENT_PLAN.md` | 35 | 四阶段架构改进计划：阶段 0（clippy 门槛）/1（tab 焦点状态机）/2（重复代码收敛）标注已完成，**阶段 3「Debug Server 生命周期治理」标注待开始** | 阶段 3 与代码一致（确实未做，见 S-4）；但「验证记录」停在 2026-03-13 且写「15 tests」，当前已 91 tests，记录严重滞后。**另：阶段 0 声称的 clippy 门槛当前实际是红的（见 §7）** |
| `cjk_font_fallback_task.md` | 90 | CJK 字体 fallback 任务日志：定位到 GPUI `open_type.rs` 的 `append_system_fallbacks` 迭代器从未被消费的 bug，已提上游 issue #57916，并重写 `src/font_fallback.rs` 为「只发现不覆盖 CJK 的符号字体」 | **内容仍有效且关键**，但**该文件当前未被 git 跟踪（untracked）**。文中记录的 GPUI patch 打在 `~/.cargo/git/checkouts/...` 里，`cargo clean -p gpui_macos` 就会丢失 —— 这是全项目最大的可复现性隐患。文中「旧实例占用 127.0.0.1:7878」与现行端口范围 37878-37977 不符，说明写于端口改动之前 |
| `ime_focus_recovery_memo.md` | 111 | 豆包输入法切回前台后按键不进终端的排障备忘（2026-07-04）：根因是只刷新 GPUI 焦点未同步窗口级 `NSTextInputContext`；含「三层输入状态」经验总结与后续检查顺序 | 有效，对应提交 `0de5d1b Sync IME context on terminal focus` |
| `input_method_cursor_indicator_memo.md` | 123 | 输入法状态光标颜色设计备忘（2026-07-04）：明确「不让输入法状态污染终端核心」，引入 `convenience` 边界 | 有效，对应 `3c3511e Add input method cursor indicator` 与 `src/convenience/` |
| `terminal_graphics_and_atuin_memo.md` | 240 | SIXEL 内联图片渲染（布局占位、滚动跟随、PTY 像素尺寸）与 Atuin 无颜色问题；结论是 `NO_COLOR=1` 从父进程继承 | 有效，对应 `pty.rs:40` 的 `command.env_remove("NO_COLOR")` 与 `src/sixel/` |

### 3.2 `research/`

| 文件 | 行数 | 内容 | 时效性 |
|---|---|---|---|
| `terminal-implementation-research.md` | 1323 | libghostty / libghostty-vt 可嵌入性调研、Zed 终端层实现模式、alacritty_terminal 复用方案。README 的架构选型直接引用它 | 作为背景资料有效；开头引用的克隆路径 `/Users/oker/Works/agent-tui/research/ghostty` **已不存在**（`.gitignore` 忽略 `research/ghostty/`，本机也无该目录），路径引用过时 |
| `api-system-plan.md` | 342 | 在现有 debug server 之上设计 `/api/v1/*` 稳定 API：统一 JSON envelope、read/write 分离、`head/tail/with_control`、key-event 端点、分 4 阶段落地，Phase 4 才做 "optional token auth" | **纯计划，零落地**（`debug_server.rs` 无任何 `/api/v1` 路径）。文档第 3 节自称 "Safe defaults: loopback bind by default / bounded payload size"，但现状连这两条都未满足。第 11 节仍把「写 API 是否默认要 token」列为 open decision —— 建议直接定为「默认要」 |

### 3.3 根目录文档

- `README.md`（234 行）：质量最高的一份，含架构图、组件行数表、功能列表、CLI 选项表、Kitty 键盘协议说明、技术栈、构建、已知限制。**问题**：
  - (a) 组件行数表已偏离实际（写 `input.rs ~1600`，实际 2057；`terminal.rs ~1100`，实际 1970；`snapshot_tab.rs ~540`，实际 565）；
  - (b) 第 120-126 行把 debug HTTP 和 `AGENT_TUI_DEBUG_ADDR` 当普通功能介绍，**无任何安全警示**；
  - (c) 未提 `--input-log-raw` 的隐私影响；
  - (d) 第 224 行「No scrollback UI」已被 snapshot tab 部分解决，未更新。
- `AGENTS.md`（7 行）：本地协作规则 ——「编译通过前不得声称完成」、内部文档用中文 / README 保持英文。简短有效。
- `TODO.md`（60 行）：见第 2 节，行号引用全部失效，需按模块重写。

### 3.4 `scripts/`

只有 `build-macos-app.sh`（100 行）。质量尚可：`set -euo pipefail`、从 `Cargo.toml` 抽 version、生成 `.app` bundle + `Info.plist`（`LSMinimumSystemVersion 13.0`、`NSHighResolutionCapable`）。做法是把真实二进制改名为 `agent_terminal-bin`，再写一个 shell wrapper 设置 `TERM`/`COLORTERM`/`TERM_PROGRAM`/`LANG`/`LC_CTYPE` 后 `exec`。

问题：
- `TERM_PROGRAM=agent_terminal`（wrapper 内）与 `src/pty.rs:39` 的 `TERM_PROGRAM=rterminal` **不一致**，下游工具的探测会因启动方式不同而拿到不同值。
- 无代码签名 / 公证 / `codesign --entitlements` 步骤。本项目需要辅助功能权限（AX），未签名 App 的 TCC 授权在每次重新构建后会失效，实际使用会反复掉权限。
- `dist/` 是脚本输出目录，却没有被 `.gitignore` 忽略（见第 6 节）。
- 没有任何 test/lint/release 脚本；`IMPROVEMENT_PLAN.md` 提到的质量门槛 `cargo clippy --all-targets -- -D warnings` 只存在于文档，没有脚本或 CI 固化。

---

## 4. 测试覆盖现状

### 4.1 主 crate

`#[cfg(test)]` 模块 **15 个**（覆盖 `src/` 下 15 个文件），`#[test]` 函数 **91 个**，`cargo test` 全绿（91 passed, 0 failed, 0.74s）。

| 文件 | 测试数 |
|---|---:|
| `src/input.rs` | 17 |
| `src/cli.rs` | 13 |
| `src/terminal.rs` | 13 |
| `src/keyboard.rs` | 11 |
| `src/tabs.rs` | 7 |
| `src/sixel/images.rs` | 5 |
| `src/font_fallback.rs` | 4 |
| `src/render.rs` | 4 |
| `src/convenience/input_method.rs` | 4 |
| `src/debug_server.rs` | 3 |
| `src/convenience/cursor_indicator.rs` | 3 |
| `src/text_utils.rs` | 3 |
| `src/sixel/parser.rs` | 2 |
| `src/pty.rs` | 1 |
| `src/sixel/pixel.rs` | 1 |

结构性观察：

- **全部是同文件内联单元测试，没有 `tests/` 集成测试目录**，也没有 `benches/`。
- `debug_server.rs:428-535` 的 3 个测试质量不错（真实起 tiny_http、真发 HTTP 报文、断言字节落到 mock writer），但**只覆盖 happy path**：没有 404、没有空 body、没有超大 body、没有并发、没有多 tab 端口推进（正是 `IMPROVEMENT_PLAN.md` 阶段 3 要求的回归点）。
- 测试倾向集中在**纯函数**（键位编码、UTF-16 换算、选区归一化、字体覆盖打分、SIXEL 解析）。涉及 GPUI `Window`/`Context` 的渲染与焦点路径基本无法测，`render.rs` 的 4 个测试只覆盖辅助计算函数。
- `cli.rs` 的 13 个测试全是「flag 能不能解析」，价值偏低但成本也低。
- `src/color.rs`、`src/macos_ax.rs`、`src/snapshot_tab.rs`、`src/input_log.rs`、`src/main.rs` **零测试**。其中 `snapshot_tab.rs` 有 `extract_selection_text` / `normalize_selection_col` / `row_text_without_wide_spacers` 三个可测纯函数（`snapshot_tab.rs:492-565`，且与 `input.rs` 里的同名逻辑重复实现），`input_log.rs` 的 JSONL 序列化也完全可测。

### 4.2 vendored `alacritty_terminal`

- 在工作区根目录 `cargo test -p alacritty_terminal` **会失败**：`package alacritty_terminal cannot be tested because it requires dev-dependencies and is not a member of the workspace`（path 依赖但未加入 `[workspace] members`）。
- 在 `vendor/alacritty_terminal/` 目录内 `cargo test` **完全可跑且全绿**：unittests 135 passed、`tests/ref.rs` 45 passed、doc-test 1 passed。
- 代价：会在 `vendor/alacritty_terminal/` 生成独立的 `Cargo.lock`（未被 `.gitignore` 覆盖）和独立 `target/`（`target/` 模式恰好在任意深度生效，所以这个被忽略了）。
- 结论：上游测试仍然可用，但**当前工程配置下不会被跑到** —— `cargo test` 只跑主 crate 的 91 个。sixel 这类 vendored 改动没有任何上游测试保护。

---

## 5. Vendor 与依赖策略风险

### 5.1 `vendor/alacritty_terminal` 相对上游的改动

版本 `0.25.1`（`vendor/alacritty_terminal/Cargo.toml:3`），`edition 2024`，`rust-version 1.85.0`。引入历史只有两次提交：

1. `a7a071d feat: add eye-care theme and improve selection UX`（2026-03-14）—— 整棵 vendored 树的引入提交，同时改了 `Cargo.toml` 把依赖切到 path。
2. `9084d96 Add sixel rendering and terminal color fixes`（2026-06-15）—— **唯一的实质性 fork 改动**，共 4 文件 +481/-3：
   - `src/sixel.rs` **新增 420 行**（全新文件，上游 alacritty_terminal 0.25.1 无 SIXEL 支持）
   - `src/lib.rs:12` 新增 `pub mod sixel;`
   - `src/event.rs` +24 行（新增 SIXEL 相关事件）
   - `src/term/mod.rs` +39/-3（接入 SIXEL 处理与颜色修正）

即：**fork 面很小且集中** —— 只有一处新增模块 + 三处接缝。但这些改动**没有任何测试**（上游 135 个测试不覆盖新增的 `sixel.rs`；主 crate 侧的 SIXEL 测试在 `src/sixel/parser.rs`（2 个）和 `src/sixel/images.rs`（5 个），测的是应用层解析而非 vendored 侧的 term 接缝），且 vendored 树里没有任何 README/patch 说明改了什么、为什么改、如何 rebase 到新上游。

### 5.2 维护风险清单

| 风险 | 说明 |
|---|---|
| **无 upstream 追踪基线** | vendored 树没记录对应的上游 commit/tag，只有 `version = 0.25.1`。将来要升级 alacritty_terminal，得手工 diff 出上面 4 个文件的改动再重新应用。建议在 `vendor/alacritty_terminal/` 放一个 `VENDOR.md`（上游 rev + 改动清单 + rebase 步骤），或改用 `[patch]` + 独立 fork 仓库 |
| **vendored 测试不在 CI 路径上** | 见 4.2。建议把 vendored crate 加入 `[workspace] members`，让 `cargo test` 一次跑到全部 271 个测试 |
| **GPUI 依赖锁在 git rev，且有 cargo cache 内的手工 patch** | `Cargo.toml:13-14` 把 `gpui` / `gpui_platform` 锁在 zed `rev = 19c8363a8e0d8a2f7a7181bab2c14d87390c0f25`（`default-features = false`）。而 `docs/cjk_font_fallback_task.md` 记录了一处**必须存在、但只存在于 `~/.cargo/git/checkouts/` 里的 `gpui_macos/src/open_type.rs` 手改**。这意味着：换机器、`cargo clean -p gpui_macos`、或 CI 全新 checkout，**中文字体渲染都会静默回退到错误行为**，而且没有任何测试或构建期检查能发现。这是当前最严重的可复现性风险，优先级应高于多数功能条目。修法：把 patch 沉淀为 `[patch."https://github.com/zed-industries/zed.git"]` 指向自己的 zed fork 分支，或 vendor `gpui_macos` |
| **git rev 锁定的常规代价** | 无语义化版本、无 CHANGELOG、无法用 `cargo update` 获得安全修复；zed 一旦 force-push 或 GC，该 rev 可能不可获取。同时 `gpui` 上游 API 变动频繁（本项目大量使用 `window.text_system()`、`canvas`、`InputHandler` 等相对内部的 API），升级成本会随时间线性上升 |
| **`rust-toolchain.toml` 固定 `channel = "1.95.0"`** | 只锁了 channel，**没有 `components`（rustfmt/clippy）、没有 `profile`、没有 `targets`**。`IMPROVEMENT_PLAN.md` 把 clippy 定为门槛，却没在 toolchain 文件里声明 clippy 组件，新环境上 `cargo clippy` 可能直接不可用。另：主 crate 用 `edition 2024`，vendored crate 声明 `rust-version 1.85.0`，1.95.0 满足，无冲突 |
| **无 CI** | 仓库中无 `.github/`、无任何 CI 配置。所有质量门槛（build / test / clippy / self-check）都靠人工与 `AGENTS.md` 里的约定执行 |
| **License 合规** | 根目录**无 LICENSE 文件**，README 写 "currently private and unlicensed"；但 `vendor/alacritty_terminal/LICENSE-APACHE` 存在，且构建产物静态链接了 Apache-2.0 代码。一旦对外分发 `.app`，需在根目录保留第三方许可声明（Apache-2.0 要求保留 NOTICE/许可副本） |

---

## 6. `.gitignore` 与仓库卫生

`.gitignore` 当前**只有两行**：

```
target/
research/ghostty/
```

`git status --short` 实测未忽略项：

| 路径 | 状态 | 说明 |
|---|---|---|
| `dist/` | ❌ **未忽略** | `scripts/build-macos-app.sh` 的输出目录，内含 `dist/Agent Terminal.app`（完整 release 二进制，数十 MB）。必须加 `dist/` |
| `.DS_Store` | ❌ **未忽略** | macOS Finder 元数据。必须加 `.DS_Store`（建议 `**/.DS_Store`） |
| `docs/cjk_font_fallback_task.md` | ⚠️ **未跟踪但应提交** | 不是该忽略的垃圾，而是**遗漏提交**的重要文档（含 GPUI patch 的唯一说明，见 5.2）。应尽快 `git add` |
| `vendor/alacritty_terminal/Cargo.lock` | ⚠️ 会在 vendored 目录内跑 `cargo test` 时生成 | 建议加 `vendor/*/Cargo.lock`（或把 vendored crate 纳入 workspace，从根本上不再生成） |
| `target/`、`vendor/alacritty_terminal/target/` | ✅ 已忽略 | `target/` 模式在任意深度生效 |

建议的最小 `.gitignore` 补丁：

```
target/
dist/
.DS_Store
**/.DS_Store
vendor/*/Cargo.lock
research/ghostty/
```

---

## 7. 编译与静态检查现状（审查时实测）

- `cargo check --release`：**通过**。
- `cargo test`：**91 passed, 0 failed**。
- `cargo clippy --release`：**失败，3 个 error + 3 个 warning**。
  - 3 个 error 全部来自同一处死逻辑：`src/keyboard.rs:195` 与 `:198` 的 `modifiers.control || modifiers.alt || (modifiers.shift && modifiers.alt)` —— 第三项被第二项完全吸收（`clippy::overly_complex_bool_expr`，默认 deny）。行为上无 bug（表达式结果等价），但**说明作者原本可能想表达别的条件**（如 `shift && !alt`），值得回看意图。
  - warnings：`terminal.rs:1451` needless_range_loop、一处 collapsible_if、一处 manual `!Range::contains`。
  - **重要含义**：`docs/IMPROVEMENT_PLAN.md` 阶段 0 声称已建立 `cargo clippy --all-targets -- -D warnings` 质量门槛并标记「已完成」，但该门槛当前实际是**红的**。文档与现实不一致，且因为没有 CI，无人会发现。
- 依赖 future-incompat 警告：`block v0.1.6`（objc 生态传递依赖）**将被未来 Rust 版本拒绝编译**。目前被 `rust-toolchain.toml` 的 1.95.0 固定所掩盖，属定时炸弹。

---

## 8. 工程化改进建议（按优先级）

### P0（安全 / 可复现性，应立刻做）

1. **调试接口默认关闭 + token + Host 校验**（S-1/S-2）。最小改动：`cli.rs` 加 `--debug-http`（默认 false）与 `--debug-http-token`；`terminal.rs:429` 改为条件调用；`debug_server.rs:311` 在所有 `POST` 分支前统一校验 token 与 `Host` 头；`AGENT_TUI_DEBUG_ADDR` 非 loopback 时拒绝或要求额外 flag。这一条同时解决 TODO 第 5 条，是全项目投入产出比最高的修复。
2. **沉淀 GPUI patch**（5.2 第三行）。改用 `[patch."https://github.com/zed-industries/zed.git"] gpui_macos = { git = "<自己的 fork>", branch = "cjk-fallback" }`，并在 README/AGENTS 层面注明。目前「正确的中文渲染」依赖一个不在版本控制内的本地文件修改，任何环境迁移都会静默退化。
3. **提交 `docs/cjk_font_fallback_task.md`**，并修好 `.gitignore`（第 6 节补丁）。

### P1（近期）

4. **落地 debug server 单实例 + 会话路由**（S-4 / `IMPROVEMENT_PLAN.md` 阶段 3）。改为进程级单实例、路径带 tab id（如 `/debug/tabs/{id}/input`），tab 销毁时注销会话；顺带加请求体大小上限（S-5）与 404/边界测试。
5. **`input_log` 隐私加固**（S-6）：创建文件时 `mode(0o600)`；`--input-log-raw` 时向 stderr 打印一次明确警告；README 补隐私说明。
6. **修 TODO 第 8 条**（`pty.rs:71` 记录 reader 错误）—— 三行改动，当前 PTY 异常断开完全静默。
7. **修 TODO 第 4 条**：`input.rs:434-447` 的左移次数改为按字符数（`chars().count()`），修正非 BMP 字符错位。
8. **补 Cmd+C 键绑定**（TODO 9）：`keyboard.rs` 加 `is_copy_shortcut`，终端 tab 与 snapshot tab 共用；顺带把 `snapshot_tab.rs:492-565` 与 `input.rs` 中重复的选区文本提取逻辑合并到一处并加测试。
9. **加 CI**（GitHub Actions，macOS runner）：`cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test` + `cargo run -- --self-check`。`IMPROVEMENT_PLAN.md` 阶段 0 已把这些定为门槛，只是没有自动化 —— 且当前 clippy 是红的，必须先修（§7）。同时在 `rust-toolchain.toml` 补 `components = ["clippy", "rustfmt"]`。
10. **把 vendored crate 加入 workspace**，使 `cargo test` 一次覆盖 271 个测试（4.2）。

### P2（持续）

11. **重写 TODO.md**：现有行号全部指向已不存在的 `src/main.rs` 布局；建议按模块归档，并把已完成项移入 CHANGELOG 或直接删除。
12. **同步 README 的组件行数表与「已知限制」**，补一节 "Security / Threat model"，明确说明 debug HTTP 与 AX 暴露面（1.4 观察项）。
13. **给 vendored fork 写 `VENDOR.md`**：上游 rev、4 个文件的改动摘要、rebase 步骤；并为 `vendor/.../sixel.rs` 的接缝补几个上游侧测试。
14. **粘贴防护解耦**（1.4）：把「多行」单独作为确认触发条件，不再要求非 ASCII 占比；非 bracketed-paste 分支同样剥离 ESC。
15. **渲染性能**（TODO 1 / 7）：per-cell `shape_line` 改为按行/按样式段批量 shape，并把 `cell_width` 的每帧测量替换为缓存字段（`terminal.rs:300` 已有字段，渲染路径未用）。
16. **构建脚本**：统一 `TERM_PROGRAM`（`scripts/build-macos-app.sh` wrapper 与 `pty.rs:39` 现在不一致），加入 `codesign` 步骤以保住 AX 权限授权。

---

**一句话总结**：代码组织与测试纪律（91 个单元测试、模块拆分、详实的中文排障备忘）明显优于同规模个人项目；但**默认开启且无认证的 `/debug/input` 构成本地任意命令执行（可被普通网页 CSRF 触发）**，以及**中文渲染依赖一处不在版本控制内的 cargo cache patch**，是两个必须优先处理的结构性问题；此外 `.gitignore` 漏了 `dist/` 与 `.DS_Store`，`docs/cjk_font_fallback_task.md` 这份关键文档还未提交，而文档声称已建立的 clippy 门槛当前实际是红的。
