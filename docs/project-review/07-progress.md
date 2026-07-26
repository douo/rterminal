# 执行进度

> 对应 [06-work-plan.md](06-work-plan.md) 的分阶段计划。缺陷编号见 [05-bug-ledger.md](05-bug-ledger.md)。
> 起点：`main @ c4301f9`

## 阶段 0 · 止血与护栏 —— 已完成

| 缺陷 | 状态 | 提交 |
|---|---|---|
| ENG-1 gpui CJK patch 不在版本控制 | 已缓解（见下） | `f89ecc6` |
| ENG-4 `.gitignore` 漏 dist/ 与 .DS_Store | 已修 | `f89ecc6` |
| ENG-5 cjk 文档未被 git 跟踪 | 已修 | `f89ecc6` |
| ENG-6 vendored 181 个测试跑不到 | 已修 | `f89ecc6` |
| ENG-2 clippy 3 error + 5 warning | 已修 | `2d6cd7a` |
| ENG-7 无 CI、toolchain 缺 components | 已修 | `2d6cd7a` |
| ROB-1 DCS 无上限致假死 + OOM | 已修 | `831fe7d` |
| SEC-1 debug HTTP 无认证任意命令执行 | 已修 | `2f99bcd` |
| SEC-2 可绑 0.0.0.0 | 已修 | `2f99bcd` |
| SEC-5 请求体无上限 | 已修 | `2f99bcd` |
| ENG-9 README（debug/安全部分） | 部分完成 | `29c4a2e` |

### 验收记录

- `scripts/check.sh` 端到端通过：补丁校验 → fmt → `clippy --all-targets -D warnings` → 测试 → self-check。
- 测试数：主 crate **91 → 105**；workspace 合计 **272**（91→105 + 135 + 45 + 1）。
  此前 vendored 的 181 个从不被跑到。
- debug HTTP 端到端验证（用 `AGENT_TUI_DEBUG_ADDR` 指定端口，避开机上一个跑了 5 天的旧实例）：
  - 不带 `--debug-http` → 不监听
  - 带上后：无 token → 401；伪造 `Host: attacker.example.com` → 403；正确 token → 200
  - `AGENT_TUI_DEBUG_ADDR=0.0.0.0:...` → 拒绝启动并打印原因
- `patches/apply-vendor-patches.sh` 做了往返验证：还原 → 检出缺失并 exit 1 → 重新应用 → 校验通过。

### 遗留与偏差

- **ENG-1 只是缓解，不是根治。** 补丁已进版本控制（`patches/gpui_macos-cjk-fallback.patch`）
  且脚本 + CI 会在缺失时**显式失败**，所以"静默退化"这个性质已经消除。但根治需要把补丁推到
  一个自己的 zed fork 并用 `[patch]` 指过去 —— 那需要一个 GitHub fork，属方向决策，
  没有擅自代做。只 vendor `gpui_macos` 单个 crate 不可行：它对 zed workspace 内其他 crate
  有大量 path 依赖。
- **`cargo fmt` 成了新的脚枪。** vendored crate 现在是 workspace 成员，裸跑 `cargo fmt`
  会把整棵上游源码重排（实测约 1500 行无关 diff），毁掉与上游对比的能力。rustfmt 的
  `ignore` 选项是 nightly-only，用不了，只能靠作用域约束：用 `scripts/check.sh`，
  或显式写 `cargo fmt -p agent_terminal`。已写进 `VENDOR.md` 与 README。
- **CI 尚未在真实 GitHub 上跑过**（本地无法验证 workflow 语法之外的行为）。首次推送后需确认。
- **`keyboard.rs` 那处 clippy 死逻辑按行为保持处理。** `control || alt || (shift && alt)`
  的第三项被第二项吸收；查 git log 该函数自 `889baa8` 引入后从未改动，没有证据表明作者
  想写 `shift && !alt`，所以只删冗余项并补注释说明 shift 单独按下是故意排除的
  （Shift+Tab 有明确的 legacy 编码 CSI Z）。是否该让 shift 也走 CSI-u 属 kitty 规范
  一致性问题，归入阶段 3 的 COR-12。
- **SEC-4（每 tab 泄漏一个 debug server 线程/端口）未做**，仍按计划留在阶段 5。
  实测机上那个旧实例正占着 13 个端口，是这个泄漏的实证。
