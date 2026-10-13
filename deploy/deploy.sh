#!/usr/bin/env bash
# llmux 可靠部署脚本 — 构建 → 同步 deploy → 部署到路由器 docker-compose → 健康检查
#
# 用法:
#   ./deploy.sh            # 交互确认后执行
#   ./deploy.sh -y         # 跳过确认直接执行
#   ./deploy.sh --dry-run  # 只显示计划，不执行
#
# 安全护栏（避免误伤）:
#   - set -euo pipefail: 任一步失败立即中止，绝不带病继续
#   - 只写三类文件: 本地 deploy/llmux、路由器 llmux、compose 构建产物
#   - 绝不触碰 data/（llmux_db.db + master.key）与 config.json
#   - 每次覆盖前备份: 本地 llmux.bak-<ts> / 路由器 llmux.bak-<ts>
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SRC_DIR="$PROJECT_DIR"
DEPLOY_DIR="$PROJECT_DIR/deploy"
ROUTER=router
REMOTE_DIR=/root/docker/llmux
BASE_PATH="${BASE_PATH:-${LLMUX_BASE_PATH:-llmux}}"
VITE_BASE_PATH="${VITE_BASE_PATH:-$BASE_PATH}"
HEALTH_PATH="${BASE_PATH:+/$BASE_PATH}/api/health"
HEALTH_PATH="${HEALTH_PATH#/}"
HEALTH_PATH="/${HEALTH_PATH#/}"
# Router compose maps 25977:25976 + BASE_PATH default llmux (nginx /llmux/ → 25977)
PROD_PORT=25977
HEALTH_URL="http://192.168.1.2:${PROD_PORT}${HEALTH_PATH}"
HEALTH_URLS=("$HEALTH_URL" "http://192.168.1.2:${PROD_PORT}/api/health" "https://openwrt.xiaokubao.space${HEALTH_PATH}")
SHADOW=0
SHADOW_PORT=25978
SHADOW_NAME=llmux_new
TS="$(date +%Y%m%d-%H%M%S)"

ASSUME_YES=0
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    -y|--yes) ASSUME_YES=1 ;;
    --dry-run) DRY_RUN=1 ;;
    --shadow) SHADOW=1 ;;
    *) echo "未知参数: $arg" >&2; exit 1 ;;
  esac
done
if [ "$SHADOW" -eq 1 ]; then
  HEALTH_URL="http://192.168.1.2:${SHADOW_PORT}${HEALTH_PATH}"
  HEALTH_URLS=("$HEALTH_URL")
fi

run() {
  # 进度横幅走 stderr：本函数有被 `x="$(run ...)"` 捕获的用法（见 7b 的日志目录
  # 校验），混进 stdout 会把 `▶ ssh ...` 变成命令的输出内容。
  echo "▶ $*" >&2
  [ "$DRY_RUN" -eq 1 ] || "$@"
}

confirm() {
  [ "$ASSUME_YES" -eq 1 ] && return 0
  read -r -p "确认执行以上步骤? [y/N] " ans
  [[ "$ans" =~ ^[Yy]$ ]] || { echo "已取消。"; exit 1; }
}

cd "$SRC_DIR"
if [ -n "$BASE_PATH" ]; then
  if [ "$SHADOW" -eq 1 ]; then sfx=" SHADOW"; else sfx=""; fi; echo "═══ llmux 部署 (tag $TS) BASE_PATH=/$BASE_PATH VITE_BASE_PATH=$VITE_BASE_PATH${sfx} ═══"
else
  echo "═══ llmux 部署 (tag $TS) ═══"
fi

# 1. UI 构建（BASE_PATH 非空则带前缀）
if [ ! -f ui/dist/index.html ] || find ui/src -type f -newer ui/dist/index.html -print -quit | grep -q .; then
  if [ -n "$VITE_BASE_PATH" ]; then
    run env VITE_BASE_PATH="$VITE_BASE_PATH" npm --prefix "$SRC_DIR/ui" run build
  else
    run npm --prefix "$SRC_DIR/ui" run build
  fi
else
  echo "ℹ UI 无改动，跳过 npm run build"
  if [ -n "$VITE_BASE_PATH" ]; then
    echo "ℹ 但 BASE_PATH=$VITE_BASE_PATH 非空，强制重建以注入前缀"
    run env VITE_BASE_PATH="$VITE_BASE_PATH" npm --prefix "$SRC_DIR/ui" run build
  fi
fi

# 2. Rust release 构建
run ~/.cargo/bin/cargo build --release
[ -s target/release/llmux ] || { echo "✗ 构建产物缺失" >&2; exit 1; }

# 3. 计划摘要 + 确认
echo
if [ "$SHADOW" -eq 1 ]; then
  echo "将要执行 (影子 $SHADOW_NAME:$SHADOW_PORT):"
  echo "  ① cp target/release/llmux → $DEPLOY_DIR/llmux.shadow-$TS (不覆盖现行 deploy/llmux)"
  echo "  ② scp → $ROUTER:$REMOTE_DIR/llmux.shadow-$TS + $REMOTE_DIR/${SHADOW_NAME}.locations"
  echo "  ③ $ROUTER: 隔离数据卷 data_shadow + docker compose -f docker-compose.shadow.yml build && up -d"
  echo "  ④ 健康检查 $HEALTH_URL (影子)"
  echo "  ⑤ 收尾: 清理悬空镜像 (docker image prune -f) + 本地/远程 >7天旧备份 (llmux.bak-*/llmux.shadow-*)"
