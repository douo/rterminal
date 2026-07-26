#!/usr/bin/env bash
# 把 patches/ 下的补丁应用到 cargo git checkout 中的依赖源码。
#
# 为什么需要这个脚本：gpui 依赖锁在 zed 的一个 git rev 上，而我们必须修一个
# gpui_macos 的 bug（CoreText 系统级联从未生效，导致中文渲染成手写体）。补丁打在
# ~/.cargo/git/checkouts/ 里，`cargo clean -p gpui_macos`、更新 git 依赖、或换一台
# 机器都会让它消失——而且是**静默**消失：编译照样通过，只是中文变成手写体。
#
# 用法：
#   scripts/apply-vendor-patches.sh          # 应用（幂等）
#   scripts/apply-vendor-patches.sh --check  # 只校验，未应用则以非 0 退出（供 CI 用）
#
# 正确的长期方案是把补丁推到自己的 zed fork 并在 Cargo.toml 用
# [patch."https://github.com/zed-industries/zed.git"] 指过去。见 docs/cjk_font_fallback_task.md。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PATCH_DIR="$REPO_ROOT/patches"

# 与 Cargo.toml 中 gpui/gpui_platform 的 rev 保持一致。
ZED_REV_SHORT="19c8363"

CHECK_ONLY=0
if [[ "${1:-}" == "--check" ]]; then
  CHECK_ONLY=1
fi

CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"

fail() {
  echo "error: $*" >&2
  exit 1
}

# 定位 zed 的 cargo git checkout。目录名含 URL 哈希，所以用 glob 而不是硬编码。
shopt -s nullglob
CANDIDATES=("$CARGO_HOME_DIR"/git/checkouts/zed-*/"$ZED_REV_SHORT")
shopt -u nullglob

if [[ ${#CANDIDATES[@]} -eq 0 ]]; then
  fail "找不到 zed 的 cargo git checkout（期望 $CARGO_HOME_DIR/git/checkouts/zed-*/$ZED_REV_SHORT）。
       先跑一次 \`cargo fetch\` 让 cargo 把依赖 checkout 出来，然后重新执行本脚本。"
fi

if [[ ${#CANDIDATES[@]} -gt 1 ]]; then
  echo "warning: 找到多个 zed checkout，全部处理：" >&2
  printf '  %s\n' "${CANDIDATES[@]}" >&2
fi

status=0

for checkout in "${CANDIDATES[@]}"; do
  echo "==> checkout: $checkout"

  for patch in "$PATCH_DIR"/*.patch; do
    name="$(basename "$patch")"

    # 已经打过了？（反向应用能成功即说明补丁内容已在源码里）
    if git -C "$checkout" apply --reverse --check "$patch" >/dev/null 2>&1; then
      echo "    [ok]      $name 已应用"
      continue
    fi

    if [[ $CHECK_ONLY -eq 1 ]]; then
      echo "    [MISSING] $name 未应用" >&2
      status=1
      continue
    fi

    if git -C "$checkout" apply --check "$patch" >/dev/null 2>&1; then
      git -C "$checkout" apply "$patch"
      echo "    [applied] $name"
    else
      echo "    [FAILED]  $name 既不能应用也不是已应用状态——上游源码可能已变，需要人工重做补丁" >&2
      status=1
    fi
  done
done

if [[ $status -ne 0 ]]; then
  if [[ $CHECK_ONLY -eq 1 ]]; then
    echo "" >&2
    echo "有补丁未应用。跑 \`scripts/apply-vendor-patches.sh\` 修复。" >&2
    echo "注意：不打这个补丁编译依然会通过，但中文会渲染成手写体。" >&2
  fi
  exit 1
fi

if [[ $CHECK_ONLY -eq 1 ]]; then
  echo "全部补丁已应用。"
else
  echo "完成。若刚刚有补丁被应用，需要重新编译：cargo build"
fi