- README 组件行数表仍过时，留待阶段 5 的 ENG-9 一起修。

## 阶段 1 · 输入保真与存活性 —— 已完成

| 缺陷 | 状态 | 提交 |
|---|---|---|
| COR-1 Ctrl+标点误编码（`Ctrl+-` 发 CR 执行命令行） | 已修 | `78b0c4b` |
| COR-7 Ctrl+Alt 丢 ESC 前缀 | 已修 | `78b0c4b` |
| COR-9 死键双重输入 | 已修 | `78b0c4b` |
| COR-2 `unmarkText` 丢弃整段组合 | 已修 | `d86d8de` |
| COR-6 组合中粘贴/拖放不处理 marked text | 已修 | `d86d8de` |
| COR-14 点击丢弃而非提交组合 | 已修 | `d86d8de` |
| COR-3 SGR release 丢按钮号 | 已修 | `8e1684e` |
| COR-8 focus guard 滞留吞掉下次 release | 已修 | `8e1684e` |
| COR-11 Cmd+A 在 alt-screen 误发 Ctrl-U | 已修 | `8e1684e` |
| ROB-3 reader 静默退出 + 不重试 EINTR | 已修 | `6bb13ba` |
| ROB-4 子进程从不 wait 导致僵尸 | 已修 | `6bb13ba` |
| SEC-7 纯 ASCII 多行粘贴不触发确认 | 已修 | `4a078de` |
| SEC-6 input log 明文 + 0644 | 已修 | `4a078de` |
| ROB-2 PTY 写阻塞 UI 线程 | 已修 | `d7abdab` |
| ROB-5 输出通道无背压 | 已修 | `d7abdab` |
| SEC-3 debug 写绕过主线程与影子模型 | 已修 | `a774876` |

### 验收记录

- `scripts/check.sh` 全绿。主 crate 测试 **105 → 120**，workspace 合计 **301**。
- 新增回归测试要点：
  - Ctrl+标点编码表，含"`Ctrl+-` 不再发 CR""`Ctrl+3` 不再发 XOFF"的负向断言；
  - focus guard 的"拖出表面松开"序列；
  - SGR release 保留按钮号与修饰位；
  - 粘贴风险判定（`curl … | sh` 形态必须确认）；
  - PTY 投递不阻塞调用方（用永远写不动的 writer 模拟卡死的 PTY）、字节顺序不变。
- 端到端验证：
  - 向不读 stdin 的 `sleep 60` 注入 200 KB → 请求 0.47 ms 返回，应用保持响应；
  - shell 自己 `exit` 后 tab 保持打开 → 子进程已从进程表消失，无 `STAT=Z`；
  - 新建 input log 权限为 `-rw-------`，`--input-log-raw` 打印警示；
  - debug 注入经主线程抵达 shell，`injected_events` 计数正确。

### 遗留与偏差

- **IME 三条（COR-2 / COR-6 / COR-14）没有自动化测试。** `AgentTerminal` 需要 gpui 的
  `Window`/`Context`，无法在单元测试里构造，项目也没有 gpui `TestAppContext` 脚手架
  （`input.rs` 现有测试全是纯函数）。真实的 IME 组合路径需要人工用中文输入法验证。
  **建议单独排一项"搭 gpui 测试脚手架"**——它能同时解锁 IME、焦点、渲染这三块目前
  完全测不到的区域，价值高于任何单条 bug 修复。COR-11 同样受此限制。
- **SEC-3 的"模型确实同步了"无法观测。** `/debug/state` 不含 `input_line`，
  所以只能给出结构性依据（注入现在调用键盘路径的同两个函数）。要可观测需给
  `/debug/state` 加字段，属 `research/api-system-plan.md` 范畴。
- **ROB-2 差点悄悄削弱 SEC-1 的回归测试。** 写入变异步后，"未认证不得落到 PTY"
  那几条如果立刻检查空 sink 就会**假通过**——被拒的请求和"还没写完"的请求看起来
  一样。已改成先给 100 ms 时间窗口再断言。这类"异步化削弱既有反向断言"的连带风险
  值得在后续阶段留意。
