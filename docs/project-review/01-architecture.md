# 架构审查报告

> 子报告 1/4 · 审查基线 `main @ c4301f9` · 只读分析
> 审查范围：`src/` 全部一手代码（重点 main.rs / terminal.rs / tabs.rs / render.rs / pty.rs / input.rs / cli.rs / snapshot_tab.rs），vendored alacritty_terminal 仅看接口。

**总体结论**：这不是一个"需要推倒重来"的代码库。main.rs、pty.rs、cli.rs、tabs.rs 都小而干净，单线程所有权模型（不用 FairMutex）是一个合理且比 zed/alacritty 更简单的设计。真正的问题集中在两点：**AgentTerminal 是一个横跨三个文件、约 60 个 `pub(crate)` 字段的"分布式 God-object"**，以及**渲染路径上每帧的重复拷贝与逐 cell shape**。此外 snapshot_tab.rs 与主渲染路径存在大面积复制粘贴。

---

## 1. 数据流 / 所有权 / 锁

### 1.1 链路全景

```
PTY reader 线程 (pty.rs:61-74)
   │  read() → 8KB chunk
   ▼
async_channel::unbounded (pty.rs:60)
   │
   ▼
_pump_task（gpui 主线程 executor, terminal.rs:553-585）
   │  批量 drain（≤256 chunk / ≤256KB, terminal.rs:57-58, 560-569）
   ▼
ingest_batch (terminal.rs:590) ── sixel_parser.advance → processor.advance(&mut self.term)  (terminal.rs:630)
   │        （alacritty Term 的 VTE 解析发生在 UI 线程！）
   ▼
refresh_snapshot (terminal.rs:730)  →  ScreenSnapshot（Vec<Vec<CellSnapshot>> 全量重建）
   │  cx.notify()
   ▼
Render::render (render.rs:183)  →  snapshot.clone() ×2 (render.rs:207, 246)
   ▼
canvas paint 闭包 (render.rs:283-634)  →  逐 cell shape_line + paint
```

**所有权模型**：`Term` 被 `AgentTerminal` 实体**独占持有**（terminal.rs:274 `pub(crate) term: Term<TitleTrackingListener>`），不加任何锁。`grep FairMutex src/` 零命中 —— 这与 alacritty/zed 的"解析线程 + FairMutex\<Term\>"模式完全不同：本项目把解析搬到了 UI 线程，用消息通道代替共享内存。反向通道（Term → PTY，如 DSR 应答）通过 `TitleTrackingListener`（terminal.rs:132-181）：`PtyWrite` 同步写 PTY，其余事件压入 `Arc<Mutex<Vec<PendingTerminalEvent>>>`，在 `process_pending_terminal_events`（terminal.rs:633）消费。

### 1.2 锁的评估

用到的锁全部是 parking_lot::Mutex，粒度都很小：`writer` / `master` / `child`（terminal.rs:302-304）、`terminal_title`（terminal.rs:292）、`pending_term_events`（terminal.rs:333）、`canvas_bounds`（terminal.rs:329）。

- **无持锁渲染**：render 前先把 snapshot 深拷贝出来（render.rs:207），paint 闭包中只有两处瞬时锁：`terminal_title.lock()`（render.rs:238-242）和 `canvas_bounds_shared.lock()` 写回 bounds（render.rs:284）。没有跨 paint 生命周期持有的锁。
- **无死锁风险**：全代码未发现嵌套加锁；`PtySession::spawn` 中对 master 的两次 lock 是顺序的（pty.rs:48-59）。
- **真实的性能/健壮性问题**（按严重度）：
  1. **PTY 写在 UI 线程上阻塞式进行**：`write_to_pty` 持 writer 锁做 `write_all + flush`（pty.rs:86-92），由键盘路径（input.rs `write_bytes` → terminal.rs:978）和解析路径（terminal.rs:141-144 的 `PtyWrite`）在主线程调用。若子进程停止读取、内核 PTY 缓冲写满，UI 线程会被卡死。
  2. **无背压的 unbounded channel**（pty.rs:60）：reader 线程永远在读，子进程侧永远感觉不到背压；UI 侧每次 update 只消费 ≤256KB，`cat 大文件` / `yes` 洪泛时通道会无限堆内存。
  3. **VTE 解析占用 UI 线程**：单次 update 的停顿被 256KB 上限约束住了（terminal.rs:57-58），这是一个合理的折衷，但意味着高吞吐输出时 UI 帧率与解析吞吐互相挤占。
  4. **每个 batch 全量重建 snapshot**：`refresh_snapshot` 每次 O(rows×cols) 重建所有 cell（含每 cell 的 String/Vec 分配），还附带整屏 URL 扫描 `annotate_plain_text_links`（terminal.rs:820）和 debug 用整屏文本行重建（terminal.rs:842-847，即使没人连 debug server 也做）。

