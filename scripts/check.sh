#!/usr/bin/env bash
# 项目质量门槛。CI 与本地用同一个入口，避免两边漂移。
#
#   scripts/check.sh          # 全量检查
#   scripts/check.sh --fix    # 顺手把能自动修的（格式化）改掉
#
# 注意 fmt 是**包级**的（-p agent_terminal）：vendored alacritty_terminal 是
# workspace 成员，裸跑 `cargo fmt` 会把整棵上游源码按本项目风格重排，
# 那会毁掉与上游 diff 的能力（见 vendor/alacritty_terminal/VENDOR.md）。
# rustfmt 的 `ignore` 选项是 nightly-only，所以只能靠作用域约束。

set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

FIX=0
if [[ "${1:-}" == "--fix" ]]; then
  FIX=1
fi

step() { printf '\n==> %s\n' "$1"; }

step "vendored 依赖补丁校验"
# 不打这个补丁编译照样通过，只是中文静默渲染成手写体——所以必须显式校验。
scripts/apply-vendor-patches.sh --check

step "格式化 (仅 agent_terminal)"
if [[ $FIX -eq 1 ]]; then
  cargo fmt -p agent_terminal
else
  cargo fmt -p agent_terminal -- --check
fi

step "clippy (全 workspace, warnings 视为错误)"
cargo clippy --all-targets -- -D warnings

step "测试 (全 workspace，含 vendored 的 181 个)"
cargo test --workspace

step "self-check"
cargo run --quiet -- --self-check

printf '\n全部通过。\n'
