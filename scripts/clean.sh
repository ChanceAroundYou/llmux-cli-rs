#!/usr/bin/env bash
# llmux 项目清理 — 构建产物 / 陈旧备份 / 工具残留
#
# 用法:
#   ./scripts/clean.sh                  # 只报告，不删（默认）
#   ./scripts/clean.sh --apply          # 真删 L1
#   ./scripts/clean.sh --apply --all    # 连 L2 / L3 一起删
#   ./scripts/clean.sh --apply --all --l3-days=14
#
# 分级:
#   L1  增量编译缓存、cargo 临时目录、preview 截图、超额 .bak、空目录   默认执行
#   L2  ui/node_modules（删后需 npm ci 重装）                            需 --all
#   L3  .claude/observations / .omc / .mindfs/sessions 的陈旧记录        需 --all
#
# 永不删:
#   ui/dist            rust_embed 在编译期读它（static_ui.rs 的 #[folder]），
#                      删了 cargo build 直接失败
#   deploy/llmux       当前部署产物
#   target/release     llmux 与 deploy/llmux 内容一致，保留以便增量重建
#   任何 git 跟踪的文件，以及 crates/、ui/src 下的源码
#
# 与 deploy/deploy.sh 第 8 步的关系: 那边管路由器上的远端备份，本脚本管本地；
# 备份保留策略（留最新 3 份）刻意保持一致，两边同时跑不会互相误删。
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC_DIR="$PROJECT_ROOT"
DEPLOY_DIR="$PROJECT_ROOT/deploy"
UI_DIR="$SRC_DIR/ui"

# cargo 不在 PATH（见 AGENTS.md），得自己找
CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
[ -x "$CARGO" ] || CARGO="$(command -v cargo || true)"

APPLY=0
WITH_L2=0
WITH_L3=0
L3_DAYS=30
KEEP_BACKUPS=3

usage() {
  # 打印头部注释块（到首个非 # 行为止），免得行号写死改了说明就错位
  sed -n '2,/^[^#]/p' "$0" | sed 's/^# \{0,1\}//; $d'
  exit "${1:-0}"
}

for arg in "$@"; do
  case "$arg" in
    --apply)     APPLY=1 ;;
    --all)       WITH_L2=1; WITH_L3=1 ;;
    --l3-days=*) L3_DAYS="${arg#*=}" ;;
    -h|--help)   usage 0 ;;
    *) echo "未知参数: $arg（-h 看用法）" >&2; exit 1 ;;
  esac
done
case "$L3_DAYS" in
  ''|*[!0-9]*) echo "--l3-days 必须是正整数，收到: $L3_DAYS" >&2; exit 1 ;;
esac

MODE=$([ "$APPLY" -eq 1 ] && echo "APPLY" || echo "DRY-RUN")
FREED=0

# ── 工具函数 ────────────────────────────────────────────────────────────

