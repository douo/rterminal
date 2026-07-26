# 工作计划

> 综合产物 · 制定日期 2026-07-26 · 审查基线 `main @ c4301f9`
> 缺陷编号见 [05-bug-ledger.md](05-bug-ledger.md)。执行进度见 [07-progress.md](07-progress.md)。

---

## 一、第一性原理：凭什么这样排序

排序不按"看起来急"或"TODO.md 里排第几"，而是先回答一个问题：**这个东西是什么？**

一个终端模拟器在本质上是一条**双向字节管道**加一份**显示契约**。它的定义性属性只有四条：

| | 契约 | 违反意味着 |
|---|---|---|
| **A** | **输入保真**：用户按下的键 → 精确对应的字节，不多不少 | 它执行了用户没打的命令，或吞掉了用户打的内容 |
| **B** | **输出保真**：PTY 字节流 → 屏幕状态的确定性映射 | 屏幕在说谎 |
| **C** | **存活性**：无论收到什么字节，终端保持响应 | 它是个会挂的管道 |
| **D** | **隔离性**：只有用户及用户授权的程序能向 PTY 写 | 它是个后门 |

**不满足这四条的东西不是终端，是一个偶尔正常工作的输入设备。** 所以任何违反 A–D 的缺陷，优先级都在功能、性能、结构之上——无论它看起来多小。

据此，四份审查报告里最刺眼的不是那个 2057 行的文件，而是：

- `Ctrl+-` 发送 `0x0d` = **回车**（违反 A：执行了用户没打的命令）→ **COR-1**
- 任意网页可 CSRF 到 `127.0.0.1:37878..37977` 往你的 shell 写 `\n`（违反 D）→ **SEC-1**
- `printf '\x1bP'` 之后终端永久停止更新并无界吃内存（违反 C）→ **ROB-1**
- IME 走 `unmarkText` 收尾时整段拼音静默消失（违反 A）→ **COR-2**

这四条构成阶段 0/1 的全部内容。它们加起来的改动量不超过 200 行。

再往上有三条**元原则**，它们决定了其余阶段的次序：

**元原则 1 — 可复现性是所有断言的前提。**
正确的中文渲染依赖一处只存在于 `~/.cargo/git/checkouts/` 里的 gpui 手改（**ENG-1**）。这意味着"中文渲染正常"这句话既不可验证也不可传递：换台机器、跑一次 `cargo clean`，它就静默变假。在把这个 patch 沉淀进版本控制之前，一切关于字体与渲染的工作都建立在流沙上。所以它和 S0 缺陷同批处理。

**元原则 2 — 先消灭缺陷的根因生成器，再修缺陷。**
观察到的 COR-4（左箭头按 UTF-16 单元数）、COR-5（候选窗按 UTF-16 单元定位）、DSP-4（鼠标坐标按像素线性除法）是**同一类错误**：某处用了字符数 / UTF-16 单元数 / 像素除法，而正确答案是列宽。它们之所以能各自独立地写错，是因为 `cell_advance_cols` 有 **4 份拷贝**、cell 语义转换有 **2 份拷贝**（**ARCH-2**）。

这推出一个反直觉但重要的排序结论：**去重要排在修这一族 bug 之前，而不是之后。** 否则修 3 处漏 1 处，然后在某个 emoji 上再复发一次。去重在这里不是审美活动，是把复发面从 4 降到 1。

**元原则 3 — 产品论点的保真度不是"体验问题"。**
这个项目不是"又一个终端"。它的差异化是把当前输入行通过 macOS AX 暴露出去，让语音/agent 读改命令行。**这个命题的价值完全等于影子模型的保真度**——所以 **COR-10**（↑/↓ 历史、Tab 补全、多行粘贴后模型变脏）不是小 bug，是产品论点的正确性根基。

但它也是唯一一条从第一性原理看**当前方法本身有上限**的缺陷：靠嗅探写向 PTY 的字节来反推 shell 行内容，是一个原理性的启发式，永远补不完（补完 ↑/↓ 还有 `Ctrl-R`、`fzf` widget、zsh 自动建议……）。唯一能拿到 ground truth 的办法是**让 shell 自己说**（OSC 133 shell integration / zsh ZLE hook）。这是方向性决策，列为战略项而非排期任务（见第四节）。

---

## 二、结论先行：需要重构吗？

**需要整理，不需要重构。**

