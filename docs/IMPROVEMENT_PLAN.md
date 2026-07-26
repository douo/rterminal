# Agent TUI 改进计划（架构与可维护性）

目标：在不破坏现有功能前提下，提升焦点/生命周期一致性、降低重复代码成本、补齐可回归测试。

## 阶段 0：质量门槛统一（已完成）
- [x] 将 `cargo clippy --all-targets -- -D warnings` 作为本地通过门槛。
- [x] 修复当前 clippy 阻塞项（dead_code、test 模块位置、可折叠 if、单字符 push_str）。
- 验证：`cargo check` + `cargo test` + `cargo clippy --all-targets -- -D warnings`。

## 阶段 1：Tab 焦点状态机收敛（已完成）
- [x] 统一 “激活 tab => 强制焦点落到 active terminal”。
- [x] 修复点击“当前 tab”不重新聚焦的问题。
- [x] 修复 shell-exit 触发关闭 tab 后焦点未一致恢复的问题。
- [x] 提取纯函数并补测试，覆盖 close 后 active index 变化规则。
- 验证：阶段 0 全量 + 新增单元测试。

## 阶段 2：重复代码收敛（已完成）
- [x] 收敛 tab 索引切换处理的重复逻辑（数据驱动/宏化，保持行为一致）。
- [x] 收敛输入模块中明显重复包装函数（保持 API 兼容）。
- 验证：阶段 0 全量 + 行为回归测试。

## 阶段 3：Debug Server 生命周期治理（已完成，并入项目审查阶段 5 的 SEC-4）
- [x] 单实例 debug server + 会话路由：进程级 `DebugTabRegistry` + 单 server 线程，
      路由 `/debug/tabs/{id}/...`，旧路径兼容映射到首个存活 tab。
- [x] tab 创建时经 `register_debug_http_tab` 注册，`DebugTabHandle` 随
      `AgentTerminal` Drop 自动注销。
- [x] 端到端验证多 tab 路由与已关闭 tab 的 404 行为（见
      `docs/project-review/07-progress.md` 阶段 5）。

## 里程碑验收标准
1. 不引入功能回退（现有自动化测试持续通过）。
2. 每阶段结束至少完成一次全量验证并记录结果。
3. 对每个线上问题修复，至少增加一个能复现/防回归的测试点。

> 本计划已由 `docs/project-review/06-work-plan.md`（2026-07-26 的全面审查工作计划）
> 吸收并替代；后续进度看 `docs/project-review/07-progress.md`。

## 当前验证记录
- 2026-03-13：阶段 0 + 阶段 1 + 阶段 2（部分）完成后，`cargo check` / `cargo test`（15 tests）/ `cargo clippy --all-targets -- -D warnings` 全部通过。
- 2026-03-13：阶段 2 全量完成后，再次通过 `cargo check` / `cargo test`（15 tests）/ `cargo clippy --all-targets -- -D warnings` / `cargo run -- --self-check`。
- 2026-07-26：项目审查阶段 0–5 完成后，`scripts/check.sh`（fmt + clippy -D warnings +
  workspace 全量测试 + self-check）全绿；测试规模 15 → **327**（主 crate 145 +
  vendored 136/45/1）。历史记录中的"15 tests"停留在模块拆分之前，勿再引用。