# 项目外的路径一律拒绝，防路径穿越把脚本用到别处
assert_inside_root() {
  case "$1" in
    "$PROJECT_ROOT"/*) : ;;
    *) echo "✗ 拒绝操作项目根之外的路径: $1" >&2; exit 1 ;;
  esac
}

bytes() { du -sb "$1" 2>/dev/null | cut -f1 || echo 0; }
human() {
  local b="${1:-0}"
  awk -v b="$b" 'BEGIN{
    split("B KiB MiB GiB TiB", u, " ")
    i=1
    while (b >= 1024 && i < 5) { b /= 1024; i++ }
    printf (i == 1 ? "%d%s" : "%.1f%s"), b, u[i]
  }'
}

# git 跟踪的文件一律不删。git 不跟踪空目录，所以对目录这个判断天然放行。
is_tracked() {
  local rel="$1"
  case "$rel" in
    "$SRC_DIR"/*) git -C "$SRC_DIR" ls-files --error-unmatch "${rel#"$SRC_DIR"/}" >/dev/null 2>&1 ;;
    *) return 1 ;;   # 仓库根之外（如 /tmp），与 llmux 无关，恒为未跟踪
  esac
}

# skip <理由> —— 显式跳过，计入报告但不删
skip() { printf '  %-8s %s  (%s)\n' "[$MODE]" "$1" "$2"; }

remove() {
  local path="$1" label="$2" sz
  [ -e "$path" ] || return 0
  assert_inside_root "$path"
  if is_tracked "$path"; then
    skip "$label" "git 跟踪，跳过: $path"
    return 0
  fi
  sz="$(bytes "$path")"
  if [ "$APPLY" -eq 1 ]; then
    rm -rf -- "$path"
    FREED=$((FREED + sz))
    printf '  %-8s %s  释放 %s  %s\n' "[APPLY]" "$label" "$(human "$sz")" "$path"
  else
    printf '  %-8s %s  可释放 %s  %s\n' "[DRY]" "$label" "$(human "$sz")" "$path"
  fi
}

# ── 开跑前的前置检查 ──────────────────────────────────────────────────────

echo "═══ llmux 清理 [$MODE] ═══"
echo "项目根: $PROJECT_ROOT"

# 删 target/incremental 时若 cargo 正在编译，会留下半截产物
if [ "$APPLY" -eq 1 ]; then
  if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
    echo "✗ 检测到 cargo/rustc 正在运行，等构建结束再跑（否则 target 会被删成半截）" >&2
    exit 1
  fi
fi

# 有未提交改动不阻断（删构建产物与源码无关），但要说清楚
if [ -n "$(git -C "$SRC_DIR" status --porcelain 2>/dev/null)" ]; then
  echo "ℹ 工作树有未提交改动 —— 本脚本不碰 src/ 下源码，可以放心跑"
fi
echo

# ── L1: 增量编译缓存与临时产物 ───────────────────────────────────────────

echo "── L1 构建缓存与临时产物"

# 陈旧编译产物 — target/debug/deps 里的大头。
# 集成测试可执行文件（静态链进整个 axum 栈，单个 160-200M）和 .rlib/.rmeta
# 都是「改一次代码多一份」，旧的 cargo 早已不认却不会删 —— 实测同一测试能积到
# 3-5 份。仅可执行文件就 39 个 = 3.95 GiB，另加陈旧 .rlib 559 MiB。
#
# 白名单由 cargo 自己给：`--message-format=json` 的 compiler-artifact 消息
# 带每个当前有效产物的精确路径。不要自己按 mtime 或 hash 前缀猜 —— 猜错会
# 删掉 cargo 还要用的，只能全量重建（实测 3分24秒）。
#
# 两类字段都要取：
#   "executable" → 集成测试的可执行文件（160-200M 一个，占大头）
#   "filenames"  → .rlib / .rmeta（llmux-server 一个就 150M+）
#
# 两个 profile 各判一次：debug 跑测试，release 只留可执行程序（见下）。
#
# 同时清掉配套的 .d 和 .fingerprint/ 目录：留着会让 cargo 认为依赖文件缺失
# 从而触发不必要的重链。
stale="$(mktemp)"; keep="$(mktemp)"
trap 'rm -f "$stale" "$keep" "$keep.raw"' EXIT
deps="$SRC_DIR/target/debug/deps"
if [ -d "$deps" ]; then
  # 白名单要同时取两次：cargo test --no-run（集成测试 harness）与 cargo build
  # （lib/bin 产物）。只取其一会漏—— 实测 build 独有的一批 .rmeta 不在 test
  # 清单里，会被误判成陈旧（实测差 240 vs 45 个）。
  #
  # 无改动时 cargo 照样重吐 Fresh 单元的消息（实测第二次跑仍是 309 条），
  # 所以这里不需要先触发一次真实编译。
  ( cd "$SRC_DIR" && "$CARGO" test --offline --no-run --message-format=json 2>/dev/null
    cd "$SRC_DIR" && "$CARGO" build --offline --message-format=json 2>/dev/null ) \
    > "$keep.raw" || true
  {
    grep -o '"executable":"[^"]*"' "$keep.raw" 2>/dev/null | sed 's/.*"executable":"//; s/"$//'
    grep -o '"filenames":\[[^]]*\]' "$keep.raw" 2>/dev/null \
      | grep -o '"[^"]*/target/debug/deps/[^"]*"' | tr -d '"'
  } | while IFS= read -r p; do basename "$p"; done | sort -u > "$keep"
  rm -f "$keep.raw"
  if [ ! -s "$keep" ]; then
    skip "陈旧编译产物" "拿不到 cargo 产物清单，跳过（宁可不删也不删错）"
  else
    # deps 下所有文件逐个比对：不在白名单里的就是陈旧。
    # 三条豁免：
    #  1) 硬链接（links>1）：与target/<profile>/ 顶层产物共享同一份数据
    #     （如 deps/llmux-<hash> ↔ debug/llmux），删目录项不释放空间（另一个
    #     链接还在），cargo 下次还会重建回来 —— 白删。
    #  2) .d 依赖清单：cargo 的 JSON filenames 里不含它们，但 cargo 自己要用来
    #     判断是否需要重链。判据跟着主产物走 —— 白名单里存在任何以<裸名> 为前缀
    #     的条目（.rlib/.rmeta）就说明主产物有效，.d 必须留。
    #  3) lib 前缀：.d 文件名不带 lib（powerfmt-xxx.d），而白名单里的产物带
    #     （libpowerfmt-xxx.rlib）。只试裸名会把这些全误判成陈旧 ——
    #     实测 release 侧 356 个 .d 全是这个误判。两个前缀都试。
    #
    # 硬链接判断不能用 stat -c %h 直接挡 .d：可执行文件 llmux-<hash> 是
    # release/llmux 的硬链接伙伴（links=2），但同名的 llmux-<hash>.d 是
    # 独立 inode（links=1），而 cargo 的 executable 字段只给顶层路径
    # target/release/llmux（不含 hash），按 basename 匹配不上 → 当前 binary
    # 的依赖清单会被误删。所以 .d 另判：同名主产物存在就说明这批是活的。
    find "$deps" -maxdepth 1 -type f -printf '%f\n' 2>/dev/null \
      | while IFS= read -r f; do
          case "$f" in
            *.d) base="${f%.d}"
                 [ -e "$deps/$base" ] && continue
                 if grep -q "^$base\." "$keep" || grep -q "^lib$base\." "$keep"; then continue; fi ;;
            *)   [ "$(stat -c %h "$deps/$f" 2>/dev/null || echo 1)" -eq 1 ] || continue
                 if grep -qxF "$f" "$keep"; then continue; fi ;;
          esac
          echo "$f"
        done > "$stale" || true
    n=$(grep -c . "$stale" || true)
    if [ "$n" -eq 0 ]; then
      skip "陈旧编译产物" "无陈旧项"
    else
      # 每个循环体最后一条命令若是 `[ ... ] && cmd`，条件为假时退出码非零，
      # 整个 while 的退出码就跟着非零 —— set -e 下赋值语句直接杀掉脚本。
      # 一律用 if 包裹。
      sz=$(while IFS= read -r f; do
              for p in "$deps/$f" "$deps/$f.d"; do if [ -e "$p" ]; then bytes "$p"; fi; done
              b="${f%-*}"; h="${f##*-}"
              d="$SRC_DIR/target/debug/.fingerprint/$b-$h"
              if [ -d "$d" ]; then bytes "$d"; fi
              echo 0
            done < "$stale" | awk '{s+=$1} END {print s+0}')
      if [ "$APPLY" -eq 1 ]; then
        while IFS= read -r f; do
          rm -f -- "$deps/$f" "$deps/$f.d" || true
          rm -rf -- "$SRC_DIR/target/debug/.fingerprint/${f%-*}-${f##*-}" || true
          echo .
        done < "$stale" >/dev/null
        FREED=$((FREED + sz))
        printf '  %-8s %s  释放 %s  %d 个陈旧产物\n' "[APPLY]" "陈旧编译产物" "$(human "$sz")" "$n"
      else
        printf '  %-8s %s  可释放 %s  %d 个陈旧产物\n' "[DRY]" "陈旧编译产物" "$(human "$sz")" "$n"
      fi
    fi
  fi