架构骨架是健康的，而且有一处比 alacritty/zed 更聪明的选择：`Term` 由 `AgentTerminal` 独占持有、VTE 解析在 UI 线程、用 channel + 256 KB 批量上限替代 `FairMutex<Term>` 共享内存。这个设计在延迟与正确性上都更简单，**不要动它**。

真正该做的整理只有三件，每件都是增量可验证的：

1. **把 4 份复制的宽字符/cell 语义合成 1 份**（ARCH-2）——降低缺陷率，非审美。
2. **把 AX 副作用搬出 `Render::render`**（ARCH-4）——渲染函数当前在回写模型，是后续一切拆分的前置阻塞。
3. **把 60 个平铺的 `pub(crate)` 字段收进有边界的子 struct**（ARCH-3）——不必拆文件。上一轮"main.rs 拆模块"留下的教训正是：文件拆开了，对象没拆开，`AgentTerminal` 现在横跨 3 个文件 3200 行。

明确**不做**的（过度设计）见第五节。

---

## 三、分阶段计划

每阶段的验收都遵循 `AGENTS.md`：**先编译，再测试，然后才谈完成**。

### 阶段 0 · 止血与护栏（1–2 天）

同时做两件事：堵住违反契约 C/D 的窟窿，以及**建立一个绿色的质量门槛**——后续每个阶段都要靠它兜底，所以必须最先立起来。

| 条目 | 内容 | 改动量 |
|---|---|---|
| **SEC-1** | debug HTTP 改 opt-in：`cli.rs` 加 `--debug-http`（默认 false）+ `--debug-http-token`（缺省随机生成并打印）；`terminal.rs:429` 改条件调用；所有 POST 分支前统一校验 token 与 `Host` 头（拒绝非 `127.0.0.1:<port>`，防 DNS rebinding） | 中 |
| **SEC-2** | `AGENT_TUI_DEBUG_ADDR` 校验为仅 loopback，非 loopback 需额外显式 flag | 小 |
| **SEC-5** | 请求体加大小上限（如 1 MB），超限返回 413 | 小 |
| **ROB-1** | DCS payload/raw 加上限（建议 16 MB），超限把 raw 原样吐回 `Bytes` 并回到 Ground；处理 CAN(0x18)/SUB(0x1a) 中止 | 小 |
| **ENG-1** | gpui patch 沉淀：`[patch."…/zed.git"] gpui_macos = { git = "<自己的 fork>", branch = "cjk-fallback" }`；或退而 vendor `gpui_macos`。**这条卡住其他人复现本项目的能力** | 中 |
| **ENG-5** | `git add docs/cjk_font_fallback_task.md`（ENG-1 的唯一说明文档，目前 untracked） | 极小 |
| **ENG-4** | `.gitignore` 补 `dist/`、`.DS_Store`、`**/.DS_Store`、`vendor/*/Cargo.lock` | 极小 |
| **ENG-2** | 修 clippy 3 个 error。**注意**：`keyboard.rs:195,198` 的 `(shift && alt)` 被 `alt` 完全吸收，删掉即等价——但先回看原意图，作者很可能想写 `shift && !alt`，那是个真 bug 而非死代码 | 小 |
| **ENG-7** | `rust-toolchain.toml` 补 `components = ["clippy","rustfmt"]`；加 GitHub Actions（macOS runner）：`fmt --check` + `clippy --all-targets -- -D warnings` + `test` + `--self-check` | 小 |
| **ENG-6** | vendored crate 加入 `[workspace] members`，使 `cargo test` 一次覆盖 271 个测试（当前主 crate 91 个，vendored 的 180 个从不被跑到） | 极小 |

**验收**：`cargo clippy --all-targets -- -D warnings` 绿；`cargo test` 覆盖 271 个测试且全绿；不带 `--debug-http` 启动时 `curl 127.0.0.1:37878..37977` 全部连接失败；`printf '\x1bP'; cat 100MB文件` 后终端仍响应；`cargo clean && cargo build` 后中文渲染仍正确（ENG-1 真正被验证的唯一方式）。

---

### 阶段 1 · 输入保真与存活性（3–4 天）

契约 A 与 C。这一阶段修完，终端才算"不会执行你没打的东西，也不会挂"。

