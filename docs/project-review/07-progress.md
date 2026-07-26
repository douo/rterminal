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

## 下一步

阶段 2（去重 ARCH-2 / ARCH-4）**必须先于**阶段 3。理由见
[06-work-plan.md](06-work-plan.md) 元原则 2：COR-4 / COR-5 / DSP-4 是同一类错误
（列宽 vs 字符数 vs UTF-16 单元 vs 像素除法），之所以能各自独立写错，是因为
`cell_advance_cols` 有 4 份拷贝。先统一再修，否则修 3 处漏 1 处。
