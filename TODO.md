# TODO

> 2026-07-26 重写（ENG-8）。旧版全部行号指向已不存在的 `src/main.rs` 单文件布局，
> 且其中 #4 的修复方案（按 wcwidth 列宽发左箭头）方向是错的——照做会让 CJK 光标
> 多退一倍。缺陷编号见 `docs/project-review/05-bug-ledger.md`，进度见
> `docs/project-review/07-progress.md`。

## 待做

### 搭 gpui 测试脚手架（价值最高的单项投入）
`AgentTerminal` 需要 gpui 的 `Window`/`Context`，无法在单元测试里构造，
项目也没有 `TestAppContext` 脚手架。IME 组合生命周期（COR-2/6/14）、焦点、
渲染三块目前完全测不到，只能人工用中文输入法验证。搭起来能一次解锁三个盲区。

### ENG-1 根治：把 gpui CJK 补丁推到自己的 zed fork
现状是"缓解"：补丁在版本控制里（`patches/gpui_macos-cjk-fallback.patch`），
缺失时 CI 显式失败。根治要建一个 GitHub fork 并用 `[patch]` 指过去——
需要一个方向决策（fork 维护成本），没有擅自代做。

### DSP-10：EL 清行粒度过粗
`EL` 对整行发 Erase 即使只清了光标右侧，图片"任意相交即整幅删除"——
提示符重绘、`clear -x` 会让图片瞬间消失。精细化需要给 Erase 事件带列范围
（vendored 事件结构改动），收益低，刻意未排期。

### ENG-3：`block v0.1.6` future-incompat
经 `cocoa v0.26`（本项目直接依赖 + gpui 传递依赖）引入，将被未来 Rust 拒绝编译。
无法在本仓库内解决：需等 zed 上游迁移到 objc2 生态，或随 gpui 升级消失。
当前被 `rust-toolchain.toml` 固定 1.95.0 掩盖，属已知约束。

## 战略决策项（不排期，需先讨论方向）

见 `docs/project-review/06-work-plan.md` 第四节：

1. **影子输入行模型换地基**（COR-10）：字节嗅探是原理性启发式，↑/↓ 历史、
   Tab 补全、`Ctrl-R`、fzf widget 永远补不完。ground truth 只能来自 shell 自报
   （OSC 133 / zsh ZLE hook）。建议先做增量校正 + OSC 133 原型，不推倒重写。
2. **主屏 scrollback 回看**：snapshot tab（Cmd+Shift+S）已覆盖主要需求。
   若要做真回看，**必须先修 ARCH-1**（`refresh_snapshot` 网格行不加
   `display_offset` 而光标行加了），否则回看时整屏内容消失且光标错位。

## 已完成（2026-07-26 审查修复轮，51 项缺陷中 45 项）

阶段 0–5 的完整清单与提交号见 `docs/project-review/07-progress.md`。
旧 TODO 的 11 项对应关系：#1 逐字符 shape（PERF-1b）、#2 模块拆分（更早完成）、
#3 new() 重复（更早完成）、#4 宽字符光标回退（COR-4，按**字符数**修，非旧方案）、
#5 debug HTTP 加固（SEC-1）、#6 indexed colors（更早完成）、#7 测量缓存（PERF-1c）、
#8 reader 错误记录（ROB-3）、#9 Cmd+C 复制（选区松开即复制）、#10 滚动回看（见战略
决策 2）、#11 input_line 同步（见战略决策 1）。
