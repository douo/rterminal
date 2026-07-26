# 缺陷台账（统一编号 / 已去重）

> 综合产物 · 审查基线 `main @ c4301f9` · 由四份子报告合并去重而成
> 编号规则：`SEC` 安全 · `COR` 输入正确性 · `DSP` 显示正确性 · `ROB` 鲁棒性与资源 · `PERF` 性能 · `ARCH` 结构 · `ENG` 工程化
> 「来源」列指向子报告中的原始条目，便于回查完整分析与修复建议。

## 分级定义

| 级别 | 判据 |
|---|---|
| **S0 严重** | 违反终端核心契约：执行用户未键入的内容、丢失用户输入、进程失去响应，或使项目产出不可复现 |
| **S1 高** | 数据丢失或安全暴露面扩大，触发条件明确 |
| **S2 中** | 功能性错误，用户可感知；或资源泄漏随使用时长累积 |
| **S3 低** | 局部不一致、非默认配置下的问题、规范偏差、内部质量 |

---

## S0 严重（4 项）

| ID | 缺陷 | 位置 | 来源 |
|---|---|---|---|
| **SEC-1** | debug HTTP 接口默认无条件启动、无认证；`POST /debug/input` 把请求体原始字节直写 PTY，含 `\n` 即在用户 shell 执行任意命令。因 `Content-Type: text/plain` 属 CORS safelisted，**任意网页可用 `fetch(mode:'no-cors')` 穷举 37878-37977 这 100 个端口触发注入**；同机任意进程亦可。`GET /debug/state`、`/debug/screen` 泄露屏幕全文（密钥、token、SSH 内容） | `terminal.rs:429`（无条件调用）<br>`debug_server.rs:14-16, 311-406, 348-373, 374-403, 158-176` | 04 §S-1 |
| **COR-1** | Ctrl+非字母键统一做 `to_ascii_lowercase() & 0x1f`，对标点/数字产生完全错误的控制字节：**`Ctrl+-` → `0x0d`（CR，直接执行当前命令行）**、`Ctrl+3` → `0x13`（XOFF，冻结输出）、`Ctrl+2` → `0x12`（^R）、`Ctrl+/` → `0x0f`、`Ctrl+;` → `0x1b`（ESC） | `keyboard.rs:420-427` | 02 §H1 |
| **ROB-1** | SIXEL 前置 DCS 分流器对 `payload`/`raw` 无长度上限，且不处理 CAN(0x18)/SUB(0x1a) 中止。`printf '\x1bP'; cat 大文件` 后**整个会话输出被静默吞入双份 Vec，屏幕停止更新、内存无界增长、无恢复手段** | `sixel/parser.rs:81-145` | 03 §H1 |
| **ENG-1** | 正确的中文字体渲染依赖一处**只存在于 `~/.cargo/git/checkouts/` 的 gpui_macos `open_type.rs` 手工修改**（`append_system_fallbacks` 迭代器未被消费）。换机器、`cargo clean -p gpui_macos`、CI 全新 checkout 都会**静默退化**，且无任何测试能发现 | `Cargo.toml:13-14`<br>`docs/cjk_font_fallback_task.md` | 04 §5.2 · 03 §L11 |

---

## S1 高（3 项）

