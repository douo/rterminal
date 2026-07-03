# 输入法状态光标颜色备忘

日期：2026-07-04

## 背景

用户希望终端光标颜色能够反映当前系统输入法状态，用于快速区分英文输入法和中文输入法。这个需求的目标不是增强终端协议，也不是改变 PTY 输入行为，而是提供个人使用便利性。

因此设计时必须避免让“输入法状态提示”污染终端核心：

- PTY、终端解析、按键编码不应该依赖 macOS 输入法 API。
- 渲染层不应该直接查询系统输入法。
- 后续如果增加更多个人便利功能，应当可以放在同一类外围状态中，而不是继续往终端核心字段里散落平台逻辑。

## 当前光标颜色来源

修改前，光标颜色由 `src/render.rs` 中的渲染调色板固定决定：

- `RenderPalette::cursor_bg`
- `Theme::Default` 和 `Theme::EyeCare` 当前都使用 `rgba(0xffea00a6).into()`

也就是说，光标形状和位置来自终端状态，但光标颜色本质上是一个固定主题颜色，不会随系统输入法变化。

## 第一性原理拆解

这个需求实际包含三个不同层次的问题：

1. 系统事实：当前 macOS 输入源是什么。
2. 使用者意图：这个输入源应当被视为英文/拉丁输入，还是中文/CJK 输入。
3. UI 表达：在不改变终端语义的前提下，用什么颜色提示该状态。

这三层不能混在一起。系统查询属于平台适配；英文/中文分类属于便利性状态；光标颜色只是渲染时对状态的表达。

因此本次实现采用单向依赖：

```text
macOS 输入源查询 -> convenience 状态 -> render 光标颜色
```

并避免反向依赖：

```text
PTY / 终端解析 / 输入字节路径 -> macOS 输入源查询
```

## 修改

新增 `src/convenience/` 作为个人便利性功能的归属位置：

- `src/convenience/input_method.rs`
  - 定义 `InputMode::{Latin, Cjk, Unknown}`。
  - macOS 下使用 Carbon TIS 查询当前键盘输入源。
  - macOS 下通过 `kTISNotifySelectedKeyboardInputSourceChanged` 监听输入源切换。
  - 监听器使用 RAII 管理，创建时注册 CF distributed notification，drop 时注销。
  - 根据输入源 ID、输入模式 ID、语言列表做轻量分类。
  - 非 macOS 平台返回 `Unknown`。

- `src/convenience/cursor_indicator.rs`
  - 提供纯函数 `cursor_color_for_input_mode(base, input_mode)`。
  - 英文/未知输入法沿用主题光标色。
  - CJK 输入法使用绿色光标色，和当前黄色主题光标形成明显区分。

- `src/convenience/mod.rs`
  - 新增 `ConvenienceState`。
  - 聚合当前输入法状态，并提供刷新节流。
  - 这是后续个人便利功能可以继续扩展的位置。

接入点保持在 UI 边缘：

- `src/terminal.rs`
  - `AgentTerminal` 持有 `ConvenienceState`。
  - 新增 `refresh_convenience_state` 作为统一刷新入口。
  - 终端获得焦点时先刷新一次当前输入法状态，再启动输入法变更监听任务。
  - 终端失去焦点时取消监听任务，任务取消会 drop 掉平台监听器。

- `src/tabs.rs`
  - active tab 被聚焦时刷新便利性状态。
  - 覆盖 FlashSpace 切回窗口、tab 切换、重新聚焦当前 tab 等路径。

- `src/input.rs`
  - 按键进入终端处理路径时刷新输入法状态。
  - 覆盖用户刚切换输入法后立刻输入的场景。

- `src/render.rs`
  - 渲染层只读取 `ConvenienceState` 中的输入法模式。
  - 光标绘制使用 `cursor_color_for_input_mode` 得到最终颜色。
  - 光标拖尾颜色跟随实际光标颜色，避免主光标和拖尾状态不一致。

## 取舍

这次接入了 macOS 输入源变更通知，但没有启动全局后台常驻轮询。

原因是该功能定位为低侵入的个人便利性提示。输入法状态只有在终端获得焦点时才需要影响终端光标；失焦后继续监听没有实际 UI 价值，反而会让一个视觉提示功能变成长期运行服务。

当前版本采用“聚焦期间监听 + 事件兜底”的方式：

- 创建终端时刷新一次。
- focus-in 时先刷新当前状态，再注册 macOS 输入源变更通知。
- focus-out 时取消监听。
- macOS 输入源变更通知到达后，在 GPUI 实体上下文里重新读取当前输入法状态并更新光标。
- active tab 聚焦时刷新。
- keydown 时刷新。
- render 时有 250ms 的轻量兜底刷新，但只有状态变化才触发重新绘制。

通知回调不直接更新终端，也不持有 `AgentTerminal` 或 `Window`。它只向有界 channel 发出“输入法可能变化”的信号；实际读取当前输入源、分类、更新光标颜色都回到 `ConvenienceState` 和 GPUI 实体上下文完成。

## 验证建议

手动验证路径：

1. 启动 Agent Terminal。
2. 光标在英文输入法下保持原主题黄色。
3. 切换到豆包输入法或其他中文输入法。
4. 聚焦终端，确认 focus-in 会立即刷新当前输入法状态。
5. 观察光标变为绿色。
6. 在终端仍然聚焦时切回英文输入法，观察光标恢复主题黄色。
7. 切到其他应用后再切换输入法，确认 Agent Terminal 不在失焦期间持续监听；切回终端后会先读取当前状态并更新光标。

重点确认：

- 中文输入法状态下，beam/block/underline/hollow block 光标都使用提示色。
- 光标拖尾颜色和当前光标颜色一致。
- PTY 输入、IME 候选词提交、tab 切换、窗口激活行为不受影响。