| 条目 | 内容 |
|---|---|
| **COR-1** | Ctrl 掩码只对 `@ a-z [ \ ] ^ _ space ?` 生效，其余键不发送或按 xterm 传统映射。**优先级最高：`Ctrl+-` 当前会执行命令行** |
| **COR-2 + COR-6 + COR-14** | 统一 IME marked text 生命周期：`unmark_text` 改为**提交**而非丢弃；粘贴/拖放入口先提交或 discard；鼠标按下改 discard 为提交（对齐 macOS 惯例）。三条同源，一起改一起测 |
| **COR-7** | ctrl 分支在 `modifiers.alt` 时前缀 `0x1b`，恢复 `C-M-*` 绑定 |
| **COR-9** | 编码函数感知 `option_as_meta`，或 alt 分支在 `key_char.is_none()` 时返回 None 交给 IME，消除死键双重输入 |
| **COR-8** | `FocusActivationMouseGuard` 在新的非 first_mouse 按下、或 focus-out 时强制清空 |
| **COR-3** | SGR release 保留原按钮号，仅切换 `M`/`m` 后缀 |
| **COR-11** | Cmd+A 在 `alt_screen` 时不发 `0x15` |
| **ROB-2 + ROB-5** | PTY 写移出 UI 线程（专职写任务，UI 只投递）；channel 改 `bounded` 恢复背压 |
| **ROB-3** | reader 线程 `Interrupted => continue`，其余错误记录并区分"真 EOF"与"读错误"，避免把存活 shell 误判为已退出 |
| **ROB-4** | 子进程 `wait()` 回收：Drop 与 pump 检测到 EOF 时都要收尸 |
| **SEC-6** | `input_log` 文件 `mode(0o600)`；`--input-log-raw` 启动时向 stderr 打印一次明确警告 |
| **SEC-7** | 粘贴确认把"多行"解耦为独立触发条件（不再要求非 ASCII 占比）；非 bracketed-paste 分支补 ESC 剥离 |
| **SEC-3** | debug 写路径改为投递到主线程处理，与键盘路径共用同一入口，从而同步影子模型 |

**验收**：`cargo test` 全绿；新增回归测试覆盖 Ctrl+标点编码表、SGR release 编码、marked text 提交路径；手工验证 `Ctrl+-`/`Ctrl+3` 不再产生 CR/XOFF；对 `sleep 100` 粘贴 1 MB 文本 UI 不冻结；`exit` 后 `ps` 无僵尸。

---

### 阶段 2 · 去重（元原则 2 的前置动作，2–3 天）

**必须在阶段 3 之前完成**，否则那一族宽字符 bug 要在 4 个地方各修一次。

| 条目 | 内容 |
|---|---|
| **ARCH-2** | 新建公共模块（建议 `grid_cells` + `geometry`），收纳：`CellSnapshot`/`ScreenSnapshot`、网格→快照转换（参数化 viewport / 含 scrollback 两种遍历）、`cell_advance_cols`、`row_text_without_wide_spacers`、选区三件套、`build_terminal_font`、`palette_for`、line_height 与布局常量。删除 `snapshot_tab.rs` 与 `input.rs` 中的复制体 |
| **ARCH-4** | AX 同步与输入法状态刷新搬出 `Render::render`，改到独立 update 路径；渲染恢复只读 |
| **ENG-6b** | 为收拢后的唯一实现补单元测试（宽字符/emoji/zerowidth/link 的列宽与选区文本提取），当前 `snapshot_tab.rs` 那三个纯函数零测试 |

**验收**：`cargo test` 全绿且新增列宽/选区测试；`grep -c cell_advance_cols src/` 结果为 1 处定义；渲染函数内无模型写操作。

---

### 阶段 3 · 显示与坐标保真（2–3 天）

契约 B。建立在阶段 2 的唯一实现之上，每条只需改一处。