- **两个既有测试编码的是 bug 行为**（断言纯 ASCII 4 行粘贴"安全"），已按新意图改写。
  这是收紧而非放宽，与 iTerm2 的多行粘贴确认行为一致。
- **写队列满时会丢弃并打日志**，而不是阻塞。取舍理由：那种情况意味着前台程序真的
  不读 stdin 了，这次写本来也到不了子进程；静默堆积更糟。

## 阶段 2 · 去重 —— 已完成

| 条目 | 状态 | 提交 |
|---|---|---|
| ARCH-2 cell 语义 4 份拷贝 → 1 份（新模块 `grid_cells.rs`） | 已修 | `88b5b75` |
| ENG-6b 收拢后的唯一实现补单元测试（宽字符/emoji/zerowidth/选区） | 已修 | `88b5b75` |
| ARCH-4 AX 同步与输入法刷新搬出 `Render::render` | 已修 | `03d8754` |

### 验收记录

- `grep -rn "fn cell_advance_cols" src/` → 1 处定义（grid_cells.rs）；
  `row_text_without_wide_spacers`、选区三件套、`build_terminal_font`、`palette_for`、
  line_height 公式同样各剩 1 处。
- terminal.rs 两处 85 行的网格转换循环各缩成 5 行，共用 `snapshot_cell()`。
- `Render::render` 内无模型写操作；AX 同步改由 100ms 周期任务驱动，
  外部 AX 覆写的最大发现延迟从"取决于是否有帧"变为固定 100ms。
- `scripts/check.sh` 全绿；主 crate 测试 **120 → 129**，workspace 合计 **310**。

### 遗留与偏差

- 逐 cell paint 循环（render.rs vs snapshot_tab.rs，~90 行 ×2）**未合并**：
  两边的选区来源、palette 字段、滚动窗口逻辑不同，硬抽会得到一个带 6 个参数的
  闭包地狱。它属于渲染热路径，留给阶段 4 的 PERF-1b（按 run 合并 shape）时
  一并重写——那时循环体本身就要换掉，先合并是白做。
- AX 同步从"每帧"改为"每 100ms"是行为变化：发布模型 → AX 的延迟上限从一帧
  变为 100ms。对语音工具的读路径无感知差异（人的操作粒度远大于 100ms），
  但若未来有自动化工具高频读 AX，需要把节拍调小或改成"模型变更即推"。

## 阶段 3 · 显示与坐标保真 —— 已完成

| 缺陷 | 状态 | 提交 |
|---|---|---|
| COR-4 左箭头按 UTF-16 单元数（emoji 多退一格） | 已修 | `47ccc0d` |
| COR-5 IME 候选窗按 UTF-16 单元定位 | 已修 | `47ccc0d` |
| DSP-4 鼠标坐标纯线性除法（hover 与点击错位） | 已修 | `47ccc0d` |
| DSP-1 sixel 与滚动事件顺序错位致图片偏移（S1） | 已修 | `ead311d` |
| DSP-2 图片与 alt screen 互相穿透 | 已修 | `ead311d` |
| DSP-3 resize 后图片与预留区脱节 | 已修 | `ead311d` |
| DSP-5 BOLD+DIM 返回原色 | 已修 | `51522ac` |
| DSP-6 Indexed 前景忽略 DIM | 已修 | `51522ac` |
| DSP-7 INVERSE 在变体应用之前交换 | 已修 | `51522ac` |
| DSP-8 光标在宽字符上只盖左半格 | 已修 | `51522ac` |
| DSP-9 IME 文本色硬编码不随主题 | 已修 | `51522ac` |
| DSP-15 光标灰化判据用 window_active | 已修 | `51522ac` |
| DSP-11 sixel 空列不计入宽度 | 已修 | `1566eca` |
| DSP-12 DCS 内裸 0x9c 截断 UTF-8 | 已修 | `1566eca` |
| DSP-13 measure_cell_width 回退与渲染错位 | 已修 | `1566eca` |
| DSP-14 本地化家族名交给 CoreText 的失配风险 | 已修 | `1566eca` |
| COR-12 kitty 三处规范偏差 | 已修 | `80b3184` |
| COR-13 replacement_range 被忽略致重复输入 | 已修 | `80b3184` |
| COR-15 空 range / 选区查询退化答案 | 已修 | `80b3184` |

