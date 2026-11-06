#!/usr/bin/env bash
# 一键启动本地开发环境：后端（streamable-http）+ 前端（Vite dev）。
#
# 用法：scripts/dev.sh [--backend-port 8000] [--web-port 5173] [--pg-dsn <dsn>] [--reset-data] [--no-web]
#   --backend-port  后端 HTTP 端口（默认 8000）
#   --web-port      前端 Vite 端口（默认 5173，被占用时自动 +1 顺延）
#   --pg-dsn        可选。给了就生成 .dev-data/policy.toml（追加 dev-pg 数据源）并注入 HJZL_DEV_PG_DSN
#   --reset-data    启动前清空 .dev-data/（默认保留，便于连续试用）
#   --no-web        只起后端
#
# Ctrl+C 会同时停掉前后端（POSIX 用进程组，Windows 用 taskkill /T）；日志在 .dev-data/logs/。
# 前端代理目标用 HJZL_DEV_API_TARGET 指定（默认 http://127.0.0.1:8000），与 HJZL_DEV_ADMIN_TOKEN 一样只存在于 Node 进程环境。

set -uo pipefail
# 作业控制：后台子进程各自成组，Ctrl+C 时能按进程组整体收掉
set -m

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BACKEND_DIR="$REPO_ROOT/backend"
WEB_DIR="$REPO_ROOT/admin-web"
DATA_DIR="$REPO_ROOT/.dev-data"
LOG_DIR="$DATA_DIR/logs"
BASE_POLICY="$REPO_ROOT/config/policy.toml"

ADMIN_TOKENS="alice:tok_alice_000000000,bob:tok_bob_00000000000"
ALICE_TOKEN="tok_alice_000000000"
BOB_TOKEN="tok_bob_00000000000"

BACKEND_PORT=8000
WEB_PORT=5173
PG_DSN=""
RESET_DATA=0
NO_WEB=0

usage() {
  sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --backend-port) shift; BACKEND_PORT="${1:-}" ;;
    --backend-port=*) BACKEND_PORT="${1#*=}" ;;
    --web-port) shift; WEB_PORT="${1:-}" ;;
    --web-port=*) WEB_PORT="${1#*=}" ;;
    --pg-dsn) shift; PG_DSN="${1:-}" ;;
    --pg-dsn=*) PG_DSN="${1#*=}" ;;
    --reset-data) RESET_DATA=1 ;;
    --no-web) NO_WEB=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "错误：未知参数 '$1'" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

case "$(uname -s 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*) IS_WINDOWS=1 ;;
  *) IS_WINDOWS=0 ;;
esac

BACKEND_PID=""
WEB_PID=""
STOPPING=0

kill_tree() {
  local pid="$1"
  [ -z "$pid" ] && return 0
  if [ "$IS_WINDOWS" = "1" ]; then
    # MSYS 的 pid 不是 Windows pid，先用 ps -W 映射，再 taskkill 整棵树
    local wpid
    wpid="$(ps -W 2>/dev/null | awk -v p="$pid" '$1==p {print $4; exit}')"
    if [ -n "$wpid" ]; then
      taskkill //T //F //PID "$wpid" >/dev/null 2>&1 || true
    fi
  fi
  kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
  sleep 1
  kill -KILL -- "-$pid" 2>/dev/null || true
  kill -9 "$pid" 2>/dev/null || true
}

stop_children() {
  [ "$STOPPING" = "1" ] && return 0
  STOPPING=1
  if [ -n "$WEB_PID" ]; then
    echo "==> 停止前端（pid $WEB_PID）"
    kill_tree "$WEB_PID"
  fi
  if [ -n "$BACKEND_PID" ]; then
    echo "==> 停止后端（pid $BACKEND_PID）"
    kill_tree "$BACKEND_PID"
  fi
  sleep 1
  local left=""
  [ -n "$WEB_PID" ] && kill -0 "$WEB_PID" 2>/dev/null && left="$left web"
  [ -n "$BACKEND_PID" ] && kill -0 "$BACKEND_PID" 2>/dev/null && left="$left backend"
  if [ "$IS_WINDOWS" = "1" ]; then
    # MSYS 包装进程退出不代表 Windows 子进程退出，再按 Windows pid 核一遍
    for pid in "$WEB_PID" "$BACKEND_PID"; do
      [ -z "$pid" ] && continue
      local wpid
      wpid="$(ps -W 2>/dev/null | awk -v p="$pid" '$1==p {print $4; exit}')"
      [ -n "$wpid" ] && tasklist //FI "PID eq $wpid" //NH 2>/dev/null | grep -q "$wpid" && left="$left win:$wpid"
    done
  fi
  if [ -n "$left" ]; then
    echo "警告：以下进程仍在运行：$left"
  else
    echo "前后端进程已全部退出（无残留）"
  fi
}

