# 终端图像渲染与 Atuin 收尾备忘录

## 背景

这轮工作最开始是为了排查终端里的图片渲染。目标是让 SIXEL 兼容的图片输出更接近
kitty 的内联图片体验：

- 图片要占据真实的终端布局空间。
- 图片后续输出的文本要在图片下方继续换行，而不是覆盖图片。
- 图片要跟随普通终端内容一起向上滚动。
- 图片不能画到应用自己的 tab/title 区域上。
- SIXEL 调色板解码后的颜色要尽量贴近源图。

SIXEL 基本可用后，又出现了一个独立症状：`Ctrl+R` 交互搜索界面没有预期的颜色。
最开始的判断是 zsh/readline 的 reverse-search 高亮可能丢了 SGR 样式，因为当时
`CellSnapshot` 只保留前景色、背景色和链接，没有保存 bold/italic/underline/strikeout
这些样式 flag。于是先补了终端样式 flag 的传递和渲染。

后续继续诊断后确认，这个 `Ctrl+R` 并不是 zsh 内置 reverse search，而是 Atuin：

```text
"^R" atuin-search
"^R" atuin-search-viins
```

Atuin 的 zle widget 最终执行的是：

```zsh
ATUIN_SHELL=zsh ATUIN_LOG=error ATUIN_QUERY=$BUFFER atuin search ... -i
```

这次 Atuin 没有颜色的真实原因是：启动开发窗口的父进程环境里有 `NO_COLOR=1`，
rterminal 创建 PTY 子 shell 时继承了这个变量。Atuin 遵守 `NO_COLOR` 约定，因此在
TUI 里主动关闭颜色。用 `env -u NO_COLOR` 启动的验证窗口可以恢复 Atuin 颜色。

## 最终决策

- 保留 SIXEL 解码和渲染路径。这是本轮主功能，并且已经有解析、布局占位、滚动跟随、
  PTY 像素尺寸等回归测试。
- 保留 bold/italic/underline/undercurl/strikeout 的文本样式传递。它不是 Atuin 颜色
  问题的最终根因，但属于正确的终端行为，也修复了其他 SGR 样式显示缺口。
- 保留 PTY 子 shell 默认 `TERM=xterm-256color`，并提供 `AGENT_TUI_TERM` 覆盖。终端
  模拟器应该声明自己的能力，而不是盲目继承启动进程外层的 `screen-256color` 或 tmux
  环境。
- 给子 shell 设置 `COLORTERM=truecolor` 和 `TERM_PROGRAM=rterminal`，方便 CLI 工具识别
  颜色能力和终端宿主。
- 创建子 shell 前移除 `NO_COLOR`。父进程可以为了日志稳定设置 `NO_COLOR`，但这个策略
  不应该泄漏进交互式终端模拟器会话。

## 权衡

- SIXEL 渲染采用务实方案：解码后的图片作为 `RenderImage` 单独保存并由 GPUI 绘制，
  终端网格则通过合成的光标移动和换行序列预留布局空间。这样避免大幅侵入 vendored
  terminal grid，但图片生命周期也因此和 cell 生命周期分离。
- 图片最多保留 128 张，用来限制内存增长；暂时没有实现完整图形协议的存储模型。
- 图片裁剪通过终端画布上的 `ContentMask` 实现。它能解决覆盖 tab/title 区域的问题，
  但还没有实现更精细的滚动区域级裁剪语义。
- 文本样式目前按 cell 渲染，没有合并相邻同样式 run。这样实现简单，也贴合现有渲染
  结构；后续如果需要性能优化，可以按相同样式聚合。
- `TERM=xterm-256color` 是保守兼容默认值，不代表 rterminal 的完整能力。`AGENT_TUI_TERM`
  留给后续实验自定义 terminfo。
- 移除 `NO_COLOR` 是产品层面的选择：从 rterminal 启动的交互 shell 默认应该有颜色，即使
  父进程为了自己的日志禁用了颜色。

## 关于 Alacritty 与 Kitty 图形协议

这轮讨论里还确认了一个边界：项目虽然基于 `alacritty_terminal`，但这并不意味着可以
直接获得 Kitty 图形协议支持。