| 条目 | 内容 |
|---|---|
| **COR-4** | 左箭头次数改按 `chars().count()`。**同时修正 TODO.md #4 的错误方案描述**——它提出的 wcwidth 方案会让 CJK 多退一倍 |
| **COR-5 + DSP-4** | 候选窗定位与 `mouse_grid_point` 统一走阶段 2 的列宽函数，消除 UTF-16 单元/像素线性除法的误用 |
| **DSP-1** | `store_sixel_image` 之前先 drain 一次 pending 事件（或给图片记录入库时的事件序号），修正 sixel 与滚动的顺序错位 |
| **DSP-2 + DSP-3** | 图片与 alt screen / resize 的交互：`swap_alt` 补发 Erase（或渲染层按 `alt_screen` 过滤）；`apply_grid_size` 同步调整图片坐标 |
| **DSP-5/6/7** | 颜色语义三连：BOLD+DIM 按 DIM 处理；`Indexed` 分支支持 DIM；INVERSE 交换移到 bold/dim 变体应用**之后** |
| **DSP-8** | 光标宽度乘 `width_cols`，宽字符上覆盖两格 |
| **DSP-9 / DSP-15** | IME 文本色随主题；光标灰化判据改 `focused` |
| **DSP-11/12/13** | sixel 空列计入 `max_x`；DCS 内 0x9c 做 UTF-8 感知；`measure_cell_width` 失败时的回退与网格计算对齐 |
| **COR-12/13/15** | kitty 协议三处偏差；`text_for_range` 空 range 语义；`selected_text_range` 返回真实选区 |
| **DSP-14** | `preferred_family_name` 避免本地化家族名 |

**验收**：`cargo test` 全绿；`seq 30; img2sixel x.png` 图片位置正确；`\e[1;7;31m` 渲染为亮红背景；CJK 上的块光标覆盖两格；emoji 行内 Cmd+点击链接命中正确。

---

### 阶段 4 · 性能（2–3 天）

**刻意排在正确性之后**：PERF-* 不违反任何契约，README 也已自承限制。但做完前三阶段后它成为最明显的用户可感知项。

| 条目 | 内容 |
|---|---|
| **PERF-1a** | `snapshot` 改 `Arc<ScreenSnapshot>`，消掉每帧两次整屏深拷贝（大窗口约 5 万次 cell 级堆克隆/帧） |
| **PERF-1b** | 按 run 合并同格式相邻 cell 后整段 `shape_line`，替代每 cell 一次 `cell.text()` + shape（TODO #1，约 2000 次/帧） |
| **PERF-1c** | prepaint/paint 直接用 `self.cell_width`，删掉重复测量（TODO #7 的剩余一半） |
| **PERF-3** | 摄取侧：URL 扫描与 debug 文本行重建改为按需/惰性（当前即使无人连 debug server 也每 batch 全量做） |
| **PERF-2** | 系统字体扫描移出首窗口创建路径（后台任务 + 首帧用保守回退） |

**验收**：`cargo test` 全绿；大窗口（≥200×60）下 `yes` 洪泛与光标动画期间的 CPU 占用相对基线明显下降（做前先记录基线数字）。

---

### 阶段 5 · 结构与工程化收尾（1–2 天）

| 条目 | 内容 |
|---|---|
| **ARCH-3** | 字段分组：`CursorSlide`(5)、`LatencyDiagnostics`(8)、`SelectionState`(4)、`InputLineMirror`(6) 收进子 struct 且字段私有，`AgentTerminal` 从约 60 字段降到约 25。**不拆文件** |
| **ENG-8** | 重写 `TODO.md`：现有行号全部指向已不存在的 `main.rs` 布局，且 #4 方案是错的（文档在主动误导）。改为按模块归档，已完成项移入 CHANGELOG |
| **ENG-9** | README 更新组件行数表与已知限制；**新增 Threat model 一节**，明确 debug HTTP 与 AX 暴露面（任何有辅助功能权限的进程可读走命令行并注入替换） |
| **ENG-10** | `vendor/alacritty_terminal/VENDOR.md`：上游 rev/tag、4 文件 +481/-3 的改动摘要、rebase 步骤；为 sixel 接缝补上游侧测试 |
| **ENG-13** | 更新 `IMPROVEMENT_PLAN.md` 验证记录（停在 15 tests，现 271）；`api-system-plan.md` 把「写 API 是否默认要 token」的 open decision 直接定为「默认要」 |
| **ENG-11** | 统一 `TERM_PROGRAM`（脚本 `agent_terminal` vs `pty.rs` `rterminal`）；构建脚本加 codesign，避免每次重建掉 AX 授权 |
| **ENG-12** | 加 LICENSE 与第三方许可声明（产物静态链接 Apache-2.0 的 alacritty_terminal） |
| **SEC-4** | debug server 改进程级单实例 + 路径带 tab id（`/debug/tabs/{id}/input`），tab 销毁时注销会话，修掉线程/端口/writer 泄漏（`IMPROVEMENT_PLAN.md` 阶段 3 的原定内容） |
| **ENG-3** | 评估 `block v0.1.6` 的 future-incompat：能否通过升级 objc 生态绕开，或记录为已知约束 |

---

