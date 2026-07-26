# vendored alacritty_terminal

本目录是 `alacritty_terminal` 的 fork，不是原封不动的副本。

| | |
|---|---|
| 上游 | https://github.com/alacritty/alacritty |
| 版本 | `0.25.1`（见 `Cargo.toml`） |
| 引入提交 | `a7a071d`（2026-03-14，整棵树引入 + 依赖切成 path） |
| fork 改动 | `9084d96`（2026-06-15，SIXEL 支持） |

## 相对上游改了什么

只有一处功能性 fork，4 个文件：

| 文件 | 改动 |
|---|---|
| `src/sixel.rs` | **新增**（约 470 行）。上游 0.25.1 无 SIXEL 支持，这个解码器是本项目写的。2026-07-26 补 `mark_column`（空列计入宽度，DSP-11）与对应测试 |
| `src/lib.rs` | +1 行 `pub mod sixel;` |
| `src/event.rs` | +24 行，新增 SIXEL 相关事件（含 `Event::Erase`） |
| `src/term/mod.rs` | +39/-3，接入 SIXEL 处理与颜色修正 |

除此之外应与上游 `0.25.1` 逐字节一致。**任何超出上面清单的 diff 都是意外，应该还原。**

## 两条硬规则

**1. 不要在这棵树上跑裸 `cargo fmt`。**

本 crate 是 workspace 成员（这样它的 181 个测试才会被 `cargo test --workspace` 跑到），
所以裸 `cargo fmt` 会把整棵上游源码按本项目风格重排 —— 一次就产生约 1500 行无关 diff，
彻底毁掉"与上游对比"的能力。rustfmt 的 `ignore` 选项是 nightly-only，指望不上。

用 `scripts/check.sh`，或显式写 `cargo fmt -p agent_terminal`。

**2. 我们自己新增的 `src/sixel.rs` 要接受 lint。**

本 crate 的 `lib.rs` 顶部有 `#![deny(clippy::all, ...)]`。升级 clippy 后新增的 lint 如果
命中**上游**代码，倾向于加局部 `#[allow]` 并注明是上游代码；如果命中 `src/sixel.rs`
（那是我们自己的），就正常修掉。

## 如何 rebase 到新上游

1. 取上游对应 tag 的干净副本：`git clone --depth 1 -b v<新版本> …`
2. 从本目录抽出 fork 改动：`git log -p --follow -- src/sixel.rs` 以及 `9084d96` 对
   `lib.rs` / `event.rs` / `term/mod.rs` 的三处接缝 diff。
3. 把 `src/sixel.rs` 整文件搬过去，再手工重做三处接缝（它们很短，但位置依赖上游代码结构）。
4. `Cargo.toml` 里把依赖改回 path 形式（上游用的是 crates.io 版本号）。
5. 跑 `cargo test --workspace`。注意：**上游 181 个测试完全不覆盖 `src/sixel.rs`**，
   SIXEL 的回归保护只在主 crate 的 `src/sixel/` 里（parser 2 个 + images 5 个），
   接缝部分目前没有测试兜底——rebase 后需要人工验证内联图片显示。

## 已知待办

- 三处接缝（`lib.rs` / `event.rs` / `term/mod.rs`）零上游侧测试（上面第 5 点）；
  `src/sixel.rs` 本身已有 4 个解码测试。
- DSP-10（`EL` 清行对整行发 Erase、图片"相交即整删"）仍在这棵树里，属行为取舍，
  未排期（见 `docs/project-review/07-progress.md` 阶段 3 遗留）。
  DSP-1/DSP-2/DSP-11 已在 2026-07-26 修复（DSP-1/2 在主 crate 侧，DSP-11 在本树）。