trap 'stop_children; exit 130' INT
trap 'stop_children; exit 143' TERM
trap 'stop_children' EXIT

port_free() {
  node -e 'const net=require("net");const s=net.createServer();s.once("error",()=>process.exit(1));s.listen(Number(process.argv[1]),"127.0.0.1",()=>s.close(()=>process.exit(0)))' "$1" >/dev/null 2>&1
}

wait_http() {
  # wait_http <url> <seconds> <out-file>；成功返回 0
  local url="$1" seconds="$2" out="$3"
  local deadline=$(( $(date +%s) + seconds ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    local code
    code="$(curl -sS --connect-timeout 2 --max-time 3 -o "$out" -w "%{http_code}" "$url" 2>/dev/null || echo 000)"
    if [ "$code" = "200" ]; then return 0; fi
    sleep 1
  done
  return 1
}

for tool in cargo node npm curl; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "缺少必需工具：$tool" >&2
    echo "安装指引：cargo → https://rustup.rs；node/npm → https://nodejs.org；curl → apt/brew install curl" >&2
    exit 1
  fi
done

if [ "$RESET_DATA" = "1" ] && [ -d "$DATA_DIR" ]; then
  echo "==> 清空 $DATA_DIR"
  rm -rf "$DATA_DIR"
fi
mkdir -p "$LOG_DIR"

# ------------------------------------------------------------ 配置：可选 PG 数据源
POLICY_FILE="$BASE_POLICY"
BACKEND_ENV=(env)
if [ -n "$PG_DSN" ]; then
  POLICY_FILE="$DATA_DIR/policy.toml"
  cp "$BASE_POLICY" "$POLICY_FILE"
  cat >>"$POLICY_FILE" <<'EOF'

[[datasources]]
name = "dev-pg"
engine = "postgres"
readonly = true
allowed_schemas = ["public"]
statement_timeout_ms = 15000
dsn_env = "HJZL_DEV_PG_DSN"
EOF
  BACKEND_ENV+=(HJZL_DEV_PG_DSN="$PG_DSN")
  echo "==> 已生成临时数据源配置：$POLICY_FILE（数据源 dev-pg，DSN 走 HJZL_DEV_PG_DSN）"
else
  echo "==> 未提供 --pg-dsn：使用 config/policy.toml 原样（db_query_readonly 会返回 MCP_NOT_IMPLEMENTED，属预期）"
fi

# ------------------------------------------------------------ 后端
SERVER_BIN="$BACKEND_DIR/target/debug/hjzl-server"
if [ ! -x "$SERVER_BIN" ] && [ -x "$BACKEND_DIR/target/debug/hjzl-server.exe" ]; then
  SERVER_BIN="$BACKEND_DIR/target/debug/hjzl-server.exe"
fi
if [ ! -x "$SERVER_BIN" ]; then
  echo "==> 首次运行：cargo build --bin hjzl-server"
  ( cd "$BACKEND_DIR" && cargo build --bin hjzl-server ) || { echo "后端二进制构建失败" >&2; exit 1; }
fi

BACKEND_LOG="$LOG_DIR/backend.log"
(
  cd "$REPO_ROOT" || exit 1
  export HJZL_MCP_TRANSPORT=streamable-http
  export HJZL_MCP_HOST=127.0.0.1
  export HJZL_MCP_PORT="$BACKEND_PORT"
  export HJZL_MCP_CONFIG_FILE="$POLICY_FILE"
  export HJZL_MCP_REGISTRY_PATH="$DATA_DIR/registry.sqlite3"
  export HJZL_MCP_AUDIT_PATH="$DATA_DIR/audit.sqlite3"
  export HJZL_ADMIN_TOKENS="$ADMIN_TOKENS"
  exec "${BACKEND_ENV[@]}" "$SERVER_BIN"
) >"$BACKEND_LOG" 2>&1 &
BACKEND_PID=$!
disown "$BACKEND_PID" 2>/dev/null || true
echo "==> 后端已启动（pid $BACKEND_PID）：http://127.0.0.1:$BACKEND_PORT"

HEALTH_FILE="$DATA_DIR/health.json"
if ! wait_http "http://127.0.0.1:$BACKEND_PORT/health" 30 "$HEALTH_FILE"; then
  echo "后端未在 30s 内就绪（端口 $BACKEND_PORT 是否被占用？）" >&2
  echo "---- backend.log 尾部 ----"
  tail -n 30 "$BACKEND_LOG" 2>/dev/null
  echo "----------------------"
  exit 1
fi
CHAIN="$(node -e 'try{console.log(JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).audit_chain)}catch(e){console.log("unknown")}' "$HEALTH_FILE")"
if [ "$CHAIN" != "valid" ]; then
  echo "警告：audit_chain = $CHAIN（审计链异常），服务仍继续运行，请检查 .dev-data/audit.sqlite3"
fi

# ------------------------------------------------------------ 前端
WEB_PORT_USED=0
if [ "$NO_WEB" != "1" ]; then
  candidate="$WEB_PORT"
  while [ "$candidate" -lt $(( WEB_PORT + 20 )) ] && ! port_free "$candidate"; do
    candidate=$(( candidate + 1 ))
  done
  if [ "$candidate" -ge $(( WEB_PORT + 20 )) ]; then
    echo "找不到可用前端端口（$WEB_PORT-$(( WEB_PORT + 19 ))）" >&2
    exit 1
  fi
  if [ "$candidate" != "$WEB_PORT" ]; then
    echo "端口 $WEB_PORT 被占用，前端改用 $candidate"
  fi
  WEB_PORT_USED="$candidate"

  # 前端代理目标由 HJZL_DEV_API_TARGET 指定（默认 http://127.0.0.1:8000），与 token 一样只存在于 Node 进程环境
  VITE_ARGS=(run dev -- --host 127.0.0.1 --port "$WEB_PORT_USED")
  API_TARGET="http://127.0.0.1:$BACKEND_PORT"
  echo "==> 前端代理目标：$API_TARGET"

  WEB_LOG="$LOG_DIR/web.log"
  (
    cd "$WEB_DIR" || exit 1
    export HJZL_DEV_ADMIN_TOKEN="$ALICE_TOKEN"
    export HJZL_DEV_API_TARGET="$API_TARGET"
    exec npm "${VITE_ARGS[@]}"
  ) >"$WEB_LOG" 2>&1 &
  WEB_PID=$!
  disown "$WEB_PID" 2>/dev/null || true
  echo "==> 前端已启动（pid $WEB_PID）：http://127.0.0.1:$WEB_PORT_USED"

  if ! wait_http "http://127.0.0.1:$WEB_PORT_USED/" 20 "$DATA_DIR/index.html"; then
    echo "前端未在 20s 内就绪" >&2
    echo "---- web.log 尾部 ----"
    tail -n 30 "$WEB_LOG" 2>/dev/null
    echo "---------------------"
    exit 1
  fi
fi

# ------------------------------------------------------------ 横幅
if [ "$NO_WEB" = "1" ]; then
  WEB_LINE="管理台   （未启动：--no-web）"
else
  WEB_LINE="管理台   http://127.0.0.1:$WEB_PORT_USED"
fi
echo ""
echo "================ 开发环境已就绪 ================"
echo "$WEB_LINE"
echo "MCP      http://127.0.0.1:$BACKEND_PORT/mcp"
echo "健康     http://127.0.0.1:$BACKEND_PORT/health"
echo "令牌     alice=$ALICE_TOKEN （普通）  bob=$BOB_TOKEN （审批）"
echo "数据     .dev-data/{registry,audit}.sqlite3   日志  .dev-data/logs/"
echo "停止     Ctrl+C（会同时停掉前后端）"
echo "================================================="
echo ""

# ------------------------------------------------------------ 前台等待
while :; do
  if ! kill -0 "$BACKEND_PID" 2>/dev/null; then
    echo "后端已退出"
    break
  fi
  if [ -n "$WEB_PID" ] && ! kill -0 "$WEB_PID" 2>/dev/null; then
    echo "前端已退出"
    break
  fi
  sleep 1
done