**小结**：锁本身没病，病在"该异步的写是同步的、该有界的通道是无界的、该增量的快照是全量的"。

## 2. terminal.rs God-object 解剖

名义上 terminal.rs 是 1970 行，但 `AgentTerminal` 的方法实际横跨三个文件：`impl AgentTerminal` 在 input.rs:240-1291 还有约 1050 行，`impl Render for AgentTerminal` 在 render.rs:183-692。**真实的 God-object 约 3200 行、约 60 个字段（terminal.rs:272-340），几乎全部 `pub(crate)`。**

terminal.rs 内可拆分的内聚单元（行数为现址估计）：

| 单元 | 现址 | 约行数 | 内聚性 |
|---|---|---|---|
| PTY 事件监听 + pending 事件队列 | terminal.rs:107-181, 633-663 | 130 | 高，几乎零依赖 |
| Cell/Screen 快照类型 + 网格→快照转换 | terminal.rs:183-256, 730-848, 1004-1101 | 330 | 高；且 730-848 与 1004-1101 是重复逻辑（见 §5） |
| 光标滑动动画 | terminal.rs:59-60, 280-290 字段, 1103-1195 | 110 | 高，纯状态机 |
| Enter 延迟探针 + PTY 摄取诊断 | terminal.rs:264-270, 1197-1376 | 190 | 高，纯遥测 |
| 明文 URL 检测 | terminal.rs:1431-1511 | 80 | 高，纯函数 |
| 网格尺寸/字号/resize | terminal.rs:850-871, 939-976, 1389-1415 | 90 | 高 |
| sixel 摄取胶水 | terminal.rs:590-697 | 100 | 中（依赖 term+images） |
| OSC52 剪贴板 | terminal.rs:699-728 | 30 | 高 |
| IME/输入法监听生命周期 | terminal.rs:879-937 | 60 | 中 |
| self_check | terminal.rs:1887-1970 | 85 | 高，应随 cli 走 |
| 测试 | terminal.rs:1558-1885 | 330 | — |

input.rs 里挂在同一 struct 上的还有：input_line 影子模型 + macOS AX 同步（input.rs:240-470, 1106-1291，约 450 行）、鼠标/选区/mouse-report 编码（input.rs:481-870, 1172-1228, 1293-1660，约 600 行）、键盘（input.rs:871-1100）、IME EntityInputHandler（input.rs:1664-1815）。

**字段污染示例**：光标动画的 5 个字段（terminal.rs:286-290）、延迟探针的 8 个字段（terminal.rs:314-319）、选区的 4 个字段（terminal.rs:325-328）全部平铺在顶层且 `pub(crate)`，任何文件都能绕过状态机直接改 `cursor_anim_from_col`。

## 3. 模块间耦合

- **"按文件拆分"≠"按边界拆分"**：TODO.md #2 的模块拆分做了，但 input.rs / render.rs / terminal.rs 三个文件都在同一个 struct 的内脏里操作。terminal.rs 里 `pub(crate)` 出现 130 次；input.rs 里 `self.` 引用近 300 处，直接读写 `snapshot` / `term` / `selection_*` / `input_line` 等字段而非通过方法。
- **input ↔ render 双向依赖**：render.rs:10 引 `crate::input::selection_contains_cell`；input.rs:19-22 反向引 render.rs 的 `CUSTOM_TITLE_BAR_HEIGHT / STATUS_BAR_HEIGHT / TEXT_PADDING_X / measure_cell_width / terminal_content_padding_y`。布局几何常量被"渲染"模块拥有，但输入命中测试同样需要 —— 说明缺一个 layout/geometry 模块。
- **render() 里做非渲染副作用**：`Render::render` 开头执行 macOS AX 状态同步并**回写模型**（render.rs:184-205 `apply_external_ax_input_state` + `last_ax_published_*` 赋值），还在 render 里刷新输入法状态（render.rs:231-233）。渲染函数成了事件循环钩子，这是最反直觉的耦合点。
- **snapshot_tab.rs 靠复制而非复用与主路径解耦**（详见 §5）—— 耦合的另一种病态形式。
- **干净的部分**：tabs.rs 只通过 `Entity<AgentTerminal>` 的公开方法交互（tabs.rs:41, 50-52, 159）；pty.rs、cli.rs、color.rs、keyboard.rs、text_utils.rs 边界清晰。

