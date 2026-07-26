# 输入链路 Bug 审查报告

> 子报告 2/4 · 审查基线 `main @ c4301f9` · 只读分析
> 审查范围：`src/input.rs`、`src/keyboard.rs`、`src/convenience/{input_method,cursor_indicator,mod}.rs`、`src/text_utils.rs`、`src/input_log.rs`，交叉验证 `src/terminal.rs`、`src/render.rs`、`src/macos_ax.rs`、`src/pty.rs`、`src/debug_server.rs`，以及 gpui（rev 19c8363）中 `Keystroke::is_ime_in_progress` 与 `first_mouse` 的实际语义。按严重程度排序。

---

## 高严重度

### H1. Ctrl+标点/数字 被错误编码为无关控制字符（可意外执行命令）
- **位置**: `src/keyboard.rs:420-427`（`encode_printable_keystroke` 的 ctrl 分支）
- **问题**: 对任意单个 ASCII 键统一做 `ch.to_ascii_lowercase() & 0x1f`。该运算只对 `@ a-z [ \ ] ^ _ ?` 有意义，对标点和数字会产生完全错误的控制字节：
  - `Ctrl+-` → `0x2d & 0x1f = 0x0d`（**CR，等于按下 Enter，会直接执行当前 shell 命令行**；xterm 约定应发 `0x1f`）
  - `Ctrl+/` → `0x0f`（^O；应为 `0x1f`，Emacs/readline 的 undo 失效）
  - `Ctrl+3` → `0x13`（^S = XOFF，**在开启 ixon 的终端里直接冻结输出**）
  - `Ctrl+2` → `0x12`（^R，误触发 bash reverse-i-search；应为 NUL）
  - `Ctrl+;` → `0x1b`（ESC，会把 vim 从插入模式踢出）
- **触发**: 用户按下任意 Ctrl+非字母键。习惯 Emacs `C--`/`C-/` undo 或误触 `Ctrl+数字` 即可复现，其中 `Ctrl+-` 发送 CR 具有破坏性（执行已输入命令）。
- **严重程度**: **高**
- **建议**: 仅对 `@ a-z [ \ ] ^ _ space ?` 应用 0x1f 掩码，其余键回退为不发送或按 xterm 传统映射。

### H2. `unmarkText` 丢弃组合文本，违反 AppKit 提交语义
- **位置**: `src/input.rs:1720-1725`（`unmark_text` 直接置 `ime_marked_text = None`）
- **问题**: NSTextInputClient 规范要求 `unmarkText` 时"将 marked text 作为正常文本插入"。本实现直接丢弃，且从不向 PTY 写入。凡是 IME 走 `unmarkText`（而非 `insertText`）收尾的提交路径 —— 例如某些输入法在切换输入源、按特定键、或系统强制结束组合时 —— **用户已敲的整段拼音/汉字静默丢失**。
- **触发**: 拼音组合进行中切换输入法/触发系统级 unmark；具体 IME 依赖，但属于数据丢失类缺陷。
- **严重程度**: **高**（数据丢失；发生频率取决于 IME）
- **建议**: `unmark_text` 中若 `ime_marked_text` 非空，先 `insert_input_text_at_cursor` + `write_text_input` 再清除。

---

## 中严重度

### M1. SGR 鼠标 release 丢失按钮编号（且编码形态非法）
- **位置**: `src/input.rs:1322-1343`（`encode_mouse_report`）
- **问题**: SGR (1006) 模式下 release 事件被编码为 `\x1b[<{3+mods};x;y m`。SGR 协议的设计正是**release 保留原按钮号**、仅用 `m` 后缀区分；按钮 3 是 X10 遗留的"release/未知"。当前实现使 tmux/vim 等无法区分释放的是哪个键；`3+mods`（如 ctrl 释放时 `<19...m`）更是规范外组合。
- **触发**: 任何 SGR mouse mode 应用中的鼠标释放（tmux `mouse on` 即 SGR）。多数程序容错，但依赖 release 按钮号的拖拽/多键逻辑会错乱。
- **严重程度**: **中**
- **建议**: SGR 分支 release 时使用原 `button + mods`，仅切换 `M/m` 后缀。

### M2. `rewrite_terminal_input_line` 按 UTF-16 单元而非字符数发送左箭头（TODO #4 仍未修复）
- **位置**: `src/input.rs:441-446`（`tail = input_line_len_utf16() - input_cursor_utf16`，每单元发一个 `\x1b[D`）
- **问题**: zle/readline 对 `\x1b[D` 的语义是**后退一个字符**（与显示列宽无关）。因此：
  - CJK（BMP，1 UTF-16 单元）恰好正确 —— TODO #4 中"需要按 wcwidth 列宽发送"的方案判断反而是**错误方向**，按列发会把 CJK 多退一倍；
  - **emoji/增补平面字符（2 UTF-16 单元）会多发一个左箭头**，光标每个 emoji 偏左一个字符，AX 语音改写（`apply_external_ax_input_state`, input.rs:1161）触发重写后 shell 光标与模型光标失同步。