## 四、战略决策项（需要方向判断，不排期）

这两项不是 bug 修复，而是"当前方法有原理性上限"的问题。建议单独讨论后再排期。

**决策 1 — 影子输入行模型（COR-10）要不要换地基？**
现方案靠嗅探写向 PTY 的字节反推 shell 行内容，这是原理性的启发式：补完 ↑/↓ 还有 `Ctrl-R`、`fzf` widget、zsh 自动建议、多行续行……永远补不完。从第一性原理看唯一的 ground truth 来源是让 shell 自己说：OSC 133 shell integration，或 zsh 侧的 ZLE hook 主动上报 `$BUFFER`/`$CURSOR`。

- **收益**：AX 暴露的输入行从"大概对"变成"精确"，这是产品差异化的地基。
- **代价**：需要 shell 侧配置（用户安装一段 rc 片段）；现有 AX 用例依赖影子模型的具体行为，迁移需回归。
- **建议**：先做**增量**——把 ↑/↓ 与 Tab 补全后的行内容用现有 AX 屏幕匹配机制做一次校正（低成本、无需 shell 配合），同时把 OSC 133 作为可选增强路径原型验证。不要一步推倒重写。

**决策 2 — 主屏 scrollback 回看（TODO #10）做不做？**
当前用 Cmd+Shift+S 生成只读 snapshot tab 绕开了这个需求，是个不错的取舍。若要做真正的滚轮回看：

- **前置阻塞**：必须先修 **ARCH-1** —— `refresh_snapshot` 的网格行不加 `display_offset` 而光标行加了。当前 offset 恒为 0 故等价，**一旦引入回看，回看时整屏内容会全部消失且光标行错位**。这个坑不修就动手，会得到一个看起来完全无法理解的 bug。
- **建议**：若 snapshot tab 已满足实际使用，此项可长期不做；若要做，ARCH-1 必须是第一个 commit。

---

## 五、明确不做（过度设计）

记录在案，避免以后有人"顺手"引入：

| 不做 | 理由 |
|---|---|
| 引入 zed 式 `FairMutex<Term>` + 独立解析线程 | 当前"channel + UI 线程解析 + 256 KB 批量上限"在延迟与正确性上都更优。除非做完阶段 4 后实测仍有洪泛卡顿，否则不要动这个地基 |
| 给 Term / 渲染后端做 trait 抽象 | 单二进制、单窗口、单渲染后端。抽象层只增加阅读成本，不换来任何可替换性 |
| 拆 workspace 多 crate、引入事件总线 | 同上。当前规模（约 9.7k 行一手代码）远未到需要 crate 边界的程度 |
| 把 `input.rs` 再按"键盘/鼠标/IME"拆成三个文件 | **在阶段 5 的字段分组完成之前**，按文件切分只会制造更多跨文件字段触碰——这正是上一轮"main.rs 拆模块"的教训：文件拆开了，对象没拆开 |
| 立刻推倒重写影子输入行模型 | 见决策 1：先做增量校正与原型验证，不要在没有 shell 集成方案落地前砸掉现有行为 |
| 清理 `color.rs:104-119` 的 `Default::default()` 死代码之外的"顺手重构" | 该分支经核查不可达，非缺陷。TODO #6 可直接标记完成 |

---

## 六、阶段依赖关系

```
阶段 0（止血 + 护栏）
  ├─ ENG-2 ──→ ENG-7（CI 门槛必须先绿才能建）
  ├─ ENG-1 ──→ 任何字体/渲染相关工作（否则结论不可验证）
  └─ ENG-6 ──→ 后续所有阶段的测试覆盖面
        ↓
阶段 1（输入保真 + 存活性）  ← 契约 A/C，独立可做
        ↓
阶段 2（去重 ARCH-2/ARCH-4）
  └─ 必须先于 ↓，否则同族 bug 要修 4 遍
        ↓
阶段 3（显示保真 COR-4/5 · DSP-*）
        ↓
阶段 4（性能 PERF-*）    ← ARCH-4 完成后 render 才是只读，改动才安全
        ↓
阶段 5（结构 ARCH-3 + 文档）
        ↓
战略项：决策 1（OSC 133）· 决策 2（scrollback，前置 ARCH-1）
```

**总计约 12–17 个工作日**，其中阶段 0+1（契约级，约 5 天）覆盖了全部 4 项 S0 与 3 项 S1 缺陷。若时间只够做一件事，做阶段 0。