## 4. 渲染路径的每帧重复工作

1. **"逐字符 shape"问题仍然存在**：TODO.md #1 所指的逻辑原样搬到了 render.rs:396-401 —— paint 闭包里对每个非空白 cell 单独 `cell.text()`（String 分配）+ 构造 `TextRun` + `shape_line`。80×24 满屏 ≈ 每帧近 2000 次调用。gpui 的 LineLayoutCache 会对相同文本命中缓存，所以实际开销是"每 cell 一次 String 分配 + 缓存哈希查找"而非完整 shape，但仍然放弃了整行 batch、也天然做不了连字。snapshot_tab.rs:429 是同一问题的复制体。
2. **每帧两次整屏深拷贝**：render.rs:207 `self.snapshot.clone()` + render.rs:246 `canvas_snapshot = snapshot.clone()`。`CellSnapshot` 含 `Vec<char>` 和 `Option<String>`（link），大窗口（如 300×80）意味着每帧约 5 万次 cell 级堆结构克隆。快照本身在 ingest 时已是不可变的，包一层 `Arc<ScreenSnapshot>` 即可归零。
3. **cell_width 反复测量**：`measure_cell_width`（font resolve + advance）在 prepaint 的 `link_hover_bounds` 每帧调一次（render.rs:86-87），paint 里又用同样逻辑重算一次（render.rs:306-311），而 `self.cell_width` 字段（terminal.rs:300）明明已在 `sync_grid_to_window` 维护。TODO.md #7 未解决。
4. **每帧重建 Font/TextRun 模板与克隆 font_family/fallbacks 字符串**（render.rs:225-248, 292-303）。
5. **颜色转换**做得对：在 `refresh_snapshot` 摄取时一次性转成 Hsla（terminal.rs:807-808），渲染时零转换。但代价是转换发生在**每个 PTY batch**而非每帧变化时；配合整屏 URL 扫描（terminal.rs:820）和 debug 行重建（terminal.rs:842-847），高输出场景下摄取侧是新的热点。
6. `color.rs:104-119`（TODO #6）仍在用 `Default::default()` colors 兜底 0-15 号索引色。经交叉核查（见子报告 3/4），该分支实际为不可达死代码，非缺陷，但值得清理。

## 5. 重复代码（复制粘贴清单）

| 重复内容 | 位置 | 规模 |
|---|---|---|
| 网格 cell → CellSnapshot 转换（INVERSE/HIDDEN/wide/zerowidth/link 全套逻辑） | terminal.rs:762-819（refresh_snapshot） vs terminal.rs:1038-1086（capture_snapshot_data） | ~85 行 ×2，**最危险的重复**：宽字符/隐藏字符语义改动必须双写 |
| 空白 CellSnapshot 字面量（无视已有 `Default` impl） | terminal.rs:736-758, 1019-1035 | ~20 行 ×2 |
| 逐 cell paint 循环（bg → selection → shape → paint → spacer 记账） | render.rs:327-420 vs snapshot_tab.rs:360-453 | ~90 行 ×2 |
| `cell_advance_cols` | terminal.rs:1532, render.rs:46, input.rs:1615, snapshot_tab.rs:473 | **×4** |
| `row_text_without_wide_spacers` | terminal.rs:1540, input.rs:1604, snapshot_tab.rs:556 | ×3 |
| `extract_selection_text` / `normalize_selection_bounds` / `normalize_selection_col` | input.rs:1532/1496/1585 vs snapshot_tab.rs:509/481/492 | ~120 行 ×2 |
| `build_terminal_font` | render.rs:40, snapshot_tab.rs:467 | ×2 |
| `palette_for`（主题色板） | render.rs:162-181 vs snapshot_tab.rs:30-43 | ×2 |
| line_height 计算 | render.rs:134-136 vs snapshot_tab.rs:83-85, 461-465 | ×3 |