else
  echo "将要执行:"
  echo "  ① cp target/release/llmux → $DEPLOY_DIR/llmux   (旧版备份 llmux.bak-$TS)"
  echo "  ② scp → $ROUTER:$REMOTE_DIR/llmux               (远程旧版备份 llmux.bak-$TS)"
  echo "  ③ $ROUTER: docker compose build && up -d (镜像 tag 统一 llmux:latest)"
  echo "  ④ 健康检查 $HEALTH_URL"
  echo "  ⑤ 收尾: 清理悬空镜像 (docker image prune -f) + 本地/远程 >7天旧备份 (llmux.bak-*/llmux.shadow-*)"
fi
confirm

if [ "$DRY_RUN" -eq 1 ]; then
  echo "ℹ [dry-run] 以上为计划，未执行任何操作。"
  exit 0
fi

# 4/5. 影子与现行分流
if [ "$SHADOW" -eq 1 ]; then
  run cp target/release/llmux "$DEPLOY_DIR/llmux.shadow-$TS"
  run scp "$DEPLOY_DIR/llmux.shadow-$TS" "$ROUTER:$REMOTE_DIR/llmux.shadow-$TS"
  run ssh "$ROUTER" "cp $REMOTE_DIR/llmux.shadow-$TS $REMOTE_DIR/llmux_new && chmod +x $REMOTE_DIR/llmux_new && ls -l $REMOTE_DIR/llmux_new"
  # 同步影子 compose 与 nginx 影子 location（如已在本地 deploy/ 准备）
  if [ -f "$DEPLOY_DIR/docker-compose.shadow.yml" ]; then
    run scp "$DEPLOY_DIR/docker-compose.shadow.yml" "$ROUTER:$REMOTE_DIR/docker-compose.shadow.yml"
  fi
  if [ -f "$DEPLOY_DIR/llmux.shadow.locations" ]; then
    run scp "$DEPLOY_DIR/llmux.shadow.locations" "$ROUTER:/etc/nginx/conf.d/llmux.shadow.locations"
    run ssh "$ROUTER" "nginx -t && nginx -s reload && echo '✓ nginx shadow reloaded'"
  fi
  run ssh "$ROUTER" "mkdir -p $REMOTE_DIR/data_shadow && ( [ -f $REMOTE_DIR/data/llmux_db.db ] && cp -n $REMOTE_DIR/data/llmux_db.db $REMOTE_DIR/data_shadow/ 2>/dev/null || true )"
  run ssh "$ROUTER" "cd $REMOTE_DIR && docker compose -f docker-compose.shadow.yml build"
else
  [ -f "$DEPLOY_DIR/llmux" ] && run cp "$DEPLOY_DIR/llmux" "$DEPLOY_DIR/llmux.bak-$TS"
  run cp target/release/llmux "$DEPLOY_DIR/llmux"
  run ssh "$ROUTER" "cp $REMOTE_DIR/llmux $REMOTE_DIR/llmux.bak-$TS && ls -l $REMOTE_DIR/llmux.bak-$TS"
  run scp "$DEPLOY_DIR/llmux" "$ROUTER:$REMOTE_DIR/llmux"
  run ssh "$ROUTER" "cd $REMOTE_DIR && docker compose build"
fi
echo "▶ compose up (首次冲突则接管孤儿容器后重试)"
if [ "$SHADOW" -eq 1 ]; then
  run ssh "$ROUTER" "cd $REMOTE_DIR && BASE_PATH=$BASE_PATH docker compose -f docker-compose.shadow.yml up -d"
  echo "▶ 影子容器 llmux_new 已启动 (端口 $SHADOW_PORT, BASE_PATH=/$BASE_PATH)"
else
  if ! ssh "$ROUTER" "cd $REMOTE_DIR && docker compose up -d" 2>/dev/null; then
  has_label="$(ssh "$ROUTER" "docker inspect llmux --format '{{index .Config.Labels \"com.docker.compose.service\"}}' 2>/dev/null || echo none")"
  if [ "$has_label" = "none" ] || [ -z "$has_label" ]; then
    echo "⚠ 容器 llmux 是孤儿(无 compose label) — 数据在卷 $REMOTE_DIR/data 中,安全删除后交 compose 接管"
    run ssh "$ROUTER" "docker rm -f llmux && cd $REMOTE_DIR && docker compose up -d"
  else
    echo "✗ compose up 失败且容器非孤儿,请手动排查" >&2; exit 1
  fi
fi
fi