### 验收记录

- `scripts/check.sh` 全绿。主 crate 测试 **129 → 142**，vendored sixel +1（透明列宽度），
  workspace 合计 **324**。
- 新增回归测试要点：尾部字符数按 char 计（emoji/CJK）、视觉列↔逻辑列双向映射、
  滚动/擦除只影响同屏图片、BOLD+DIM/Indexed DIM/INVERSE×BOLD 颜色语义、
  DCS 内 UTF-8 中的 0x9c 不截断、kitty printable release 上报与 release 无
  associated text、legacy F3 避开 DSR 冲突。

### 遗留与偏差

- **DSP-10（EL 清行对整行发 Erase、图片"相交即整删"）不在工作计划排期内**，维持现状。
  它是行为取舍而非明确 bug（提示符重绘会闪掉图片），若要精细化需要给 Erase 事件带
  列范围，涉及 vendored 事件结构改动，收益低。
- COR-13 的听写替换路径依赖影子模型的行内 range 语义，而"文档坐标系"对 IME 只有
  marked text 一段是精确的。已用"调用前是否存在组合"隔离组合提交路径，并以
  range 越界检查兜底；真实听写场景仍需人工验证（同阶段 1 的 gpui 脚手架缺口）。
- 视觉验收（`seq 30; img2sixel`、`\e[1;7;31m`、CJK 块光标）依赖真实 GUI，
  单元测试已覆盖对应的纯函数语义，端到端确认待人工跑 `scripts/run.sh`。

## 阶段 4 · 性能 —— 已完成

| 条目 | 状态 | 提交 |
|---|---|---|
| PERF-1a 每帧两次整屏深拷贝 → Arc 克隆 | 已修 | `1fe0a22` |
| PERF-1b 逐 cell shape → 按 run 合并整段 shape | 已修 | `1fe0a22` |
| PERF-1c prepaint/paint 重复测量字宽 | 已修 | `1fe0a22` |
| PERF-3 摄取侧 URL 扫描 / debug 文本重建惰性化 | 已修 | `1fe0a22` |
| PERF-2 系统字体扫描移出首窗口路径 | 已修 | `1fe0a22` |
| （冒烟发现）私有点前缀字体进 fallback 表 | 已修 | `d679499` |

### 验收记录

- `scripts/check.sh` 全绿（324 测试）。
- 真实应用冒烟（`--debug-http` 注入驱动）：注入含 CJK/emoji/SGR 颜色的命令，
  `/debug/state` 回显逐字正确；修复点前缀字体后 CoreText 警告从成串降到 0。
- run 合并的正确性边界都写进了 `paint_merged_run` 的文档注释：宽字符 /
  zerowidth / 强制双宽仍单独 shape（force_width 把第 i 个字形钉在
  i×cell_width，只对"每字形一列"成立）；空白只延续无装饰 run。

### 遗留与偏差

- **计划要求的"CPU 占用相对基线明显下降"没有量化数字。** 基线测量需要在
  改动前的构建上跑 `yes` 洪泛并采样 CPU，改完后同条件对比——审查时未预先
  采基线，此时补测只能测"现在"，没有对照。结构性依据是明确的（每帧堆克隆
  从 ~5 万次降到 2 次 Arc 克隆、shape 调用从 ~2000 次/帧降到每行数个 run、
  LineLayoutCache 键空间大幅缩小），如需数字建议后续用 Instruments 对
  `1fe0a22` 前后两个构建各采一次。
- **视觉截图验证被 macOS 屏幕录制权限（TCC）拦截**——`screencapture` 在本
  shell 上下文无授权。文本层（/debug/state）已验证；像素层的最终确认需要
  用户自己看一眼窗口（重点：粗体/颜色 run 边界、CJK 与 emoji 间距、
  下划线不画穿空格）。
- PERF-2 的换装依赖 100ms 周期任务：扫描完成后最迟 100ms 换上完整
  fallback 表，首帧到换装之间罕见符号（Nerd Font 图标等）可能短暂用
  保守表渲染。

## 下一步

阶段 5（结构与工程化收尾）：ARCH-3 字段分组、SEC-4 debug server 生命周期、
ENG-8~13 文档与工程化、ENG-3 future-incompat 评估。