TODO.md #3（`new()` Ok/Err 分支重复）已经修好了 —— 现在用元组解构一次分支（terminal.rs:387-412）。tabs.rs 的 `open_new_tab` / `open_snapshot_tab_from_active`（tabs.rs:123-172）只共享约 8 行 push/activate 样板，属可接受范围。

## 6. 重构建议（按必要性排序）

### 值得做（高收益/低风险，建议顺序执行）

1. **抽出 `grid_cells` 公共模块，消灭 cell 转换与几何 helper 的重复**。把 CellSnapshot/ScreenSnapshot、`cell_advance_cols`、`row_text_without_wide_spacers`、selection 三件套、`build_terminal_font`、`palette_for`、网格→快照转换（参数化"整屏 viewport"与"含 scrollback 全量"两种遍历）收进一处。这是纯机械重构，直接消掉 §5 里 90% 的条目，宽字符类 bug 从"改 4 处"变"改 1 处"。约一天工作量，**必要**。
2. **渲染热路径三连**：(a) `snapshot` 改 `Arc<ScreenSnapshot>`，消掉每帧两次深拷贝（render.rs:207, 246）；(b) 按 run 合并同格式相邻 cell 后整段 `shape_line`（解决 TODO #1，render.rs:396）；(c) paint 内直接用 `self.cell_width`，删掉 prepaint/paint 的重复测量（render.rs:86, 306）。**必要** —— 这是用户可感知的性能项。
3. **把 AX 同步搬出 `Render::render`**（render.rs:184-205）到独立的 update 路径（如 observe/frame callback）。渲染恢复只读，是后续一切拆分的前提。**必要**。
4. **字段分组瘦身 AgentTerminal**：不必拆文件，先把字段收进子 struct —— `CursorSlide`（5 字段+3 方法）、`LatencyDiagnostics`（8 字段+全部 probe 方法，terminal.rs:1197-1376）、`SelectionState`（4 字段+input.rs 的选区方法）、`InputLineMirror`（input_line/AX 6 字段）。子 struct 字段私有，God-object 从 60 字段降到 ~25，误用面大幅收窄。**值得**。
5. **PTY 通道加界 + 写异步化**：`async_channel::bounded`（pty.rs:60）恢复背压；PTY 写挪到专职写线程/任务，UI 只投递（解决 §1.2 的阻塞写）。**值得**，防御性改动，各半天。
6. **debug server 改 opt-in**（terminal.rs:429 现在无条件启动并监听 127.0.0.1，`/debug/input` 可注入任意字节）。TODO #5 至今未做，属安全项。**值得且便宜**。

### 不值得做（过度设计警告）

- **引入 zed 式 `FairMutex<Term>` + 独立解析线程**。当前"通道 + UI 线程解析 + 256KB 批量上限"在延迟上更优、在正确性上更简单。除非做完上面第 2 条后实测仍有洪泛卡顿，否则不要动这个地基。
- **给 Term/渲染后端做 trait 抽象**、拆 workspace 多 crate、事件总线化。单二进制、单窗口、单渲染后端的项目，抽象层只会增加阅读成本。
- **强行把 input.rs 再按"键盘/鼠标/IME"拆成三个文件**。在完成建议 4（字段分组）之前，按文件切分只会制造更多跨文件的字段触碰 —— 这正是上一轮"main.rs 拆模块"留下的教训：文件拆开了，对象没拆开。
- **input_line 影子模型（TODO #11）的彻底重写**（如接 shell 集成协议 OSC 133）。方向正确但工程量大，且现有 AX 用例依赖它的具体行为，建议等它真正成为用户可见 bug 源时再动。

### 一句话总结

架构骨架（单线程 Term 所有权、通道解耦 PTY、entity/render 分层）是健康的，不需要"重构"级别的手术；需要的是**三类整理**：把分布式 God-object 的字段收拢成有边界的子状态、把 4 份复制的 cell 语义合成 1 份、把渲染路径上每帧的两次深拷贝和 2000 次逐 cell shape 消掉。以上皆为增量可验证的改动，没有一项要求停下功能开发。
