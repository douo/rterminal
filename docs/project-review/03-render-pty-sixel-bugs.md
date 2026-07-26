# 渲染 / SIXEL / PTY Bug 审查报告

> 子报告 3/4 · 审查基线 `main @ c4301f9` · 只读分析
> 审查范围：`src/render.rs`、`src/color.rs`、`src/font_fallback.rs`、`src/sixel/{parser,images,pixel,mod}.rs`、`src/pty.rs`、`src/terminal.rs`（PTY IO / resize / scroll 相关）、`vendor/alacritty_terminal/src/sixel.rs`，并对照 `TODO.md` 与 `docs/cjk_font_fallback_task.md`。

---

## 一、高严重度

### H1. SIXEL 流解析器对未终止 DCS 无任何上限：终端"假死" + 内存无界增长
- **位置**：`src/sixel/parser.rs:81-145`（`DcsEntry` / `DcsData` 状态），`payload.push` / `raw.push` 均无长度限制；且无 CAN(0x18)/SUB(0x1a) 中止处理。
- **触发场景**：任何进入 DCS 后长期不出现终止符（`ESC \` 或 0x9c）的字节流。典型：`printf '\x1bP'; cat 大文本文件` —— 纯 ASCII 文本永远不含 0x9c，此后**整个会话的全部输出**被静默吞入 `payload` 和 `raw` 两个 Vec（双份缓冲），屏幕不再更新，内存持续增长，且无恢复手段。`cat` 二进制文件误触 `\x1bP` 也会短暂吞流。上游 alacritty 的 VTE 对 DCS/OSC 有长度上限并以流式 hook 处理，本项目自写的前置分流器没有。
- **严重程度**：高（易触发、表现为挂死 + OOM 风险）。
- **建议**：给 payload/raw 设上限（如 8–32 MB），超限后把 raw 原样吐回 `Bytes` 并回到 Ground；处理 CAN/SUB 中止。

### H2. 同一批次中先于图片产生的 Scroll 事件被错误地施加到新图片上，导致 SIXEL 错位
- **位置**：`src/terminal.rs:590-627`（`ingest_batch`：逐 chunk 处理，`process_pending_terminal_events` 只在整批末尾调用一次）+ `src/terminal.rs:633-663` + `src/sixel/images.rs:55-83`（`store` 先入队图片，依赖后续统一 drain 的 Scroll 事件对齐）。
- **机制**：文本字节在 `advance_text_bytes` 中立即处理，其触发的 `Event::Scroll` 进入 `pending_term_events` 队列；SIXEL 图片以"当时的光标行"作锚点入库；批末 `process_pending_terminal_events` 把**队列里全部** Scroll 增量应用到所有图片 —— 包括图片入库**之前**由普通换行产生的滚动量。
- **触发场景**：一次 PTY read（最多 8 KB/256 chunk 批）里"先有若干行输出把屏幕滚动了 N 行、后跟 SIXEL"，例如 `seq 30; img2sixel foo.png` 的输出合并在同一批到达且光标在底行：图片会被额外上移 N 行，画在错误位置甚至被判定滚出而直接丢弃。
- **严重程度**：高（在"输出滚动 + 图片"这一 SIXEL 最常见使用形态下必现级错位）。
- **建议**：在 `store_sixel_image` 之前先 drain 一次 pending 事件（只作用于已有图片），或给图片记录"入库时的事件序号"。

---

## 二、中严重度

### M1. 进入/退出 alt screen 不清理图片：SIXEL 覆盖在 vim/less 等全屏应用之上
- **位置**：`vendor/alacritty_terminal/src/term/mod.rs:721`（`swap_alt` 不发 `Event::Erase`，vendored 补丁只在 clear/EL/ED 处发事件）；渲染侧 `src/render.rs:422-454` 无条件绘制 `images`，未使用 `snapshot.alt_screen`（`src/terminal.rs:734` 已采集但未消费）。
- **触发场景**：主屏显示过 SIXEL 后打开 vim/less/tmux —— 图片仍浮在全屏界面上；反之在 alt screen 内预览图片（yazi/ranger）退出后图片残留在主屏，直到某次 clear 或 128 张上限淘汰。
- **严重程度**：中。
- **建议**：`swap_alt` 补发 Erase 事件，或渲染层按 `alt_screen` 标志与图片入库时的屏幕类型过滤。

### M2. 窗口 resize 后图片坐标不随 reflow 调整
- **位置**：`src/terminal.rs:951-976`（`apply_grid_size` 调 `term.resize` 触发行 reflow / 行进出 scrollback，但完全不触碰 `self.images`）。
- **触发场景**：显示图片后拖动改变窗口高/宽：文本行因 reflow 与 history 搬移而移动，图片行号原地不动，图片与其预留空白区脱节、覆盖文字。另外 resize 期间可能产生的事件要等**下一次 PTY ingest** 才被 drain（`process_pending_terminal_events` 只在 `ingest_batch` 里调用），存在窗口期错位。
- **严重程度**：中。

### M3. `write_to_pty` 在 UI 线程持锁阻塞写：对停住的前台程序大量粘贴会冻结整个应用
- **位置**：`src/pty.rs:86-93`（`write_all` + `flush`，持 `writer` 锁）；调用点 `src/terminal.rs:978-994`（主线程）。debug HTTP 线程（`src/debug_server.rs`）与主线程共用同一把 writer 锁，也可能互相阻塞。
- **触发场景**：前台程序不读 stdin（如被 `Ctrl-S` 流控、或 `sleep` 中）时粘贴超过内核 PTY 缓冲（约 16–64 KB）的文本：`write_all` 无限期阻塞，UI 线程挂死，窗口无响应。
- **严重程度**：中。

### M4. PTY reader 线程：错误静默退出、EINTR 不重试（TODO #8 未修复）
- **位置**：`src/pty.rs:71`（`Err(_) => break`）。
- **问题**：
  - a) 任何 read 错误无日志、无上报，排障困难（TODO #8 原样存在，只是从 main.rs 搬到了 pty.rs）；
  - b) `std::io::Read::read` 不自动重试 `EINTR` —— 一旦被信号打断，reader 线程退出、channel 关闭，pump 任务会把仍然存活的 shell 误标记为 "shell exited"（`src/terminal.rs:577-583`），终端永久失去输出。
- **严重程度**：中。
- **建议**：`Err(e) if e.kind() == Interrupted => continue`，其余分支记录错误（eprintln 或送入 debug state）。

### M5. 子进程从不 `wait()`：僵尸进程累积
- **位置**：`src/terminal.rs:1381-1387`（`Drop` 只 `kill()` 不 wait）；全仓无任何 `wait()` 调用；pump 任务检测到 EOF 后（`src/terminal.rs:577-583`）也不回收。
- **触发场景**：shell 自行退出（输入 `exit`）后 tab 仍开着，或多次开关 tab：每个退出的 shell 都以 zombie 形式挂在进程表直到 app 退出。
- **严重程度**：中（长时间多 tab 使用会累积）。

### M6. 鼠标坐标映射不考虑 `expands_layout` 视觉偏移：点击/选区/链接打开与渲染错位
- **位置**：`src/input.rs:810-869`（`mouse_grid_point` 纯线性 `x / cell_width`）对比 `src/render.rs:54-70,102-129`（渲染与 hover 命中都会为 `expands_layout` 宽字符累加 `extra_visual_cols`）。
- **触发场景**：使用 `--double-width-chars` 后，任一强制双宽字符右侧的所有单元：链接 hover 高亮框（render 路径，正确）与实际点击命中（input 路径，错误）指向不同单元；选区高亮同样错位（`selection_contains_cell` 用逻辑列，绘制却按视觉列）。
- **严重程度**：中（仅在该 CLI 选项启用时；启用后为系统性错位）。
- **备注**：与子报告 2/4 的 L1 是同一缺陷。

### M7. 每帧渲染性能：整屏快照深拷贝两次 + 逐 cell shape + 逐 cell String 分配（TODO #1 未修复）
- **位置**：
  - `src/render.rs:207` 与 `src/render.rs:246`：`self.snapshot.clone()` 两次完整深拷贝（每 cell 含 `Vec<char>` 与 `Option<String>`），每帧执行；
  - `src/render.rs:357-410`：每个非空白 cell 一次 `cell.text()`（堆分配 String）+ 一次 `shape_line`（80×24 满屏 ≈ 每帧近 2000 次 shape）—— TODO #1"批量按行 shape"未修复；
  - `src/render.rs:86-87, 306-311` 与 `src/input.rs:816-822`：每帧 prepaint/paint、每次鼠标事件都重新 `resolve_font + advance` 测量 cell 宽，未复用已缓存的 `self.cell_width`（TODO #7 只修了一半）。
- **严重程度**：中（大窗口/高刷 PTY 输出时 CPU 明显偏高；光标滑动动画期间连续逐帧重绘放大该成本）。

### M8. 首次启动在主线程同步扫描并解析全部系统字体
- **位置**：`src/font_fallback.rs:50-99`（`OnceLock` 初始化时 `load_system_fonts` + 对每个 face `with_face_data` 读文件并 `ttf_parser::Face::parse`），调用链 `src/terminal.rs:371 → 1523-1530`，在窗口创建路径上。
- **触发场景**：装有大量字体的 macOS 首窗口打开卡顿（数百 ms 级）；且 `OnceLock` 进程期缓存，运行中安装/卸载字体不生效（后者可接受，前者影响启动体验）。
- **严重程度**：中偏低。

### M9. debug HTTP 服务无条件启动、无认证，且随 tab 泄漏线程与 writer
- **位置**：`src/debug_server.rs:190-214`（无开关，`AGENT_TUI_DEBUG_ADDR` 只能改地址不能禁用）；每个 `AgentTerminal::new_with_options` 都会启动一个（`src/terminal.rs:429`）。
- **问题**：
  - a) 本机任意进程可向 `/debug/input` 注入任意字节到用户 shell（TODO #5"默认不启动/加 token"未修复，仅把端口挪到 37878-37977 段）；
  - b) server 线程永不退出并持有 PTY writer 的 `Arc`，关闭 tab 后线程、端口、writer（连同底层 fd）全部泄漏。
- **严重程度**：中（本地提权面 + 资源泄漏）。**注**：子报告 4/4 将 (a) 单独评为 Critical，理由是可被任意网页 CSRF 触发；以该评级为准。

---

## 三、低严重度

### L1. BOLD+DIM 组合与 alacritty 语义不一致
- **位置**：`src/color.rs:48-57`。`(true, true, _)` 落入 `_ => named` 返回原色；alacritty 将 BOLD|DIM 按 DIM 处理。触发：`\e[1;2;31m`。

### L2. `Indexed` 颜色完全忽略 DIM（与 BOLD 变亮）
- **位置**：`src/color.rs:39`（`AnsiColor::Indexed` 分支不看 flags）。`\e[38;5;1m\e[2m` 不变暗；`Spec` 分支（`color.rs:29-35`）有 DIM 处理，行为不一致。

### L3. INVERSE 交换发生在 BOLD/DIM 变体应用之前
- **位置**：`src/terminal.rs:778-782`（及 `capture_snapshot_data` 的 `1044-1048`）先 swap，再以 `is_foreground=true` + 原 flags 调 `ansi_to_hsla` —— bright/dim 被施加到"原背景色"上而不是原前景色。触发：`\e[1;7;31m`：反色块应为亮红背景，实际是普通红。

### L4. 块状/下划线光标在宽字符上只有一格宽
- **位置**：`src/render.rs:512`（`cell_width_px = cell_width.max(px(2.0))`，未乘所在 cell 的 `width_cols`）。光标停在 CJK/emoji 上时 Block/Underline/HollowBlock 只覆盖左半格。另：本项目光标为半透明覆盖层，无真正"反色光标"，深色主题下可读性依赖 alpha，属设计取舍。

### L5. IME 组合文本与默认 run 颜色写死，不随主题
- **位置**：`src/render.rs:296-303`（`run_template.color: rgb(0xd7dae0)`）与 `471-489`（IME 底色用 `palette.terminal_bg` 尚可，但文字色固定）。EyeCare 主题下 IME 文本颜色与主题脱节。

### L6. `refresh_snapshot` 网格行不加 `display_offset`，而光标行加了 —— 潜在坐标系分叉
- **位置**：`src/terminal.rs:763`（`let row = indexed.point.line.0;` 直接用 grid Line 空间，负行被 `continue` 丢弃）对比 `:823`（光标行 `+ display_offset`）。vendored `display_iter`（`vendor/alacritty_terminal/src/grid/mod.rs:422-429`）从 `Line(-display_offset)` 起步。当前 app 从不设置 display_offset（无滚动回看），恒为 0 时两者等价；**一旦实现 TODO #10 滚动回看，回看时整屏内容将全部消失而光标行错位**。现在是"埋雷"，标低。
- **重要**：这是实现 scrollback 功能的前置阻塞项。

### L7. 行清除（`EL`）会整幅删除相交图片
- **位置**：`vendor/alacritty_terminal/src/term/mod.rs:1683-1686`（EL 对整行发 Erase，即使只清了光标右侧）+ `src/sixel/images.rs:187-198`（任意相交即整图 retain 掉）。触发：任何程序在图片占据的行上执行 `\e[K`（如提示符重绘、`clear -x`），图片瞬间消失。粒度过粗，属可感知但可容忍。

### L8. SIXEL 解码：空列（`?`）不计入 `max_x`，无声明宽度时右缘透明列被截掉
- **位置**：`vendor/alacritty_terminal/src/sixel.rs:156-165`（只有置位像素更新 `max_x/max_y`）与 `:217-229`（finish 用 `max_x`）。无 `"` 光栅声明、以透明列结尾的图片宽度偏窄，占列计算（`images.rs:144-147`）随之偏小。
- **健壮性方面经核查是安全的**：`MAX_SIXEL_DIMENSION`/`MAX_SIXEL_PIXELS` 双重钳制齐全（`sixel.rs:147-153, 167-184, 193-195, 241-245`），`parse_params`/`parse_number` 饱和运算，无除法。

### L9. DCS 载荷内 0x9c 会截断合法 UTF-8
- **位置**：`src/sixel/parser.rs:101-103`（`DcsData` 中裸 0x9c 立即终止 DCS，不区分它是否为 UTF-8 连续字节）。SIXEL 载荷是 ASCII 不受影响；含 UTF-8 的未知 DCS/tmux passthrough 会被误切。上游 VTE 以 UTF-8 感知方式处理 C1。

### L10. `measure_cell_width` 失败回退 `px(8.0)` 与网格计算脱节
- **位置**：`src/render.rs:33-37`。字体解析失败时渲染仍会用系统回退字体的真实 advance 画字，但网格/PTY winsize 按 8px 计算，列数与视觉错位。仅在字体名完全无效时出现。

### L11. `preferred_family_name` 取 `families[0]` 可能得到本地化名
- **位置**：`src/font_fallback.rs:119-125`。fontdb 的 families 含多语言名，取首个非空者在非英文 locale 下可能拿到本地化家族名，交给 GPUI/CoreText 解析存在失配风险。
- CJK 过滤逻辑本身（`:197-205`，任一 CJK 探针命中即拒绝）与 `docs/cjk_font_fallback_task.md` 的设计一致，核查无误。
- **注意**：该文档提到的 GPUI `open_type.rs` 补丁位于 cargo cache，`cargo clean -p gpui_macos` 后会丢失（文档已自知）。此项在子报告 4/4 被列为 P0 可复现性风险。

---

## 四、TODO.md 对照结论

| TODO 条目 | 状态 | 依据 |
|---|---|---|
| #1 逐字符 shape → 批量按行 | **未修复** | `src/render.rs:396-401` 仍逐 cell `shape_line`，另每 cell 一次 `cell.text()` String 分配（见 M7） |
| #6 `indexed_to_rgb` 传入正确 colors | **实质已修复** | `src/color.rs:99` 先查 `colors[index]`（0-15 与 `colors[NamedColor]` 共用同一数组槽位）；`:104-119` 各分支里的 `&Default::default()` 只是无害死代码（该路径下查询必为 None，落回硬编码色），可清理但不影响正确性 |
| #7 `measure_cell_width` 缓存 | **部分修复** | 网格/PTY 侧已缓存 `self.cell_width`（`src/terminal.rs:299,864`）；但渲染每帧（`src/render.rs:86-87,306-311`）与每次鼠标事件（`src/input.rs:816`）仍重新测量 |
| #8 PTY reader 错误记录 | **未修复** | `src/pty.rs:71` 仍 `Err(_) => break`，且未处理 EINTR（见 M4） |
| #10 滚动回看 | **未修复** | `src/input.rs:547-577` 滚轮仅在 mouse-mode / alt-screen 下生效，全仓无 `scroll_display`/`display_offset` 操作；**且实现前需先修 L6 的坐标系分叉** |
| （附）#5 debug HTTP 加固 | **未修复** | `src/terminal.rs:429` 无条件启动，无 token（见 M9） |

## 五、经核查无问题的点（供后续免复查）

- **PTY 字节流 UTF-8 截断**：8 KB 读缓冲切断多字节序列由 VTE 流式解析器正确续接；sixel 分流器按字节状态机跨 chunk 保序，`finish_dcs` 先 flush 前置文本，顺序无误。
- **宽字符两格绘制**：`WIDE_CHAR_SPACER` 跳过 + `covered_until_col` 逻辑（`render.rs:329-419`）背景/选区/文字宽度均按 `width_cols` 正确放大；行高/基线交由 gpui `shaped.paint(origin, line_height, ...)` 居中，无自算基线错误。
- **SIXEL 解码器的越界/除零/大分配防护完备**（见 L8 备注）；RGBA→BGRA 转换与 `RenderImage` 尺寸校验（`images.rs:97-109`）正确。
- `compute_grid_size` 恒减 `CUSTOM_TITLE_BAR_HEIGHT` 在两种模式下均对应实际存在的标题栏/标签栏，无行数浪费。
- **OSC52、kitty keyboard、DSR 响应路径**经由 `TitleTrackingListener` 与 pending 队列，线程安全（`parking_lot::Mutex`，短临界区）。

---

**优先修复建议排序**：H1（解析器上限）→ H2（sixel 滚动事件顺序）→ M3（UI 线程阻塞写）→ M4/M5（PTY 错误与僵尸回收）→ M1/M2（图片与 alt screen / resize 交互）→ M7（每帧分配与逐 cell shape）。