- **触发**: input_line 含 emoji 且光标不在行尾时发生 AX override 重写。
- **严重程度**: **中**
- **建议**: tail 改为按 `char` 计数（`text[cursor_byte..].chars().count()`）。**同时修正 TODO.md #4 的错误方案描述。**

### M3. IME 候选窗定位把 UTF-16 偏移当作列数（宽字符错位）
- **位置**: `src/input.rs:1784-1798`（`bounds_for_range`：`origin.x += cell_width * range_utf16.start`）
- **问题**: 与渲染路径不一致：`render.rs:480-497` 用 `shape_line` 按真实字形宽度绘制 marked text，而候选窗矩形按"每 UTF-16 单元一个半角格"推算。日文假名组合、拼音已上屏部分含汉字时（每字符 1 单元但占 2 列），候选窗横向偏移可达组合长度的一半；含 emoji 时反向偏移。
- **触发**: 任何 marked text 含全角字符的组合过程。
- **严重程度**: **中**
- **建议**: 用 marked text 前缀实际列宽（或 shape 结果）换算 x 偏移。

### M4. 组合进行中 Cmd+V 粘贴 / 文件拖放不处理 marked text
- **位置**: `src/input.rs:920-983`（paste 路径）、`src/input.rs:281-304`（`write_dropped_paths_input`）
- **问题**: 组合（`ime_marked_text.is_some()`）期间按 Cmd+V：粘贴文本立即写入 PTY 与 `input_line`，而 marked text 仍悬挂 —— 渲染层继续在光标处画组合串并隐藏真实光标（render.rs:456-500），随后组合提交时文本顺序与用户所见错乱；模型 `input_line` 的插入点也与最终 PTY 字节序不一致。文件拖放同理。对照：鼠标按下路径（input.rs:626 → terminal.rs:889）会主动 discard，粘贴路径遗漏了。
- **触发**: 拼音敲一半按 Cmd+V 或拖入文件。
- **严重程度**: **中**
- **建议**: 粘贴/拖放入口先提交或 discard marked text（与 `refresh_ime_context` 一致）。

### M5. Ctrl+Alt+字母 丢失 Meta（ESC）前缀
- **位置**: `src/keyboard.rs:100`（alt 分支条件 `modifiers.alt && !modifiers.control` 排除了 ctrl+alt）与 `keyboard.rs:420-427`（ctrl 分支忽略 alt）
- **问题**: `Ctrl+Alt+x` 只发 `0x18`，而 xterm 传统是 `ESC` + ctrl-char（`\x1b\x18`）。Emacs `C-M-x`、readline `C-M-`* 系列绑定全部失效并被误认为纯 Ctrl 键。
- **触发**: 任意 Ctrl+Alt+字母 组合（option_as_meta 开启时）。
- **严重程度**: **中**
- **建议**: ctrl 分支在 `modifiers.alt` 时前缀 `0x1b`。

### M6. c4301f9 焦点激活鼠标 guard 可滞留，吞掉后续一次 mouse-up
- **位置**: `src/input.rs:52-74`（`FocusActivationMouseGuard`）、`src/input.rs:695-710`（on_mouse_up 中 guard 检查先于选区处理）
- **问题**: guard 在 first_mouse 按下时置位，仅靠**同一表面收到 mouse-up** 才清除。gpui 的鼠标监听绑定在 terminal_surface div 上（render.rs:258-263）；若激活点击按在终端区、**拖到标题栏/状态栏/窗外释放**，surface 收不到 up，`guard.button` 滞留为 `Some(Left)`。后果链：下一次正常左键点击的 release 被吞（input.rs:699-710 提前 return）——
  - (a) shift 选区无法结束/复制，`selection_mode_active` 残留（input.rs:712 不可达），之后任意无 shift 左键拖动会意外"复活"选区更新（input.rs:502-517）；
  - (b) mouse-mode 应用（tmux）收到 press 却收不到 release，出现按钮卡死。
- **触发**: 点击未激活窗口的终端区域并把鼠标拖出表面后松开，之后的第一次点击行为异常。
- **严重程度**: **中**（边界场景但后果链长，且是最近提交引入）
- **建议**: 在 `on_mouse_down` 收到新的非 first_mouse 按下、或 focus-out 时强制清空 guard。
- **另注**（同提交的设计取舍，非 bug）：first_mouse 抑制只覆盖左键；激活窗口的第一次**右/中键**仍会直通上报给 mouse-mode 应用（input.rs:53-54），行为不一致。Cmd+点击链接在激活点击时也被吞（属预期内取舍）。

