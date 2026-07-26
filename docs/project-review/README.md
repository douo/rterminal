# 项目深度审查（2026-07-26）

对 rterminal 的一次全面代码审查：缺陷排查 + 架构评估 + 重构必要性判断 + 工作计划。
审查基线 `main @ c4301f9`（56 次提交）。**审查全程只读，未修改任何源文件。**

## 目录

| 文件 | 内容 | 读它如果你想…… |
|---|---|---|
| [06-work-plan.md](06-work-plan.md) | **工作计划**（第一性原理推导 + 6 个阶段 + 战略决策项 + 不做清单） | 知道接下来干什么、为什么按这个顺序 |
| [05-bug-ledger.md](05-bug-ledger.md) | **缺陷台账**：51 项，统一编号、已跨报告去重、含免复查清单 | 查某个缺陷的位置与严重度 |
| [01-architecture.md](01-architecture.md) | 架构审查：数据流与锁、God-object 解剖、耦合、渲染路径、重复代码清单、重构建议 | 判断要不要重构、怎么重构 |
| [02-input-bugs.md](02-input-bugs.md) | 输入链路 bug：键盘编码、IME、鼠标、影子输入行 | 修输入相关缺陷 |
| [03-render-pty-sixel-bugs.md](03-render-pty-sixel-bugs.md) | 渲染 / SIXEL / PTY bug：颜色、图片、进程管理、字体 | 修显示或 PTY 相关缺陷 |
| [04-infra-security-docs.md](04-infra-security-docs.md) | 工程基础设施：安全清单、TODO.md 逐条核验、文档盘点、测试现状、vendor 策略 | 了解安全面、文档时效性、依赖风险 |

## 结论摘要

**需要整理，不需要重构。** 架构骨架是健康的，其中有一处比 alacritty/zed 更聪明的选择（`Term` 独占持有 + channel 解耦 + 256 KB 批量上限，替代 `FairMutex<Term>` 共享内存）——不要动它。代码组织与测试纪律（91 个单元测试、模块已拆分、详实的中文排障备忘）明显优于同规模个人项目。

但有 **4 项严重缺陷**违反了"终端"这个东西的定义性契约：

1. **`Ctrl+-` 被编码成 `0x0d`（CR）—— 直接执行当前命令行。** `Ctrl+3` 发 XOFF 冻结输出、`Ctrl+;` 发 ESC 踢出 vim 插入模式。根因是对所有 Ctrl+键统一做 `& 0x1f`，而该运算只对字母和少数符号有意义。（COR-1）
2. **debug HTTP 接口默认无条件启动、无认证**，`POST /debug/input` 直写 PTY。因 `Content-Type: text/plain` 属 CORS safelisted，**任意网页可用 `fetch(mode:'no-cors')` 穷举那 100 个端口，往你的 shell 注入并执行命令**。`/debug/state` 还泄露屏幕全文。（SEC-1）
3. **`printf '\x1bP'; cat 大文件` 让终端永久假死并无界吃内存** —— SIXEL 的 DCS 分流器无长度上限、不处理 CAN/SUB 中止。（ROB-1）
4. **正确的中文渲染依赖一处只存在于 cargo cache 里的 gpui 手改**，`cargo clean` 或换机器就静默退化，无任何测试能发现——这让"中文渲染正常"成为一句不可验证的断言。（ENG-1）

另有 3 项高危（`AGENT_TUI_DEBUG_ADDR` 可把上述接口绑到 `0.0.0.0`；`unmarkText` 丢弃整段拼音；SIXEL 与滚动的事件顺序错位致图片必现偏移）。

**排序原则**：先修违反契约的（A 输入保真 / B 输出保真 / C 存活性 / D 隔离性），再消灭缺陷的根因生成器，最后才是性能与结构。完整推导见工作计划第一节。

**一个反直觉的排序结论**：去重（`cell_advance_cols` 有 4 份拷贝）必须排在修那一族宽字符 bug **之前**，否则修 3 处漏 1 处，然后在某个 emoji 上再复发一次。

## 顺带发现的元问题

- **`TODO.md` 正在主动误导**：全部行号指向已不存在的 `src/main.rs` 布局；其中第 4 条提出的修复方案（按 wcwidth 列宽发送左箭头）**方向是错的**，照做会让 CJK 光标多退一倍。真正的错位只发生在 emoji 等非 BMP 字符上，应按字符数计。
- **文档声称已建立的 clippy 质量门槛，实测是红的**（3 个 error）。`IMPROVEMENT_PLAN.md` 阶段 0 标记"已完成"，但因为没有 CI，无人会发现。
- **vendored alacritty 的 180 个测试从不被跑到**（path 依赖但未加入 workspace members），自写的 sixel 接缝零上游测试保护。
- `docs/cjk_font_fallback_task.md` —— ENG-1 那处 patch 的唯一说明文档 —— **还没有被 git 跟踪**。

## 审查时的实测状态

| 检查 | 结果 |
|---|---|
| `cargo check --release` | ✅ 通过 |
| `cargo test`（主 crate） | ✅ 91 passed, 0 failed |
| `cargo test`（vendored，需进目录） | ✅ 135 + 45 + 1 passed，但不在项目测试路径上 |
| `cargo clippy --release` | ❌ 3 errors + 3 warnings |
| 依赖 future-incompat | ⚠️ `block v0.1.6` 将被未来 Rust 拒绝编译 |

## 缺陷分布

| 级别 | 数量 | 判据 |
|---|---:|---|
| S0 严重 | 4 | 违反终端核心契约，或使项目产出不可复现 |
| S1 高 | 3 | 数据丢失或安全暴露面扩大，触发条件明确 |
| S2 中 | 22 | 功能性错误用户可感知；或资源泄漏随使用时长累积 |
| S3 低 | 22 | 局部不一致、非默认配置、规范偏差、内部质量 |
| **合计** | **51** | 其中安全类 7 项 |

## 与既有文档的关系

本目录**不替代**以下文档，它们仍然有效：

- `docs/cjk_font_fallback_task.md`、`ime_focus_recovery_memo.md`、`input_method_cursor_indicator_memo.md`、`terminal_graphics_and_atuin_memo.md` —— 具体问题的排障日志与设计备忘，内容仍准确。
- `research/terminal-implementation-research.md` —— 架构选型的背景调研。

本目录**建议替换**：`TODO.md`（行号全失效且含错误方案，见工作计划阶段 5 的 ENG-8）。
本目录**建议吸收后更新**：`docs/IMPROVEMENT_PLAN.md`（其阶段 3 未完成的内容已并入本计划的 SEC-4）。