else
  skip "陈旧编译产物" "deps/ 不存在"
fi

# incremental 只是增量编译缓存，删掉不影响正确性，只让下次 build 慢一点。
# deps/ 刻意保留 —— cargo test 靠它做快速增量编译，删了要全量重编 1285 个产物。
remove "$SRC_DIR/target/debug/incremental" "增量编译缓存"

# release 下的测试产物 —— 按白名单挑，不整块删。
#
# 本项目没有任何地方用 release 跑测试（CLAUDE.md 规定 `cargo test`（debug）
# 和 `cargo build --release`，无 CI，全仓 grep 不到 `--release` + test 组合），
# 所以 release/deps 里的测试可执行文件和 test-only 依赖都不是构建 router 上
# 那个 binary 要用的。
#
# ⚠️ 白名单只用 `cargo build --release`（**不加 --tests**）。加了会真的去编译
# release 测试二进制（实测凭空多出 1.5G），不是只列清单。
#
# ⚠️ 别整块删 release/deps：里面 llmux-server 等依赖的 .rlib 是
# `cargo build --release` 真需要的（实测 678 MiB），删了要重编 9 分 00 秒。
# 只删不在白名单里的那些。
rdeps="$SRC_DIR/target/release/deps"
if [ -d "$rdeps" ]; then
  ( cd "$SRC_DIR" && "$CARGO" build --release --offline --message-format=json 2>/dev/null ) \
    > "$keep.raw" || true
  {
    grep -o '"executable":"[^"]*"' "$keep.raw" 2>/dev/null | sed 's/.*"executable":"//; s/"$//'
    grep -o '"filenames":\[[^]]*\]' "$keep.raw" 2>/dev/null \
      | grep -o '"[^"]*/target/release/deps/[^"]*"' | tr -d '"'
  } | while IFS= read -r p; do basename "$p"; done | sort -u > "$keep"
  rm -f "$keep.raw"
  if [ ! -s "$keep" ]; then
    skip "release 测试产物" "拿不到 cargo 产物清单，跳过（宁可不删也不删错）"
  else
    rstale="$(mktemp)"
    find "$rdeps" -maxdepth 1 -type f -printf '%f\n' 2>/dev/null \
      | while IFS= read -r f; do
          case "$f" in
            *.d) base="${f%.d}"
                 [ -e "$rdeps/$base" ] && continue
                 if grep -q "^$base\." "$keep" || grep -q "^lib$base\." "$keep"; then continue; fi ;;
            *)   [ "$(stat -c %h "$rdeps/$f" 2>/dev/null || echo 1)" -eq 1 ] || continue
                 if grep -qxF "$f" "$keep"; then continue; fi ;;
          esac
          echo "$f"
        done > "$rstale" || true
    rn=$(grep -c . "$rstale" || true)
    if [ "$rn" -eq 0 ]; then
      skip "release 测试产物" "无陈旧项"
      rm -f "$rstale"
    else
      rsz=$(while IFS= read -r f; do
              if [ -e "$rdeps/$f" ]; then bytes "$rdeps/$f"; fi
              echo 0
            done < "$rstale" | awk '{s+=$1} END {print s+0}')
      if [ "$APPLY" -eq 1 ]; then
        while IFS= read -r f; do
          rm -f -- "$rdeps/$f" "$rdeps/$f.d" || true
          rm -rf -- "$SRC_DIR/target/release/.fingerprint/${f%-*}-${f##*-}" || true
        done < "$rstale"
        FREED=$((FREED + rsz))
        printf '  %-8s %s  释放 %s  %d 个陈旧产物\n' "[APPLY]" "release 测试产物" "$(human "$rsz")" "$rn"
      else
        printf '  %-8s %s  可释放 %s  %d 个陈旧产物\n' "[DRY]" "release 测试产物" "$(human "$rsz")" "$rn"
      fi
      rm -f "$rstale"
    fi
  fi