| ID | 缺陷 | 位置 | 来源 |
|---|---|---|---|
| **SEC-2** | `AGENT_TUI_DEBUG_ADDR` 不做校验直传 `Server::http`，设为 `0.0.0.0:8080` 即把 SEC-1 的任意命令执行暴露到局域网/公网。README:126 还把它当普通功能介绍，无风险提示 | `debug_server.rs:194-200` | 04 §S-2 |
| **COR-2** | `unmarkText` 直接丢弃 `ime_marked_text` 且从不写 PTY，违反 NSTextInputClient「作为正常文本插入」的语义。凡 IME 以 unmarkText 收尾的提交路径（切换输入源、系统强制结束组合）**用户已敲的整段拼音/汉字静默丢失** | `input.rs:1720-1725` | 02 §H2 |
| **DSP-1** | `ingest_batch` 逐 chunk 处理但 `process_pending_terminal_events` 只在批末调用一次，导致**图片入库之前**产生的 Scroll 增量也被施加到新图片上。`seq 30; img2sixel foo.png` 合并到同一批时图片被额外上移 N 行甚至判定滚出而丢弃 | `terminal.rs:590-627, 633-663`<br>`sixel/images.rs:55-83` | 03 §H2 |
| **SEC-3** | debug HTTP 线程绕过 GPUI 主线程直接持锁写 PTY，既可在按键处理间隙插入字节产生交错的半个转义序列，又不更新 `input_line`/`input_cursor_utf16` —— 而该影子模型正是本项目对外暴露的「可信输入行」，外部辅助工具会读到错值。读方向反之：`/debug/state` 依赖 `refresh_snapshot` 时机，空闲帧后读到过期屏幕 | `debug_server.rs:262-275`<br>`pty.rs:86-93` · `terminal.rs:842-847` | 04 §S-3 |

---

## S2 中（22 项）

### 安全 / 资源

| ID | 缺陷 | 位置 | 来源 |
|---|---|---|---|
| **SEC-4** | 每个 tab spawn 一个 HTTP server，`incoming_requests()` 无终止循环、tiny_http 无 shutdown 通道 → **tab 关闭后线程、端口、`Arc<Mutex<writer>>` 全部泄漏**，注入仍会写向"已关闭"的会话；开满 100 tab 后端口耗尽仅静默 `set_error` | `terminal.rs:429`<br>`debug_server.rs:209-232, 288` | 04 §S-4 · 03 §M9 |
| **SEC-5** | 请求体 `read_to_end`/`read_to_string` 无上限，单线程串行请求循环下一个慢速大 body 即可长时间阻塞调试接口 | `debug_server.rs:353-357, 380-387, 288-298` | 04 §S-5 |
| **SEC-6** | `--input-log-raw` 逐字记录完整明文（IME 提交、粘贴内容、拖入路径）；非 raw 模式也保留**前 24 字符原文**，足以泄露 API key 前缀。日志文件未设 `0600`（受 umask 影响通常 0644，同机他用户可读），无大小上限/轮转 | `input_log.rs:27, 63-69`<br>`text_utils.rs:65-76` | 04 §S-6 |
| **SEC-7** | 粘贴确认仅在「≥4 行 **且** ≥120 字符 **且** 非 ASCII ≥35%」时触发 —— **最典型的纯 ASCII 多行 `curl … \| sh` 粘贴不触发任何确认**，且 `\n` 全转 `\r` 等于逐行回车。非 bracketed-paste 分支还漏了 ESC 剥离（bracketed 分支有剥） | `input.rs:1425-1456, 30-32, 260-279` | 04 §1.4 |
| **ROB-2** | `write_to_pty` 在 UI 线程持锁做阻塞 `write_all + flush`。前台程序不读 stdin（Ctrl-S 流控、sleep 中）时粘贴超过内核 PTY 缓冲（16–64 KB）会**无限期挂死 UI 线程**；debug HTTP 线程共用同一把锁 | `pty.rs:86-93`<br>`terminal.rs:978-994` | 03 §M3 · 01 §1.2 |
| **ROB-3** | PTY reader `Err(_) => break`：错误无日志无上报；且 `Read::read` 不自动重试 `EINTR` —— 一次信号打断即让 reader 退出、channel 关闭，pump 把**仍存活的 shell 误判为 "shell exited"**，终端永久失去输出（TODO #8） | `pty.rs:71`<br>`terminal.rs:577-583` | 03 §M4 |
| **ROB-4** | 全仓无任何 `wait()`；`Drop` 只 `kill()`。shell 自行 `exit` 后 tab 仍开着、或反复开关 tab，**每个退出的 shell 都以僵尸进程挂在进程表**直到 app 退出 | `terminal.rs:1381-1387, 577-583` | 03 §M5 |
| **ROB-5** | PTY channel 为 `unbounded`，reader 线程永不感受背压；UI 每次只消费 ≤256 KB，`cat 大文件` / `yes` 洪泛时通道**无限堆内存** | `pty.rs:60`<br>`terminal.rs:57-58, 560-569` | 01 §1.2 |

