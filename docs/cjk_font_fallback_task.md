# CJK 字体 fallback 任务日志

## 目标

让 macOS 上的中文字符渲染为系统正体（PingFang SC / Heiti / Source Han 等），不要使用手写风（Hannotate / Hanzipen / Wawati / Yuppy 等）。参考 kitty 的实现思路：信任 CoreText 的 locale-aware 系统级联，应用层不维护 CJK 字体名清单。

## 关键依赖与位置

| 文件 | 作用 |
|---|---|
| `src/font_fallback.rs` | 应用层：自动发现"专用符号字体"（Nerd Font/Emoji/Symbol）作为 fallback |
| `src/terminal.rs:1261-1268` | `parse_font_fallbacks` 把列表传给 GPUI 的 `FontFallbacks::from_fonts` |
| `~/.cargo/git/checkouts/zed-a70e2ad075855582/19c8363/crates/gpui_macos/src/open_type.rs` | GPUI 构造 CoreText cascade 列表（已 patch） |
| `~/.cargo/git/checkouts/zed-a70e2ad075855582/19c8363/crates/gpui_macos/src/text_system.rs:462-` | `layout_line`：CoreText shape，glyph runs 中能看到实际选中的字体 |

GPUI 依赖（Cargo.toml）锁在 `rev = "19c8363a8e0d8a2f7a7181bab2c14d87390c0f25"`，patch 都打在这个 rev 的 cargo checkout 里。

## 已完成

### 1. 修复 GPUI bug — `append_system_fallbacks` 系统级联从未生效（cargo cache 内的临时 patch）

`open_type.rs:128-131` 原来用 `.iter().filter(...).map(...)`，迭代器从未被消费，闭包从未执行。已改成 `for` 循环，并去掉 `font_path().is_some()` 过滤（macOS 系统字体如 PingFang 没有传统文件路径，会被该 filter 全部排除）。

```rust
for desc in default_fallbacks.iter() {
    CFArrayAppendValue(fallback_array, desc.as_concrete_TypeRef() as _);
}
```

> ⚠️ 此修改在 `~/.cargo/git/checkouts/...` 内，`cargo clean -p gpui_macos` 或更新 git dep 后会被覆盖。
>
> **补丁已沉淀进版本控制**（2026-07-26）：`patches/gpui_macos-cjk-fallback.patch`，用
> `scripts/apply-vendor-patches.sh` 幂等应用，`--check` 只校验。`scripts/check.sh` 与 CI
> 都会跑 `--check`，所以补丁丢失现在会**显式失败**而不是静默退化。
>
> 仍待做（需要一个 GitHub fork，属方向决策）：把补丁推到自己的 zed fork 并在 Cargo.toml 用
> `[patch."https://github.com/zed-industries/zed.git"]` 指过去，那样连脚本都不需要。
> 只 vendor `gpui_macos` 单个 crate 不可行——它对 zed workspace 内其他 crate 有大量 path 依赖。

### 2. 已向上游提 issue

https://github.com/zed-industries/zed/issues/57916

### 3. 简化并修正 `src/font_fallback.rs`

恢复"只发现专用符号字体"的职责，但增加了 **CJK 覆盖探针作为排除条件**：如果字体覆盖 CJK / Hiragana / Katakana / Hangul 文本字符，则不进入自动符号 fallback 列表，让这些字符交给 CoreText 系统级联处理。

本机诊断曾确认旧列表中混入：

- `.LastResort`
- `Hannotate TC`
- `HanziPen TC`
- `LiHei Pro`
- `LiSong Pro`

修复后重新打印自动 fallback 列表，上述 CJK 文本字体均已消失，列表只保留 Nerd Font / Emoji / Symbols / PUA 类候选。

新增测试：

- `terminal_symbol_coverage_score_rejects_cjk_text_fonts`
- `terminal_symbol_coverage_score_keeps_symbol_only_fonts`

## 当前状态

已验证假设成立：`src/font_fallback.rs` 自动发现的"符号字体"列表里混入了带 CJK 覆盖的字体。这些字体作为用户 fallback 排在 CoreText 系统级联之前，会抢占中文字符。

修复已完成并清理临时诊断日志。验证命令：

```bash
cargo build
cargo test font_fallback
cargo test
```

结果：`cargo build` 通过，完整 `cargo test` 66 个测试通过。

## 剩余验收

代码层验证已完成；最终视觉验收仍需在 GUI 窗口里确认中文形态。输入示例：

```text
你好世界 中文测试
```

期望：渲染为系统正体黑体（PingFang SC 形态），非 `Hannotate TC` / `HanziPen TC` 等手写风。

尝试用 debug HTTP 做自动运行时验证时发现：旧实例占用了 `127.0.0.1:7878`，新实例指定 `127.0.0.1:7979` 时未开放 debug HTTP。现有 debug API 只能证明屏幕模型包含中文，不能证明 CoreText 实际选中的字体，因此不能替代人工视觉验收。

## 验收标准

`cargo run` 后窗口里输入中文（例如 "你好世界 中文测试"），渲染为正体黑体（PingFang SC 形态），非手写风。

## 给接手 agent 的注意事项

1. **GPUI patch 在 cargo cache 里**：`cargo clean -p gpui_macos` 之后必须重新打 patch —— 直接跑
   `scripts/apply-vendor-patches.sh`（幂等，已应用时是 no-op）。**不要**手改 cargo cache，
   改了就和 `patches/` 里的版本漂移了。
2. **不要再走"应用层维护 CJK 字体名清单"路线**——用户已否决（不优雅、平台耦合）。CJK 必须交给 CoreText 系统级联。
3. **`src/font_fallback.rs` 现在的职责是"只发现 CJK-less 的专用符号字体"**：Nerd Font、Emoji、Symbols。带 CJK 的不算，应剔除。
4. 应用是 GUI（GPUI），需 background 跑：`cargo run` + `run_in_background: true`，stderr 日志在 task output file。
