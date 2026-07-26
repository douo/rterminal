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

## 阶段 1 · 输入保真与存活性 —— 进行中

见 [06-work-plan.md](06-work-plan.md#阶段-1--输入保真与存活性34-天)。
