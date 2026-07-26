# 第三方许可声明

本项目本体私有不授权（见 LICENSE），但产物**静态链接**了以下第三方代码。
一旦对外分发 `.app` 或二进制，需随附本清单及对应许可证副本。

| 组件 | 来源 | 许可 | 形态 |
|---|---|---|---|
| alacritty_terminal | https://github.com/alacritty/alacritty (0.25.1) | Apache-2.0 | vendored fork（`vendor/alacritty_terminal`，改动见其 VENDOR.md），静态链接 |
| gpui / gpui_macos / gpui_platform | https://github.com/zed-industries/zed (rev 19c8363) | Apache-2.0 | git 依赖 + 一处本地补丁（`patches/gpui_macos-cjk-fallback.patch`），静态链接 |
| 其余 crates.io 依赖 | 见 `Cargo.lock` | 以各自 crate 声明为准（MIT / Apache-2.0 为主） | 静态链接 |

Apache-2.0 许可证全文见 <https://www.apache.org/licenses/LICENSE-2.0>。
分发时建议用 `cargo about` 或 `cargo license` 生成完整清单核对一遍，
本文件只保证覆盖两处**非 crates.io 常规形态**的依赖（vendored fork 与打补丁的 git 依赖）——
它们最容易在自动扫描里漏掉。