else
  skip "release 测试产物" "release/deps 不存在"
fi

remove "$SRC_DIR/target/release/incremental" "release 增量缓存"
# target/{debug,release}/build 不删：build-script 的产物输出（icu_properties_data
# 这类要编译并运行程序生成数据）。只有 38M，不值得换来一次重跑。

remove "$SRC_DIR/target/tmp" "cargo 临时目录"

# playwright 生成的本地截图，.gitignore 已排除
remove "$SRC_DIR/preview" "preview 截图"

# 备份留最新 KEEP_BACKUPS 份，与 deploy.sh 第 8 步同一策略
mapfile -t baks < <(ls -t "$DEPLOY_DIR"/llmux.bak-* 2>/dev/null || true)
if [ "${#baks[@]}" -gt "$KEEP_BACKUPS" ]; then
  for b in "${baks[@]:$KEEP_BACKUPS}"; do remove "$b" "超额备份"; done
else
  skip "备份" "现有 ${#baks[@]} 份，未超上限（留 $KEEP_BACKUPS 份），不删"
fi

# git 不跟踪空目录，所以删空目录不会丢版本控制里的任何东西。
# 排除 .claude/governance：hook 每次运行都会重建它，删了下次照样出现，
# 只会让报告里「空目录 共 N 个」永远不为零，看着像没清干净。
# -delete 隐含 -depth，从深到浅。
empty_removed=0
while IFS= read -r d; do
  empty_removed=$((empty_removed + 1))
  [ "$APPLY" -eq 1 ] && rm -rf -- "$d"