- 我们使用的是 `alacritty_terminal` 这个终端解析和状态库，不是完整 Alacritty app 的
  渲染器。
- 上游 Alacritty 主线目前没有合并完整图形支持。长期开放的图形相关工作主要是 SIXEL，
  例如 `Support for graphics in the terminal #4763` 和 `Add support for libsixel #910`。
- SIXEL 和 Kitty Graphics Protocol 是两套不同协议。SIXEL 是 DCS 序列；Kitty 图形协议
  是 APC 形式的 `ESC _ G ... ESC \`，控制数据是 key/value，payload 通常是 base64，并且
  包含 image id、placement、delete、query、分片、文件/共享内存传输、动画、Unicode
  placeholder 等更完整的生命周期语义。
- 因此，Alacritty 的基础对文本终端很强，但不能把 Kitty 图形协议当成上游现成能力。

后续如果要接近 Kitty 的图片体验，建议方向是：保留当前 SIXEL 作为 fallback，同时实现
Kitty Graphics Protocol 的最小可用子集。优先目标可以是跑通 `kitten icat` 或现代 TUI
文件管理器的图片预览；完整追齐 Kitty 语义会是更大的工程。

## 逐文件修改

### `Cargo.toml`

- 添加 `image` crate 依赖。
- 用途：把 SIXEL 解码得到的 RGBA 像素缓冲转换为 GPUI 可绘制的 image frame。

### `Cargo.lock`

- 锁定新增的 `image` 依赖。

### `src/pty.rs`

- 扩展 `PtySession::spawn`，增加 `pixel_width` 和 `pixel_height` 参数。
- 把真实像素尺寸传给 `portable_pty::PtySize`。
- 设置子 shell 环境：
  - `TERM` 默认是 `xterm-256color`。
  - `AGENT_TUI_TERM` 可以覆盖默认 `TERM`。
  - `COLORTERM=truecolor`。
  - `TERM_PROGRAM=rterminal`。
  - spawn 前移除 `NO_COLOR`。
- 增加一个小测试，记录默认 child terminal type。
- 动机：
  - SIXEL 工具需要非零 PTY 像素尺寸才能正确决定输出尺寸。
  - Atuin 这类交互工具不应该继承 agent 环境里的 `NO_COLOR=1`。
  - 终端 app 不应该继承外层 tmux/screen 的 `TERM`。

### `src/terminal.rs`

- 增加 `TerminalImage`，记录已渲染图片状态：
  - 行/列锚点。
  - 占用的 cell 宽高。
  - GPUI `RenderImage`。
- 增加 `SixelStreamParser`，把普通 PTY 字节和 DCS SIXEL/tmux passthrough payload 分开。
- 集成 vendored SIXEL 解码：
  - 直接 DCS：`ESC P q ... ESC \`。
  - tmux passthrough 包裹的 SIXEL。
- 修正 tmux passthrough 的 DCS 流解析：
  - tmux 会把内层 SIXEL 的 `ESC` 转义成 `ESC ESC`。
  - 内层结束符因此会以 `ESC ESC \` 出现在外层 DCS payload 中。
  - parser 必须保留这段 payload，不能把它误判成外层 `ESC \` 结束符。
- 增加图片保存和布局预留：
  - 把解码后的 SIXEL 数据转成 `RenderImage`。
  - 根据图片像素尺寸和当前 cell metrics 计算占用 cols/rows。
  - 推进终端布局，让后续文本从图片占位后面或下方开始。
  - 当终端可见区域发生滚动时同步移动图片锚点。
  - 图片完全滚出可见区域后删除。
- 消费 vendored terminal 发出的滚动区域事件：
  - 不追踪 tmux pane 的内部布局，也不解析 tmux pane 切换、缩放、放大这些语义。
  - 只处理外层终端协议已经表达出的事实：某个屏幕区域向上或向下滚动了多少行。
  - 图片如果和该区域相交，就跟随同样的 delta 移动；滚出区域后删除。
  - 这覆盖 tmux、vim、less、全屏 TUI、alternate screen 等不会增长外层 scrollback 的场景。
- 消费 vendored terminal 发出的擦除区域事件：
  - `Ctrl-L`、`clear`、tmux redraw 等通常会落到终端的 clear screen / clear line 指令。
  - 图片如果和被擦除的可见行区域相交，就同步删除。
  - 这样图片不会在终端网格已经清空后继续作为 overlay 残留。
- 增加 PTY 像素尺寸计算和 resize 传播。
- 扩展 `CellSnapshot`，保存这些样式 flag：
  - `bold`
  - `italic`
  - `underline`
  - `undercurl`
  - `strikethrough`
- 在 live screen snapshot 和保存的 snapshot 数据里填充样式 flag。
- 增加测试覆盖：
  - SIXEL stream 解析。
  - tmux passthrough 中内层 `ESC ESC \` 不会提前截断外层 DCS。
  - SIXEL 布局占位。
  - 图片跟随 scrollback。
  - 图片跟随可见滚动区域移动。
  - 图片在终端擦除区域时被删除。
  - PTY 像素尺寸。
  - 文本样式 flag 保留。

### `src/render.rs`

- 为保留的 terminal images 增加 GPUI 图片绘制。
- 用终端画布范围内的 `ContentMask` 绘制图片，避免图片覆盖 tab/title 区域。
- 支持图片滚动到负 row 后仍然部分可见。
- 在 shape cell 文本时应用样式 flag：
  - `FontWeight::BOLD` 渲染 bold。
  - `FontStyle::Italic` 渲染 italic。
  - `UnderlineStyle` 渲染 underline/undercurl。
  - `StrikethroughStyle` 渲染 strikethrough。
- 保留链接样式，链接文本仍优先使用 link color。

### `src/snapshot_tab.rs`

- 在保存的 snapshot tab 里同步渲染文本样式：
  - bold
  - italic
  - underline/undercurl
  - strikethrough
- 目的：避免 live terminal 和 snapshot view 对同一段 styled terminal content 显示不一致。

### `vendor/alacritty_terminal/src/sixel.rs`

- 新增轻量 SIXEL decoder module。
- 支持：
  - raster attributes。
  - repeat introducer。
  - palette selection。
  - RGB palette entries。
  - HLS palette entries。
  - tmux passthrough 提取。
- 输出 `SixelImage`，包含 row/column 锚点、像素尺寸和 RGBA bytes。
- 包含 decoder 单元测试。

### `vendor/alacritty_terminal/src/lib.rs`

- 导出新的 `sixel` module。

### `vendor/alacritty_terminal/src/event.rs`

- 增加内部 `Scroll` 事件，描述终端可见滚动区域：
  - `region_top`
  - `region_bottom`
  - `delta`
- 目的：让上层渲染模型可以把图片锚点和终端网格滚动同步，而不是只依赖 scrollback
  增长。
- 增加内部 `Erase` 事件，描述终端可见擦除区域。
- 目的：让上层渲染模型可以在 `Ctrl-L`、clear screen、clear line 等操作后删除相交图片。

### `vendor/alacritty_terminal/src/term/mod.rs`

- 更新 primary device attributes，声明支持 SIXEL。
- 目的：让会查询终端能力的工具选择 SIXEL 输出。
- 在 `scroll_up_relative` 和 `scroll_down_relative` 里发送内部 `Scroll` 事件。
- 目的：让 tmux/full-screen TUI 这类只滚动可见区域、不增长外层 scrollback 的输出，也能
  带动图片一起滚动。
- 在 `clear_screen` 和 `clear_line` 里发送内部 `Erase` 事件。
- 目的：让清屏/清行同时清理图片 overlay。

## 验证

最终代码路径通过：

```text
cargo check
cargo test
cargo build
```

手动验证包括：

- SIXEL 图片显示和换行。
- 图片跟随文本滚动。
- 图片被裁剪在终端画布内，不再覆盖 tab 栏。
- 移除 child shell 环境里的 `NO_COLOR` 后，Atuin `Ctrl+R` 颜色恢复。

## 已知不足

- 图片生命周期仍然独立于终端 cell，不是完整的 graphics protocol model。
- 图片 scrollback 只依赖当前内存里的 retained image list，没有持久化模型。
- 图片擦除和替换语义还比较有限。
- 文本渲染仍然按 cell shape，后续可以按连续相同样式聚合优化。
- `xterm-256color` 是保守默认值，不是 rterminal 自己的 terminfo entry。
