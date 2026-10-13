#!/usr/bin/env bash
# codex 登录（外置）— device-auth + 凭据导入 llmux
#
# 为什么外置：device-auth 需要人在浏览器里输一次性码，llmux 是无头网关，
# 把交互式 OAuth 塞进 axum 等于给网关加浏览器回调 + 轮询状态机，不值。
# 登录用官方 `codex login --device-auth` 跑（独立 CODEX_HOME，不踢掉本机登录），
# 拿到 auth.json 后调 llmux 的 /api/system/codex-import 落库。
#
# 用法:
#   ./scripts/codex-login.sh --account-id 60                 # 登录+导入（默认 llmux 本地 25976）
#   ./scripts/codex-login.sh --account-id 60 --reuse          # 跳过登录，直接复用上次的 auth.json
#   ./scripts/codex-login.sh --account-id 60 --from-router    # 从路由器 sidecar 取现成凭据（免登录）
#   LLMux_BASE=http://127.0.0.1:25977 ./scripts/codex-login.sh --account-id 60 ...
#   ADMIN_USER=admin ADMIN_PASS=xxx ./scripts/codex-login.sh --account-id 60 ...
#
# 依赖: codex CLI（0.162+，带 --device-auth）、curl、python3、ssh（仅 --from-router）
# 凭据链: CODEX_HOME 隔离目录 → auth.json → llmux DB（codex_auth_json 字段）
# 副作用: 只写隔离目录和 DB，不碰 ~/.codex（本机登录不受影响）
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# 隔离会话目录：刻意不与 ~/.codex 共用 —— 同一份 refresh_token 两处刷新会互踢。
WORK_HOME="${CODEX_HOME:-$SCRIPT_DIR/../.codex-login}"
AUTH_JSON="$WORK_HOME/auth.json"
BASE="${LLMUX_BASE:-http://127.0.0.1:25976}"
ROUTER="${ROUTER:-router}"
ROUTER_AUTH="/root/docker/llmux/codex-auth/auth.json"

ACCOUNT_ID=""
REUSE=0
FROM_ROUTER=0

usage() {
  sed -n '2,/^[^#]/p' "$0" | sed 's/^# \{0,1\}//; $d'
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --account-id=*) ACCOUNT_ID="${1#*=}" ;;
    --account-id) ACCOUNT_ID="${2:-}"; shift ;;
    --reuse) REUSE=1 ;;
    --from-router) FROM_ROUTER=1 ;;
    -h|--help) usage 0 ;;
    *) echo "未知参数: $1" >&2; usage 1 ;;
  esac
  shift || true
done
if [ -z "$ACCOUNT_ID" ]; then echo "--account-id 必填" >&2; usage 1; fi

need() { command -v "$1" >/dev/null 2>&1 || { echo "缺依赖: $1" >&2; exit 1; }; }
need curl; need python3

auth_valid() {
  # 校验 auth.json：auth_mode=chatgpt 且 access_token 非空且未过期（提前 30s）
  python3 - "$1" <<'EOF' >/dev/null 2>&1
import json, base64, time, sys
d = json.load(open(sys.argv[1]))
if d.get("auth_mode") != "chatgpt":
    sys.exit(1)
t = d.get("tokens", {})
acc = t.get("access_token", "")
if not acc:
    sys.exit(1)
try:
    pay = acc.split(".")[1]
    pay += "=" * (-len(pay) % 4)
    p = json.loads(base64.urlsafe_b64decode(pay))
    exp = p.get("exp")
    if exp and int(time.time()) >= exp - 30:
        sys.exit(2)
except Exception:
    pass
EOF
}

if [ "$FROM_ROUTER" = 1 ]; then
  need ssh
  echo "→ 从路由器取 sidecar 凭据 ($ROUTER:$ROUTER_AUTH)…"
  mkdir -p "$WORK_HOME"
  scp -q "$ROUTER:$ROUTER_AUTH" "$AUTH_JSON.tmp" \
    || { echo "取回失败：确认 ssh $ROUTER 可达且文件存在" >&2; exit 1; }
  mv "$AUTH_JSON.tmp" "$AUTH_JSON"
  chmod 600 "$AUTH_JSON"
elif [ "$REUSE" = 0 ]; then
  need codex
  mkdir -p "$WORK_HOME"
  echo "→ 独立会话登录（$WORK_HOME，不影响 ~/.codex）…"
  echo "  浏览器打开提示的链接、输一次性码，完成后回车继续。"
  CODEX_HOME="$WORK_HOME" codex login --device-auth
  [ -f "$AUTH_JSON" ] || { echo "登录后没找到 $AUTH_JSON" >&2; exit 1; }
else
  [ -f "$AUTH_JSON" ] || { echo "没有可复用的 $AUTH_JSON，先不带 --reuse 跑一次" >&2; exit 1; }
  echo "→ 复用 $AUTH_JSON"
fi

if auth_valid "$AUTH_JSON"; then
  echo "→ 凭据有效"
else
  rc=$?
  if [ "$rc" = 2 ]; then echo "凭据过期但 refresh_token 还在，llmux 请求时会自刷" >&2;
  else echo "凭据无效（auth_mode/access_token），重跑登录" >&2; exit 1; fi
fi

# 登录 llmux 拿 session cookie（jar 复用，避免把密码放 URL）
JAR="$(mktemp)"
trap 'rm -f "$JAR"' EXIT
USER="${ADMIN_USER:-admin}"
if [ -z "${ADMIN_PASS:-}" ]; then
  read -rsp "llmux 管理密码 ($USER): " ADMIN_PASS; echo
fi
curl -fsS -c "$JAR" -X POST "$BASE/api/auth/login" \
  -H 'content-type: application/json' \
  -d "$(python3 -c 'import json,sys; print(json.dumps({"username": sys.argv[1], "password": sys.argv[2]}))' "$USER" "$ADMIN_PASS")" \
  >/dev/null || { echo "llmux 登录失败" >&2; exit 1; }

# 导入：auth.json 内联 POST（不经过 llmux 本机文件）
PAYLOAD="$(python3 - "$AUTH_JSON" "$ACCOUNT_ID" <<'EOF'
import json, sys
content = open(sys.argv[1]).read()
print(json.dumps({"account_id": int(sys.argv[2]), "auth_json": content}))
EOF
)"
RESP="$(printf '%s' "$PAYLOAD" | curl -fsS -b "$JAR" -X POST "$BASE/api/system/codex-import" \
  -H 'content-type: application/json' -d @-)" \
  || { echo "导入失败" >&2; exit 1; }

echo "$RESP" | python3 -c "
import json, sys
d = json.load(sys.stdin)
print('✓ 账户 {} 已切 codex 原生路径（account_hint={}，token_expired={}）'.format(
    d.get('account_id'), d.get('account_id_hint'), d.get('token_expired')))
print('  sidecar 可停：ssh router \"cd /root/docker/llmux && docker compose stop codex-api\"')
"