### 输入正确性

| ID | 缺陷 | 位置 | 来源 |
|---|---|---|---|
| **COR-3** | SGR(1006) 鼠标 release 编码为 `\x1b[<{3+mods};x;y m`，丢失原按钮号（SGR 的设计正是 release 保留按钮号、仅用 `m` 区分）；`3+mods` 更是规范外组合。tmux/vim 无法区分释放了哪个键 | `input.rs:1322-1343` | 02 §M1 |
| **COR-4** | `rewrite_terminal_input_line` 按 **UTF-16 单元数**发 `\x1b[D`，而 zle/readline 的左箭头按**字符**移动。CJK（1 单元）恰好正确，**emoji/非 BMP（2 单元）每个多退一格**。⚠️ TODO.md #4 提出的「按 wcwidth 列宽发送」方案**方向是错的**，会把 CJK 多退一倍 | `input.rs:441-446` | 02 §M2 · 04 §2 |
| **COR-5** | IME 候选窗 `bounds_for_range` 按「每 UTF-16 单元一个半角格」推算 x 偏移，而渲染用 `shape_line` 真实字形宽度。日文假名、拼音已上屏汉字时候选窗横向偏移可达组合长度的一半 | `input.rs:1784-1798`<br>对比 `render.rs:480-497` | 02 §M3 |
| **COR-6** | 组合进行中 Cmd+V 粘贴 / 拖入文件时不处理悬挂的 marked text：粘贴文本立即写 PTY，渲染层继续画组合串并隐藏真实光标，提交后文本顺序与用户所见错乱。对照鼠标按下路径会主动 discard，粘贴/拖放路径遗漏 | `input.rs:920-983, 281-304`<br>对照 `input.rs:626` → `terminal.rs:889` | 02 §M4 |
| **COR-7** | `Ctrl+Alt+字母` 只发 `0x18`，丢失 xterm 传统的 `ESC` 前缀（应为 `\x1b\x18`）。Emacs `C-M-x`、readline `C-M-*` 全部失效并被误认为纯 Ctrl 键 | `keyboard.rs:100, 420-427` | 02 §M5 |
| **COR-8** | c4301f9 引入的 `FocusActivationMouseGuard` 仅靠同一表面收到 mouse-up 清除。激活点击后**拖到标题栏/窗外释放**则 guard 滞留，吞掉下一次正常点击的 release → shift 选区无法结束且 `selection_mode_active` 残留（之后无 shift 拖动会意外复活选区）；mouse-mode 应用收到 press 无 release 出现按钮卡死 | `input.rs:52-74, 695-710, 502-517` | 02 §M6 |
| **COR-9** | `--no-option-as-meta` 下死键（Option+E/I/N/U/`）：`should_defer_to_text_input` 因 `key_char` 为 None 判定失败 → 兜底发 `\x1b e`，同时 IME 组合继续并提交 `é`。**PTY 收到 ESC+e 加组合结果，双重输入且 ESC 可能触发 vi-mode** | `keyboard.rs:123-127, 444-473` | 02 §M7 |
| **COR-10** | `input_line` 影子模型仍与 shell 失同步（TODO #11）：↑/↓ 历史导航与 Tab 补全被早退忽略；Shift+Enter 发 `\n` 但模型只匹配 `\r` 故不清行；kitty CSI-u 编码的 Ctrl+C 被忽略；**新增入口**：多行粘贴把含 `\n` 整块写入模型而 shell 已逐行执行，alt-screen 中键入也持续写入。已有 AX 屏幕匹配防护降低误重写概率，但模型本身仍是脏的 | `input.rs:399-432, 958, 977`<br>缓解见 `input.rs:1248-1286, 1229-1242` | 02 §M8 · 04 §2 |
| **COR-11** | Cmd+A 被定义为"清空输入行"，无条件发 `0x15`（Ctrl-U），不检查 alt-screen。在 vim 插入模式中删除整行、在 less/TUI 中产生意外输入 | `input.rs:909-918` | 02 §M9 |

### 显示正确性

| ID | 缺陷 | 位置 | 来源 |
|---|---|---|---|
| **DSP-2** | 进出 alt screen 不清理 SIXEL 图片（`swap_alt` 不发 Erase，渲染侧也不消费已采集的 `snapshot.alt_screen`）：主屏图片浮在 vim/less/tmux 之上；alt-screen 内预览的图片退出后残留在主屏 | `vendor/…/term/mod.rs:721`<br>`render.rs:422-454` · `terminal.rs:734` | 03 §M1 |
| **DSP-3** | `apply_grid_size` 调 `term.resize` 触发行 reflow / 行进出 scrollback，但完全不触碰 `self.images`：resize 后图片与其预留空白区脱节、覆盖文字。且 resize 期间产生的事件要等下一次 PTY ingest 才 drain | `terminal.rs:951-976` | 03 §M2 |
| **DSP-4** | `mouse_grid_point` 用纯线性 `x / cell_width`，不考虑渲染侧为 `expands_layout` 宽字符累加的 `extra_visual_cols`。`--double-width-chars` 下 hover 高亮（正确）与实际点击命中（错误）指向不同单元，选区与鼠标上报坐标同样右偏；IME 光标框、Cmd+点击开链接同受影响 | `input.rs:810-869, 604-617, 449-473`<br>对比 `render.rs:54-70, 102-129` | 03 §M6 · 02 §L1 |

### 性能

| ID | 缺陷 | 位置 | 来源 |
|---|---|---|---|
| **PERF-1** | 渲染热路径三重浪费：**(a)** 每帧两次整屏 `snapshot.clone()` 深拷贝（每 cell 含 `Vec<char>` + `Option<String>`，300×80 窗口约 5 万次 cell 级堆克隆）；**(b)** 每个非空白 cell 一次 `cell.text()` String 分配 + 一次 `shape_line`（80×24 满屏 ≈ 2000 次/帧，TODO #1 未修）；**(c)** prepaint 与 paint 各自重新 `resolve_font + advance` 测宽，而 `self.cell_width` 字段已维护（TODO #7 只修一半）。snapshot_tab.rs 是 (b) 的复制体 | `render.rs:207, 246, 357-410, 86-87, 306-311`<br>`input.rs:816-822` · `snapshot_tab.rs:429` | 03 §M7 · 01 §4 |
| **PERF-2** | 首窗口创建路径上同步扫描并 `ttf_parser::Face::parse` **全部系统字体**（`OnceLock` 初始化）。字体多的 macOS 首启卡顿数百 ms | `font_fallback.rs:50-99`<br>`terminal.rs:371, 1523-1530` | 03 §M8 |
| **PERF-3** | 摄取侧每个 PTY batch 全量重建 snapshot（O(rows×cols) 含每 cell 分配），并附带整屏 URL 扫描与 debug 用整屏文本行重建 —— **即使没人连 debug server 也做**。高输出场景下这是新的热点 | `terminal.rs:730, 820, 842-847` | 01 §1.2 |

---

## S3 低（22 项）

### 显示 / 颜色

| ID | 缺陷 | 位置 | 来源 |
|---|---|---|---|
| **DSP-5** | BOLD+DIM 同时置位时落入 `_ => named` 返回原色；alacritty 按 DIM 处理。触发 `\e[1;2;31m` | `color.rs:48-57` | 03 §L1 |
| **DSP-6** | `AnsiColor::Indexed` 分支完全忽略 DIM（`Spec` 分支有处理），`\e[38;5;1m\e[2m` 不变暗，行为不一致 | `color.rs:39` | 03 §L2 |
| **DSP-7** | INVERSE 的前后景交换发生在 BOLD/DIM 变体应用**之前**，bright/dim 被施加到"原背景色"上。`\e[1;7;31m` 的反色块应为亮红背景，实际为普通红 | `terminal.rs:778-782, 1044-1048` | 03 §L3 |
| **DSP-8** | Block/Underline/HollowBlock 光标宽度未乘所在 cell 的 `width_cols`，停在 CJK/emoji 上只覆盖左半格 | `render.rs:512` | 03 §L4 |
| **DSP-9** | IME 组合文本与默认 run 颜色硬编码 `rgb(0xd7dae0)`，EyeCare 主题下与主题脱节 | `render.rs:296-303, 471-489` | 03 §L5 |
| **DSP-10** | `EL`（清行）对整行发 Erase 即使只清了光标右侧，且图片"任意相交即整幅删除"。提示符重绘、`clear -x` 会让图片瞬间消失 | `vendor/…/term/mod.rs:1683-1686`<br>`sixel/images.rs:187-198` | 03 §L7 |
| **DSP-11** | SIXEL 解码只有置位像素更新 `max_x/max_y`，空列（`?`）不计入。无 `"` 光栅声明且以透明列结尾的图片宽度偏窄，占列计算随之偏小 | `vendor/…/sixel.rs:156-165, 217-229` | 03 §L8 |
| **DSP-12** | DCS 载荷中裸 `0x9c` 立即终止 DCS，不区分它是否为 UTF-8 连续字节。SIXEL（纯 ASCII）不受影响，含 UTF-8 的未知 DCS / tmux passthrough 会被误切 | `sixel/parser.rs:101-103` | 03 §L9 |
| **DSP-13** | `measure_cell_width` 失败回退 `px(8.0)`，但渲染仍用系统回退字体的真实 advance 画字 → 网格/PTY winsize 与视觉错位。仅字体名完全无效时出现 | `render.rs:33-37` | 03 §L10 |
| **DSP-14** | `preferred_family_name` 取 `families[0]`，非英文 locale 下可能拿到本地化家族名，交给 GPUI/CoreText 解析存在失配风险 | `font_fallback.rs:119-125` | 03 §L11 |
| **DSP-15** | 光标灰化判据用 `window_active` 而非 `focused`，同窗口内焦点转移（如 tab 重命名框）时光标不变灰。当前 UI 下影响甚微 | `render.rs:236` | 02 §L5 |

### 输入

| ID | 缺陷 | 位置 | 来源 |
|---|---|---|---|
| **COR-12** | kitty 键盘协议三处规范偏差：`kitty_associated_text_field` 不检查 event_type 致 **release 事件也附带 text**（规范仅 press 带）；仅 DISAMBIGUATE+REPORT_EVENT_TYPES 时可打印键 release 不上报；legacy 修饰 F3 编码 `\x1b[1;{m}R` 与 DSR 光标应答形态冲突（kitty 路径已改对，legacy 路径未改） | `keyboard.rs:305-323, 133-184, 381` | 02 §L2 |
| **COR-13** | `replace_text_in_range` 忽略 `replacement_range`（代码注释已声明为已知限制）。听写、自动纠错、日文再变换等替换**已提交**文本的路径退化为追加 → 重复输入。纯拼音组合无影响 | `input.rs:1727-1764` | 02 §L3 |
| **COR-14** | 组合中点击终端任意位置 `refresh_ime_context(discard=true)` 直接丢弃拼音，而 macOS 惯例（Terminal.app/TextEdit）是**提交**。属 0de5d1b 有意设计，但与 COR-2 一起构成"组合内容易静默丢失" | `input.rs:626` → `terminal.rs:879-900` | 02 §L4 |
| **COR-15** | `text_for_range` 对 `start >= end` 返回**整个** marked text 并把 range 改成 `0..len`（空 range 查询语义不符）；`selected_text_range` 恒返回 `0..0`，组合期间 IME 询问选区得到退化答案 | `input.rs:1683-1687, 1694-1708` | 02 §L5 |

### 结构 / 工程化

| ID | 缺陷 | 位置 | 来源 |
|---|---|---|---|
| **ARCH-1** | ⚠️ **scrollback 功能的前置阻塞项**：`refresh_snapshot` 的网格行**不加** `display_offset`（负行被 `continue` 丢弃），而光标行**加了**。当前 display_offset 恒为 0 故等价，一旦实现 TODO #10 回看，**回看时整屏内容全部消失且光标行错位** | `terminal.rs:763` vs `:823`<br>`vendor/…/grid/mod.rs:422-429` | 03 §L6 |
| **ARCH-2** | 语义级重复：网格 cell → CellSnapshot 转换（INVERSE/HIDDEN/wide/zerowidth/link 全套）**×2**；`cell_advance_cols` **×4**；`row_text_without_wide_spacers` ×3；选区三件套 ×2；逐 cell paint 循环 ×2；`build_terminal_font`/`palette_for` ×2；line_height 计算 ×3。**宽字符类 bug 必须改 4 处才不复发**——这是 COR-4/COR-5/DSP-4 这一 bug 族的根因生成器 | 见 01 §5 完整清单 | 01 §5 |
| **ARCH-3** | `AgentTerminal` 是横跨 3 文件、约 3200 行、约 60 个几乎全 `pub(crate)` 字段的分布式 God-object（`impl` 分布：terminal.rs + input.rs:240-1291 + render.rs:183-692）。光标动画 5 字段、延迟探针 8 字段、选区 4 字段全部平铺顶层，任何文件可绕过状态机直改 `cursor_anim_from_col` | `terminal.rs:272-340` | 01 §2 |
| **ARCH-4** | `Render::render` 内执行 macOS AX 状态同步**并回写模型**，还刷新输入法状态 —— 渲染函数成了事件循环钩子。这是后续一切拆分的前置阻塞 | `render.rs:184-205, 231-233` | 01 §3 |
| **ENG-2** | `cargo clippy --release` 当前**失败**：3 个 error 全来自 `modifiers.control \|\| modifiers.alt \|\| (modifiers.shift && modifiers.alt)` 的第三项被第二项完全吸收（`overly_complex_bool_expr`，默认 deny）。行为等价无 bug，但**说明作者原本可能想写别的条件**（如 `shift && !alt`），需回看意图。另有 3 个 warning。⚠️ `IMPROVEMENT_PLAN.md` 阶段 0 声称已建立 `-D warnings` 门槛并标记完成，实际门槛是红的 | `keyboard.rs:195, 198`<br>`terminal.rs:1451` | 实测 |
| **ENG-3** | 依赖 `block v0.1.6`（objc 生态传递依赖）**将被未来 Rust 版本拒绝编译**，目前被 `rust-toolchain.toml` 固定 1.95.0 掩盖 | `Cargo.lock` | 实测 |
| **ENG-4** | `.gitignore` 只有两行，漏 `dist/`（含数十 MB 的 `.app`）与 `.DS_Store`；`vendor/*/Cargo.lock` 也应加 | `.gitignore` | 04 §6 |
| **ENG-5** | `docs/cjk_font_fallback_task.md` **未被 git 跟踪** —— 它是 ENG-1 那处 gpui patch 的唯一说明文档 | — | 04 §6 |
| **ENG-6** | vendored crate 是 path 依赖但未加入 `[workspace] members`，根目录 `cargo test -p alacritty_terminal` 直接失败。其 271 个测试（135+45+1，目录内可跑且全绿）**从不在项目测试路径上**，自写的 sixel 接缝零上游测试保护 | `Cargo.toml` | 04 §4.2 |
| **ENG-7** | 无任何 CI（无 `.github/`）。所有质量门槛靠人工与 `AGENTS.md` 约定；`rust-toolchain.toml` 只锁 channel，未声明 `components = ["clippy","rustfmt"]`，新环境上 clippy 可能不可用 | — | 04 §5.2 |
| **ENG-8** | `TODO.md` 全部行号引用指向已不存在的 `src/main.rs` 布局；其中 **#4 的修复方案本身是错的**（见 COR-4），文档正在主动误导。统计：11 条中已完成 3、部分完成 4、未修复 4 | `TODO.md` | 04 §2 |
| **ENG-9** | README 组件行数表偏离实际（input.rs 写 ~1600 实际 2057；terminal.rs 写 ~1100 实际 1970）；debug HTTP 与 `AGENT_TUI_DEBUG_ADDR` 当普通功能介绍、**无任何安全警示**；未提 `--input-log-raw` 隐私影响；「No scrollback UI」已被 snapshot tab 部分解决未更新；缺 Threat model 一节（AX 暴露面：任何有辅助功能权限的进程可读走命令行并注入替换） | `README.md`<br>`macos_ax.rs:111-123` | 04 §3.3, §1.4 |
| **ENG-10** | vendored fork 无 `VENDOR.md`：无上游 commit/tag 基线、无改动清单（实为 4 文件 +481/-3）、无 rebase 步骤。升级需手工 diff 复原 | `vendor/alacritty_terminal/` | 04 §5.1 |
| **ENG-11** | `scripts/build-macos-app.sh` wrapper 设 `TERM_PROGRAM=agent_terminal`，与 `pty.rs:39` 的 `rterminal` **不一致**（下游工具探测结果随启动方式变化）；无 codesign/公证步骤，而本项目需 AX 权限，**未签名 App 每次重建都会掉 TCC 授权** | `scripts/build-macos-app.sh`<br>`pty.rs:39` | 04 §3.4 |
| **ENG-12** | 根目录无 LICENSE，README 称 "private and unlicensed"，但产物静态链接 Apache-2.0 的 alacritty_terminal。一旦对外分发 `.app` 需保留第三方许可副本/NOTICE | — | 04 §5.2 |
| **ENG-13** | `docs/IMPROVEMENT_PLAN.md` 验证记录停在 2026-03-13、写「15 tests」（现为 91），且阶段 0 的 clippy 门槛与现实矛盾（见 ENG-2）；`research/api-system-plan.md` 零落地，其自称的 "loopback bind by default / bounded payload size" 两条安全默认现状均未满足，且仍把「写 API 是否默认要 token」列为 open decision | `docs/IMPROVEMENT_PLAN.md`<br>`research/api-system-plan.md` | 04 §3.1, §3.2 |

---

## 统计

| 级别 | 数量 | 其中安全 | 其中已在 TODO.md 中登记 |
|---|---:|---:|---|
| S0 严重 | 4 | 1 | SEC-1（#5，未修） |
| S1 高 | 3 | 2 | — |
| S2 中 | 22 | 4 | ROB-3（#8）、COR-4（#4，方案错误）、COR-10（#11）、PERF-1（#1+#7） |
| S3 低 | 22 | 0 | ARCH-1（#10 的前置阻塞） |
| **合计** | **51** | **7** | — |

## 经核查确认无问题（免复查清单）

- **PTY 字节流 UTF-8 截断**：8 KB 读缓冲切断多字节序列由 vte 流式解析器正确续接；sixel 分流器跨 chunk 保序、`finish_dcs` 先 flush 前置文本。
- `utf16_to_byte_index` 对落在代理对中间的索引截断到字符起点，插入始终在 char 边界。
- **空 grid / resize 路径**有 `MIN_COLS/MIN_ROWS` 与全面 clamp。
- **宽字符两格绘制**：`WIDE_CHAR_SPACER` 跳过 + `covered_until_col` 使背景/选区/文字宽度均按 `width_cols` 正确放大；行高/基线由 gpui 居中，无自算错误。
- **SIXEL 解码器越界/除零/大分配防护完备**：`MAX_SIXEL_DIMENSION`/`MAX_SIXEL_PIXELS` 双重钳制，`parse_params`/`parse_number` 饱和运算，无除法；RGBA→BGRA 与 `RenderImage` 尺寸校验正确。
- **bracketed-paste 防注入**：剥离 ESC 字节，`\x1b[201~` 逃逸不可行（但非 bracketed 分支缺同样处理，见 SEC-7）。
- **锁的使用**：无持锁渲染、无嵌套加锁、无死锁风险；OSC52/kitty/DSR 响应经 pending 队列，临界区短。
- `compute_grid_size` 恒减 `CUSTOM_TITLE_BAR_HEIGHT` 在两种模式下均对应实际存在的栏，无行数浪费。
- `indexed_to_rgb`（TODO #6）**已修复**：优先查 `colors[index]`，`Default::default()` 分支为不可达死代码，可清理但非缺陷。
- `AgentTerminal::new()` 的 Ok/Err 重复（TODO #3）**已修复**。