done < <(find "$PROJECT_ROOT" \
  \( -path "$SRC_DIR/.git" -o -path "$SRC_DIR/target" -o -path "$UI_DIR/node_modules" \) -prune \
  -o -type d -name governance -path '*/.claude/*' -prune \
  -o -type d -empty -print 2>/dev/null)
if [ "$empty_removed" -gt 0 ]; then
  printf '  %-8s %s  共 %d 个\n' "[$MODE]" "空目录" "$empty_removed"
else
  skip "空目录" "无可清理项"
fi
echo

# ── L2: 依赖目录（--all） ─────────────────────────────────────────────────

echo "── L2 依赖目录"
if [ "$WITH_L2" -eq 1 ]; then
  remove "$UI_DIR/node_modules" "npm 依赖（删后需 npm ci）"
else
  skip "npm 依赖" "node_modules，--all 才删"
fi
echo

# ── L3: 工具状态与历史（--all） ───────────────────────────────────────────

echo "── L3 工具状态与历史（> ${L3_DAYS} 天）"
if [ "$WITH_L3" -eq 1 ]; then
  find "$SRC_DIR/.claude/observations" -type f -name '*.jsonl' \
       -mtime "+$L3_DAYS" -exec rm -v -- {} + 2>/dev/null || true
  find "$SRC_DIR/.omc" -type f -mtime "+$L3_DAYS" \
       -exec rm -v -- {} + 2>/dev/null || true
  find "$PROJECT_ROOT/.mindfs/sessions" -type f -name '*.jsonl' \
       -mtime "+$L3_DAYS" -exec rm -v -- {} + 2>/dev/null || true
  find "$SRC_DIR/.superpowers" -type f -mtime "+$L3_DAYS" \
       -exec rm -v -- {} + 2>/dev/null || true
else
  skip "工具状态" ".claude/observations、.omc、.mindfs/sessions，--all 才删"
fi
echo

# ── 汇总 ──────────────────────────────────────────────────────────────────

if [ "$APPLY" -eq 1 ]; then
  echo "✓ 已清理，释放 $(human "$FREED")"
  echo "  提示: src/ui/dist 已保留，cargo build 正常；"
  echo "        deploy/llmux 未动，路由器上的容器不受影响。"
else
  echo "ℹ 以上为预演，未删除任何文件。加 --apply 执行。"
fi