# 7. 健康检查 — 依次尝试 HEALTH_URLS，任一成功即通过
echo "▶ 等待容器就绪..."
sleep 3
run ssh "$ROUTER" "docker ps --filter name=^llmux\$ --format '{{.Names}} {{.Status}}'"
health_ok=0
for url in "${HEALTH_URLS[@]}"; do
  if curl -fsS --max-time 10 "$url" >/dev/null 2>&1; then
    echo "✓ $url OK — 部署成功"
    health_ok=1
    break
  else
    echo "  ✗ $url 不通，尝试下一个..."
  fi
done
if [ "$health_ok" -eq 1 ]; then
  :
else
  echo "✗ 健康检查失败（已尝试 ${#HEALTH_URLS[@]} 个 URL）!" >&2
  echo "  尝试: ${HEALTH_URLS[*]}" >&2
  echo "  诊断: ssh $ROUTER 'docker ps; docker logs --tail 50 llmux; curl -v http://127.0.0.1:${PROD_PORT}${HEALTH_PATH}'" >&2
  echo "  回滚: ssh $ROUTER 'cd $REMOTE_DIR && cp llmux.bak-$TS llmux && docker compose build && docker compose up -d'" >&2
  exit 1
fi

# 7b. 日志目录校验 — LOG_DIR 指向 NAS(CIFS bind mount)。挂载掉线时 docker 会把
#     源目录当普通空目录建出来，容器照跑但日志全丢本地，必须当场发现。
echo "▶ 校验日志目录..."
# 容器内路径（从环境变量取）与宿主机 bind 源路径（从 Mounts 取）是两回事，
# 拿容器路径去宿主机上 ls 永远失败 —— 两边都要取。
log_dir="$(ssh "$ROUTER" "docker inspect llmux --format '{{range .Config.Env}}{{println .}}{{end}}'" 2>/dev/null | sed -n 's/^LOG_DIR=//p' || true)"
if [ -n "$log_dir" ]; then
  if probe="$(ssh "$ROUTER" "docker exec llmux sh -c 'echo x > $log_dir/.deploy-probe && cat $log_dir/.deploy-probe && rm -f $log_dir/.deploy-probe'" 2>/dev/null)" && [ "$probe" = "x" ]; then
    echo "✓ $log_dir 容器内可写"
    # 确认它真落在 NAS 上，而不是 CIFS 掉线后被 docker 新建出来的空本地目录。
    # 光看「可写」不够：掉线时 bind mount 源会变成一个全新空目录，照样可写。
    log_src="$(ssh "$ROUTER" "docker inspect llmux --format '{{range .Mounts}}{{if eq .Destination \"$log_dir\"}}{{.Source}}{{end}}{{end}}'" 2>/dev/null || true)"
    echo "  bind 源：$log_src"
    # 在宿主机侧（NAS 源路径）确认日志真的落在 NAS 上
    if [ -n "$(ssh "$ROUTER" "ls -1 $log_src/llmux.log.* 2>/dev/null | head -1" 2>/dev/null || true)" ]; then
      echo "  日志已在写：$(ssh "$ROUTER" "ls -1t $log_src/llmux.log.* 2>/dev/null | head -1" 2>/dev/null || true)"
    else
      echo "⚠ $log_dir 可写但 NAS 源路径 $log_src 下没有 llmux.log.* — 请检查 CIFS 挂载" >&2
    fi
  else
    echo "⚠ $log_dir 容器内不可写 — 日志会写不进去，请检查 NAS 挂载（ssh $ROUTER 'mountpoint -q /mnt/openwrt'）" >&2
  fi
else
  echo "  (未设 LOG_DIR，日志随 DATA_DIR)"
fi

# 8. 收尾 — 清理悬空镜像 + 旧备份（每次 compose build 产生 <none> 镜像；本地/远程保留 llmux.bak-* 最近 7 天，影子产物同；额外限量仅留最新 3 份治本）
echo "▶ 清理悬空镜像 + >7天旧备份..."
run ssh "$ROUTER" "docker image prune -f | tail -n 5"
run ssh "$ROUTER" "docker images llmux --format '{{.Repository}}:{{.Tag}} {{.ID}} {{.Size}}'"
echo "  ℹ 清理本地 >7天旧备份: $DEPLOY_DIR"
[ "$DRY_RUN" -eq 1 ] || find "$DEPLOY_DIR" -maxdepth 1 \( -name 'llmux.bak-*' -o -name 'llmux.shadow-*' \) -type f -mtime +7 -delete -print
echo "  ℹ 清理远程 >7天旧备份: $ROUTER:$REMOTE_DIR"
run ssh "$ROUTER" "find $REMOTE_DIR -maxdepth 1 \( -name 'llmux.bak-*' -o -name 'llmux.shadow-*' \) -type f -mtime +7 -exec rm -v {} \; 2>&1 | head -n 20; true"
echo "  ℹ 限量备份仅留最新 3 份（本地+远程）"
if [ "$DRY_RUN" -eq 0 ]; then ls -t "$DEPLOY_DIR"/llmux.bak-* 2>/dev/null | tail -n +4 | xargs -r rm -v 2>&1 | head -n 20 || true; fi
run ssh "$ROUTER" "ls -t $REMOTE_DIR/llmux.bak-* 2>/dev/null | tail -n +4 | xargs -r rm -v 2>&1 | head -n 20 || true"