### M7. `--no-option-as-meta` 时 Option 死键双重输入
- **位置**: `src/keyboard.rs:123-127`（alt 分支兜底 `key.chars().count()==1` → ESC+key）与 `src/keyboard.rs:444-473`（`should_defer_to_text_input` 要求 `key_char` 非空才 defer）
- **问题**: `encode_special_keystroke` 的 alt 分支不检查 `is_ime_in_progress`（gpui 中 alt 按下时该函数也恒为 false，见 gpui keystroke.rs:229-235）。关闭 option-as-meta 后，死键如 `Option+E`（key_char 为 None，macOS 同时启动重音组合）：defer 判定失败 → 走编码路径 → 兜底发出 `\x1b e`；同时 IME 的 marked text 组合继续，提交时再插入 `é`。PTY 收到 ESC+e **加** 组合结果，双重输入且 ESC 可能触发 vi-mode 切换。
- **触发**: `--no-option-as-meta` + 任意死键（Option+E/I/N/U/`）。
- **严重程度**: **中**（非默认配置）
- **建议**: 编码函数感知 option_as_meta，或 alt 分支在 `key_char.is_none()` 时返回 None 交给 IME。

### M8. input_line 状态跟踪失同步（TODO #11 仍存在，且新增了失同步入口）
- **位置**: `src/input.rs:399-432`（`apply_terminal_bytes_to_input_line`）
- **现状确认**: 以下场景模型与 shell 仍会失同步：
  1. `↑/↓` 历史导航、`\x1b[A/B` 直接被 `bytes.first()==0x1b` 早退忽略（input.rs:421-423）—— TODO 原述场景，**未修复**；
  2. Tab 补全：`\t` 是控制字符被过滤（input.rs:425-429），shell 行变了模型不知道；
  3. Shift+Enter 发 `\n`（keyboard.rs:85）：zle 视为 accept-line 执行，但模型只匹配 `b"\r"`（input.rs:401），行不清空；
  4. kitty disambiguate 模式下 `Ctrl+C` 等编码为 CSI-u（ESC 前缀）被忽略，模型不清行；
  5. **新入口**：多行粘贴/确认后把含 `\n` 的整块原文插入模型（input.rs:958、977），而 shell 已逐行执行；alt-screen（vim 等）中键入也持续写入模型。
- **缓解**: 后来加入的 `ax_text_matches_screen_context` 屏幕匹配防护（input.rs:1248-1286）+ `allow_ax_override` 时间窗（input.rs:1229-1242）大幅降低了错误重写的概率，但模型本身仍是脏的（AX 辅助功能读到错误值）。
- **严重程度**: **中**（有防护缓解，但仍是 AX/语音改写功能的正确性根基）

### M9. Cmd+A 无条件向 PTY 注入 0x15
- **位置**: `src/input.rs:909-918`
- **问题**: Cmd+A 被定义为"清空当前输入行"，直接发 `0x15`（Ctrl-U），不检查 alt-screen/前台程序。在 vim 插入模式中删除整行文本、在 less/普通 TUI 中产生意外输入。
- **触发**: 在任何全屏应用中习惯性按 Cmd+A。
- **严重程度**: **中低**
- **建议**: `snapshot.alt_screen` 时跳过或不发送。

---

## 低严重度

### L1. `--double-width-chars` 强制双宽时，鼠标坐标/链接点击/IME 光标框与渲染错位
- **位置**: `src/input.rs:810-869`（`mouse_grid_point` 纯线性除法）、`src/input.rs:604-617`（`link_at_position`）、`src/input.rs:449-473`（`ime_cursor_bounds`）
- **问题**: 渲染路径对 `expands_layout` 单元会累加 `extra_visual_cols` 平移后续单元（render.rs:54-70、415-417），链接 hover 高亮也用同一算法（render.rs:97-129）；但点击→格坐标换算、Cmd+点击开链接、IME 光标定位都不考虑该位移。含强制双宽字符的行内，hover 高亮与实际点击目标不一致（可打开相邻单元的链接或落空），选区、鼠标上报坐标同样右偏。
- **触发**: 仅使用 `--double-width-chars` CLI 选项时。
- **严重程度**: **低**（非默认功能）
- **备注**: 与子报告 3/4 的 M6 是同一缺陷，从渲染侧与输入侧分别观察到。

### L2. kitty 协议若干规范偏差
- `src/keyboard.rs:305-323`：`kitty_associated_text_field` 不检查 event_type —— REPORT_ALL+REPORT_EVENT_TYPES+REPORT_ASSOCIATED_TEXT 下 **release 事件也附带 text 字段**，规范规定 text 仅随 press；
- `src/keyboard.rs:133-184`：仅 DISAMBIGUATE+REPORT_EVENT_TYPES（无 REPORT_ALL）时，普通可打印键的 release 不上报（规范上 flag 2 应覆盖所有键的 release）；
- `src/keyboard.rs:381`：legacy 修饰 F3 编码为 `\x1b[1;{m}R`，与 DSR 光标位置应答 `\x1b[{r};{c}R` 形态冲突（现代 xterm 已改用 `CSI 13;m~`，kitty 路径 keyboard.rs:350 是对的，legacy 路径没改）。
- **严重程度**: **低**

### L3. `replace_text_in_range` 忽略 `replacement_range`
- **位置**: `src/input.rs:1727-1764`（注释已声明为已知限制）
- **问题**: 听写、自动纠错、Japanese 再变换等以非空 replacementRange 替换**已提交**文本的路径会退化为追加，导致重复输入。对纯拼音组合无影响（marked text 从未写入 PTY，忽略 range 恰好正确）。
- **严重程度**: **低**（记录为限制即可）

### L4. 每次 mouse down 都 discard 组合而非提交
- **位置**: `src/input.rs:626` → `src/terminal.rs:879-900`
- **问题**: 组合中点击终端任意位置，`refresh_ime_context(discard=true)` 直接丢弃拼音。macOS 惯例（Terminal.app/TextEdit）是点击时**提交** marked text。属可辩护的设计（0de5d1b 有意为之），但与 H2 一起构成"组合内容易静默丢失"的体验问题。
- **严重程度**: **低**

### L5. 其他小项
- `src/input.rs:1683-1687`：`text_for_range` 对 `start >= end` 的请求返回**整个** marked text 并把 adjusted_range 改成 `0..len`，对空 range 查询语义不符（应返回空串）；
- `src/input.rs:1694-1708`：`selected_text_range` 恒返回 `0..0`，组合期间 IME 询问选区得到退化答案，候选窗高亮位置只能依赖 M3 中已有偏差的 `bounds_for_range`；
- `src/render.rs:236`：光标灰化判据用 `window_active` 而非 `focused`（a9f97f3），同窗口内焦点转移（如 tabs 重命名框）时光标不变灰 —— 目前 UI 下影响甚微。

---

## 经核查无问题的点（供后续免复查）

- **UTF-8 截断边界**：PTY 字节流交给 alacritty 的 `Processor::advance`（terminal.rs:629-631），vte 状态机自身处理跨 chunk 截断的 UTF-8，未发现问题。
- `utf16_to_byte_index`（text_utils.rs:3-20）对落在代理对中间的索引会截断到字符起点，插入始终在 char 边界，安全。
- **空 grid / resize 路径**有 `MIN_COLS/MIN_ROWS` 与全面 clamp，未发现问题。
- **括号粘贴防注入**（input.rs:270-275）剥离 ESC 字节，`\x1b[201~` 逃逸不可行，实现正确。
  - 注意：非 bracketed-paste 分支（input.rs:276-278）**未做**同样的剥离，见子报告 4/4 的低危观察项。

---

## TODO.md 输入相关条目现状对照

| 条目 | 现状 |
|---|---|
| #2 模块拆分 | **已完成**（input/keyboard/pty/render/debug_server 等已拆出） |
| #4 rewrite_terminal_input_line 宽字符光标 | **未修复**（input.rs:441-446 按 UTF-16 单元计数）。且 TODO 的 wcwidth 方案方向有误：`\x1b[D` 在 zle/readline 中按**字符**移动，应改为按 char 计数而非列宽（见 M2） |
| #5 debug HTTP 接口加固 | **未修复**：terminal.rs:429 仍无条件 `start_debug_http_server`，默认绑定 127.0.0.1 端口段，`POST /debug/input`（debug_server.rs:348-373）可注入任意字节且不同步 input_line 模型 |
| #9 Cmd+C 复制 | **部分实现**：Shift+拖选在释放时自动写剪贴板（input.rs:712-731），仍无 Cmd+C 快捷键 |
| #10 滚动回看历史 | **未实现**：`on_scroll_wheel`（input.rs:547-577）只处理 mouse-mode 转发与 alt-screen 箭头模拟，无 scrollback |
| #11 input_line 与 shell 同步 | **未修复**，且新增粘贴/alt-screen 失同步入口；已用 AX 屏幕匹配防护缓解误重写（详见 M8） |

## 修复优先级建议

1. H1（Ctrl+标点误编码，一行掩码条件修复，收益最大）
2. H2 + M4 + L4（IME marked text 生命周期统一：提交而非丢弃）
3. M1（SGR release 保留按钮号）与 M6（guard 滞留清理，补 focus-out/新按下重置）
4. M2（tail 按 char 计数，同时更新 TODO #4 的方案描述）
5. M5/M7（修饰键编码边角）
