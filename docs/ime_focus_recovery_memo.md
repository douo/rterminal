# IME 焦点恢复备忘

日期：2026-07-04

## 现象

在当前输入法为豆包输入法时，Agent Terminal 被切回前台后偶尔会出现键盘输入没有进入终端的情况。更准确地说：

- 输入法候选词窗口仍然会弹出。
- shell 提示符位置没有可见光标。
- 选中的候选词没有提交到 PTY 输入行。
- 候选词窗口出现在窗口左下角，而不是终端光标附近。
- 切回英文输入法、点击终端、或者切到其他应用再切回来，通常可以恢复输入。

最初该问题不是必现，但用户后来找到了稳定复现路径：

1. 在 Agent Terminal 开发版窗口里切到豆包输入法并输入。
2. 用触控板切到另一个应用。
3. 在另一个应用里切回英文输入法。
4. 再用触控板切回 Agent Terminal。
5. 此时 macOS 状态栏显示英文输入法，Agent Terminal 的光标颜色也显示英文输入法颜色，但按键仍会激活豆包输入法。
6. 豆包候选词窗口没有跟随终端光标，而是停在窗口左下角。

## 观察

候选词窗口出现在左下角是最关键的线索。在 GPUI 的 macOS 桥接中，AppKit 会向当前激活的 `NSTextInputClient` 查询 `firstRectForCharacterRange`。如果当前没有有效的 input handler，或者 handler 无法提供字符坐标，AppKit 会退回到空/零矩形，候选词窗口就会落在左下角附近。

本项目在 macOS 上已经把普通可打印按键让给 GPUI 文本输入系统处理，让 IME 组合输入走 `setMarkedText` / `insertText`。因此问题不像是 PTY 字节编码错误。

后续补充的“光标颜色是英文，但实际按键仍进入豆包输入法”把问题进一步缩小了：

- 光标颜色来自 Carbon TIS 的全局当前输入源查询。
- 实际键盘输入由当前窗口 `NSView` 的 `NSTextInputContext` 处理。
- 这两个状态面可能短暂或长期不一致。
- 触控板切回窗口时，GPUI 的 focus handle 可能本来就已经是终端；`Window::focus` 在 focus handle 未变化时会直接返回，因此单纯“重新 focus 一次”不会重建 AppKit 输入上下文。

因此，FlashSpace 或触控板切换更像是触发器，不是根因。根因是 Agent Terminal 只刷新了 GPUI 焦点和 IME 坐标，没有显式同步当前窗口自己的 `NSTextInputContext`。

## 失败的中间判断

第一版修复把重点放在窗口激活时恢复 active tab focus，并在 focus-in 和下一帧调用 `window.invalidate_character_coordinates()`。这能覆盖一部分“没有坐标”的问题，但无法解决稳定复现路径：

- 如果 AppKit 文本输入上下文仍保留豆包输入源，刷新坐标不会把它切回英文。
- 如果 focus handle 没有变化，GPUI 不会重新派发完整 focus 变更。
- 如果失焦前存在 marked text，会继续影响下一次 `handleEvent`。

这个失败判断本身是有价值的：候选框左下角不能只理解为“坐标过期”，还可能是“旧输入上下文 + 无有效候选框位置”的组合症状。

## 修改

最终修改保持低侵入边界：终端核心仍只处理 PTY、渲染和 GPUI input handler；macOS 输入法上下文同步放在 `src/convenience/input_method.rs`，作为个人便利性服务的一部分。

- `src/convenience/input_method.rs`
  - 复用 Carbon TIS 查询当前系统输入源。
  - 通过 GPUI window 的 raw AppKit handle 获取当前 `NSView`。
  - 从 `NSView` 获取该窗口自己的 `NSTextInputContext`。
  - focus 恢复时调用 `discardMarkedText`，丢弃失焦前遗留的组合态。
  - 将 `NSTextInputContext.selectedKeyboardInputSource` 同步为当前系统输入源。
  - 调用 `invalidateCharacterCoordinates`，让 AppKit 重新查询候选框位置。

- `src/terminal.rs`
  - 新增统一入口 `restore_focus_and_ime_context`。
  - 终端 focus-in、tab focus、鼠标点击都走同一个恢复路径。
  - 恢复路径先启动输入法监听并读取当前输入法状态，再同步 AppKit 文本输入上下文。
  - 当终端 focus-out 时丢弃 marked text，降低旧组合态泄漏到下一次激活的概率。
  - 下一帧再次同步输入法状态和候选框坐标，覆盖 GPUI input handler 需要在 paint 阶段重新注册的时序。

- `src/tabs.rs`
  - 当窗口重新变为 active，且没有处于 tab 重命名状态时，重新聚焦当前 active tab。
  - active tab 聚焦时也会同步 IME 上下文，而不是只调用 `window.focus`。

- `src/input.rs`
  - 鼠标点击终端时走同一个 `restore_focus_and_ime_context`，覆盖用户点击恢复窗口的路径。

## 验证

修改后执行过以下命令：

```bash
cargo check
cargo test
cargo run -- --self-check
```

用户使用稳定复现路径验证：

1. 在开发版窗口用豆包输入法输入。
2. 用触控板切到另一个应用。
3. 在另一个应用切成英文输入法。
4. 再用触控板切回开发版窗口。
5. 直接按键盘输入。

修复前该路径可以稳定复现“状态栏和光标显示英文，但实际仍触发豆包候选词且候选框落在左下角”。修复后用户反馈该稳定路径已经无法复现。

## 经验总结

这次问题隐蔽，是因为表面现象横跨三层状态：

- 系统级输入源：Carbon TIS 查询到的是全局当前输入源，光标颜色依赖这个状态。
- 窗口级输入上下文：`NSTextInputContext` 才是真正接收按键并驱动输入法候选词的状态。
- 框架级输入 handler：GPUI 只在 focused element 绘制时注册 `InputHandler`，候选框坐标也依赖这层。

排障时不能把“输入法状态”当成一个单一事实。只要出现“状态栏/光标显示 A，但实际输入走 B”，就要立刻怀疑系统状态和窗口文本输入上下文分裂，而不是继续在 PTY 编码或渲染层找原因。

后续同类问题的优先检查顺序：

1. 先区分“展示状态”来自哪里，“实际输入状态”来自哪里。
2. 如果候选词窗口落在左下角，检查 `firstRectForCharacterRange` 是否拿到了有效 input handler 和有效 bounds。
3. 如果 `Window::focus` 没有触发恢复逻辑，检查 focus handle 是否其实没变。
4. 对 IME 类 bug，失焦时主动清理 marked text，聚焦时同步窗口级 `NSTextInputContext`，比只刷新候选框坐标更可靠。
5. 这类个人便利功能要继续放在 `convenience` 边界里，避免终端核心依赖 macOS 输入法细节。
