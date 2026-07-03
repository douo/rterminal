# IME 焦点恢复备忘

日期：2026-07-04

## 现象

在当前输入法为豆包输入法时，Agent Terminal 被切回前台后偶尔会出现键盘输入没有进入终端的情况。更准确地说：

- 输入法候选词窗口仍然会弹出。
- shell 提示符位置没有可见光标。
- 选中的候选词没有提交到 PTY 输入行。
- 候选词窗口出现在窗口左下角，而不是终端光标附近。
- 切回英文输入法、点击终端、或者切到其他应用再切回来，通常可以恢复输入。

该问题不是必现，但用户反馈频率较高，并且与使用 FlashSpace 切换前台应用相关。

## 观察

候选词窗口出现在左下角是最关键的线索。在 GPUI 的 macOS 桥接中，AppKit 会向当前激活的 `NSTextInputClient` 查询 `firstRectForCharacterRange`。如果当前没有有效的 input handler，或者 handler 无法提供字符坐标，AppKit 会退回到空/零矩形，候选词窗口就会落在左下角附近。

本项目在 macOS 上已经把普通可打印按键让给 GPUI 文本输入系统处理，让 IME 组合输入走 `setMarkedText` / `insertText`。因此问题不像是 PTY 字节编码错误，更像是焦点和 input handler 同步问题：

- 鼠标点击会调用 `window.focus(&terminal.focus_handle, cx)`，因此常常可以恢复终端输入。
- tab 切换会显式聚焦 active tab。
- FlashSpace 通过 `NSRunningApplication` 激活和 Accessibility raise/focus 操作切换应用/窗口，可能把窗口带到前台，但不会经过终端的 mouse-down 聚焦路径。

因此，FlashSpace 更像是触发器，不是根因。根因是 Agent Terminal 过度依赖点击或 tab 切换路径恢复 focus 和 IME 坐标。

## 修改

这次修改让 Agent Terminal 在窗口激活和 IME 状态变化时主动恢复焦点与候选词坐标：

- `src/tabs.rs`
  - 订阅窗口激活事件。
  - 当窗口重新变为 active，且没有处于 tab 重命名状态时，重新聚焦当前 active tab。
  - 这覆盖了外部工具激活窗口但没有鼠标点击的路径。

- `src/terminal.rs`
  - 终端 focus-in 时调用 `window.invalidate_character_coordinates()`。
  - 这会要求 GPUI/AppKit 刷新 IME 候选词窗口坐标。

- `src/input.rs`
  - 在 IME mark、commit、unmark 回调后刷新 character coordinates。
  - 这与 Zed terminal 的输入路径保持一致，避免组合输入过程中候选词坐标缓存陈旧。

## 验证

修改后执行过以下命令：

```bash
cargo check
cargo test
cargo run -- --self-check
```

该问题的手动复现仍然具有随机性。较容易触发的压力路径是：

1. 将 macOS 输入法切到豆包输入法。
2. 让 Agent Terminal 停在 shell prompt。
3. 使用 FlashSpace 快捷键反复切走并切回 Agent Terminal。
4. 切回后不要点击终端。
5. 立刻输入拼音并选择候选词。

修复前的典型失败现象是候选词窗口落在左下角，并且候选词没有进入终端。修复后，窗口激活时 active tab 会重新获得 focus，IME 组合输入状态变化时也会刷新候选词坐标。
