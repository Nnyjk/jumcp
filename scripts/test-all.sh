#!/usr/bin/env bash
# 一键本地测试：后端静态检查 + 单元/契约测试 + 前端类型检查/构建/lint + 端到端冒烟。
#
# 用法：scripts/test-all.sh [--skip-e2e] [--clickhouse-dsn <dsn>] [--no-clean]
#   --skip-e2e        只跑静态检查与测试，不做端到端冒烟
#   --clickhouse-dsn <dsn> 设置 JUMCP_TEST_CLICKHOUSE_DSN，并额外跑 #[ignore] 的真库用例
#   --no-clean        保留临时数据目录（含服务日志）便于排查
#
# 退出码：0 = 全部通过；1 = 有步骤失败；2 = 参数错误。

set -uo pipefail

# 中文输出不乱码：locale 非 UTF-8 时切到 C.UTF-8（macOS 自带 UTF-8 locale，不受影响）
if ! locale charmap 2>/dev/null | grep -qi "utf-\?8"; then
  export LC_ALL=C.UTF-8 LANG=C.UTF-8
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BACKEND_DIR="$REPO_ROOT/backend"
WEB_DIR="$REPO_ROOT/admin-web"
POLICY_FILE="$REPO_ROOT/config/policy.toml"

ADMIN_TOKENS="alice:tok_alice_000000000,bob:tok_bob_00000000000"
ALICE_TOKEN="tok_alice_000000000"
# /mcp 的 per-request 身份（agent_id:role:token）；E2E 用 analyst 主体取工具清单
AGENT_TOKENS="agent-analyst:analyst:tok_agent_analyst_0001,agent-admin:admin:tok_agent_admin_000001"
ANALYST_AGENT_TOKEN="tok_agent_analyst_0001"
# E2E 访问地址；默认 127.0.0.1，可用 JUMCP_TEST_E2E_HOST 覆盖（如 WSL 里访问 Windows 宿主）
E2E_HOST="${JUMCP_TEST_E2E_HOST:-127.0.0.1}"
# 监听地址；默认 0.0.0.0，可用 JUMCP_TEST_BIND_HOST 覆盖
BIND_HOST="${JUMCP_TEST_BIND_HOST:-0.0.0.0}"

SKIP_E2E=0
NO_CLEAN=0
CLICKHOUSE_DSN=""

usage() {
  sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --skip-e2e) SKIP_E2E=1 ;;
    --no-clean) NO_CLEAN=1 ;;
    --clickhouse-dsn)
      shift
      [ $# -gt 0 ] || { echo "错误：--clickhouse-dsn 需要一个参数" >&2; exit 2; }
      CLICKHOUSE_DSN="$1"
      ;;
    --clickhouse-dsn=*) CLICKHOUSE_DSN="${1#*=}" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "错误：未知参数 '$1'" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

# 结果记录
STEP_NAMES=()
STEP_STATUS=()
STEP_SECONDS=()
STEP_LOGS=()
PASSED=0
FAILED=0
TMP_DIR=""
SERVER_PID=""

now_ms() {
  if [ -n "${EPOCHREALTIME:-}" ]; then
    awk -v t="$EPOCHREALTIME" 'BEGIN{split(t,a,",");printf "%d", a[1]*1000 + a[2]/1000}'
  else
    echo "$(( $(date +%s) * 1000 ))"
  fi
}

fmt_ms() {
  awk -v ms="${1:-0}" 'BEGIN{printf "%.1f", ms/1000}'
}

record() {
  # record <name> <PASS|FAIL> <seconds> <logfile>
  STEP_NAMES+=("$1")
  STEP_STATUS+=("$2")
  STEP_SECONDS+=("$3")
  STEP_LOGS+=("${4:-}")
  if [ "$2" = "PASS" ]; then PASSED=$((PASSED + 1)); else FAILED=$((FAILED + 1)); fi
}

print_summary() {
  echo
  echo "================ 汇总 ================"
  printf "%-46s %-6s %8s\n" "STEP" "STATUS" "SECONDS"
  printf "%-46s %-6s %8s\n" "----------------------------------------------" "------" "--------"
  i=0
  while [ "$i" -lt "${#STEP_NAMES[@]}" ]; do
    printf "%-46s %-6s %8s\n" "${STEP_NAMES[$i]}" "${STEP_STATUS[$i]}" "$(fmt_ms "${STEP_SECONDS[$i]}")"
    i=$((i + 1))
  done
  echo "----------------------------------------------"
  printf "Total: %d passed, %d failed\n" "$PASSED" "$FAILED"
}

cleanup() {
  # 保留原始退出码：EXIT trap 里最后一条命令的状态会覆盖脚本退出码
  local rc=$?
  trap - EXIT
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null
    i=0
    while [ "$i" -lt 5 ] && kill -0 "$SERVER_PID" 2>/dev/null; do
      sleep 1
      i=$((i + 1))
    done
    kill -9 "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
  if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    if [ "$NO_CLEAN" = "1" ]; then
      echo "临时目录已保留：$TMP_DIR"
    else
      rm -rf "$TMP_DIR"
    fi
  fi
  exit $rc
}
trap cleanup EXIT

fail_step() {
  # fail_step <step-name> <logfile> [extra-log]：打印日志尾部，输出汇总，退出 1
  local name="$1"
  local log="${2:-}"
  local extra="${3:-}"
  if [ -n "$log" ] && [ -f "$log" ]; then
    echo "---- $name 输出尾部 ----"
    tail -n 40 "$log"
    echo "------------------------------"
  fi
  if [ -n "$extra" ] && [ -f "$extra" ] && [ "$extra" != "$log" ]; then
    echo "---- 服务日志尾部 ----"
    tail -n 20 "$extra"
    echo "------------------------------"
  fi
  print_summary
  exit 1
}

run_step() {
  # run_step <name> <workdir> <cmd...>
  local name="$1"
  local workdir="$2"
  shift 2
  local log="$TMP_DIR/$(printf '%s' "$name" | tr -c 'A-Za-z0-9' '_').log"
  echo "==> $name"
  local start now elapsed
  start="$(now_ms)"
  if ( cd "$workdir" && "$@" ) >"$log" 2>&1; then
    elapsed=$(( $(now_ms) - start ))
    record "$name" "PASS" "$elapsed" "$log"
    return 0
  fi
  elapsed=$(( $(now_ms) - start ))
  record "$name" "FAIL" "$elapsed" "$log"
  fail_step "$name" "$log"
}

# 0. 参数后准备
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/jumcp-testall.XXXXXX")"
if [ -n "$CLICKHOUSE_DSN" ]; then
  export JUMCP_TEST_CLICKHOUSE_DSN="$CLICKHOUSE_DSN"
fi

# 1. 环境探测
echo "==> env: toolchain probe"
ENV_LOG="$TMP_DIR/env.log"
MISSING=""
for tool in cargo node npm curl; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    MISSING="$MISSING $tool"
  fi
done
{
  echo "cwd: $REPO_ROOT"
  for tool in cargo node npm curl; do
    if command -v "$tool" >/dev/null 2>&1; then
      echo "$tool: $(command -v "$tool")"
    else
      echo "$tool: MISSING"
    fi
  done
} >"$ENV_LOG" 2>&1

if [ -n "$MISSING" ]; then
  record "env: toolchain probe" "FAIL" "0" "$ENV_LOG"
  {
    echo "缺少必需工具：$MISSING"
    echo "安装指引："
    echo "  cargo → Rust 工具链：https://rustup.rs （或 apt/brew install rust）"
    echo "  node  → Node.js LTS 18+：https://nodejs.org （或 apt/brew install nodejs）"
    echo "  npm   → 随 Node.js 一并安装；确认 'npm -v' 可用"
    echo "  curl  → macOS/Linux 自带；apt install curl / brew install curl"
    echo "  若本机已装，请确认已加入 PATH。"
  } >>"$ENV_LOG"
  cat "$ENV_LOG"
  fail_step "env: toolchain probe" "$ENV_LOG"
fi
record "env: toolchain probe" "PASS" "0" "$ENV_LOG"
cat "$ENV_LOG"

# 2. 后端
run_step "backend: cargo fmt --check" "$BACKEND_DIR" cargo fmt --check
run_step "backend: cargo clippy -D warnings" "$BACKEND_DIR" cargo clippy --all-targets -- -D warnings
run_step "backend: cargo test --workspace" "$BACKEND_DIR" cargo test --workspace
if [ -n "$CLICKHOUSE_DSN" ]; then
  cargo build --manifest-path "$BACKEND_DIR/Cargo.toml" -p jumcp-connector-clickhouse >/dev/null 2>&1 || true
  run_step "backend: cargo test --ignored (真库)" "$BACKEND_DIR" cargo test --workspace -- --ignored
fi

# 3. 前端
run_step "frontend: npm ci" "$WEB_DIR" npm ci
run_step "frontend: npm run typecheck" "$WEB_DIR" npm run typecheck
run_step "frontend: npm run build" "$WEB_DIR" npm run build
run_step "frontend: eslint src --max-warnings 0" "$WEB_DIR" npx eslint src --max-warnings 0

# 4. E2E 冒烟
if [ "$SKIP_E2E" = "1" ]; then
  echo "跳过 E2E 冒烟（--skip-e2e）"
  print_summary
  exit 0
fi

run_step "e2e: build jumcp-server" "$BACKEND_DIR" cargo build --bin jumcp-server

SERVER_BIN="$BACKEND_DIR/target/debug/jumcp-server"
# Windows 工具链（如 WSL 里跑 Windows 版 cargo）产出的是 .exe
if [ ! -x "$SERVER_BIN" ] && [ -x "$BACKEND_DIR/target/debug/jumcp-server.exe" ]; then
  SERVER_BIN="$BACKEND_DIR/target/debug/jumcp-server.exe"
fi
if [ ! -x "$SERVER_BIN" ]; then
  record "e2e: 启动服务" "FAIL" "0" ""
  echo "未找到服务二进制：$SERVER_BIN"
  fail_step "e2e: 启动服务" ""
fi

# 顺序探测空闲端口（不使用固定端口）
PORT=""
candidate=18100
while [ "$candidate" -lt 18200 ]; do
  if node -e 'const net=require("net");const s=net.createServer();s.once("error",()=>process.exit(1));s.listen(Number(process.argv[1]),process.argv[2],()=>s.close(()=>process.exit(0)))' "$candidate" "$BIND_HOST" >/dev/null 2>&1; then
    PORT="$candidate"
    break
  fi
  candidate=$((candidate + 1))
done
if [ -z "$PORT" ]; then
  record "e2e: 选端口" "FAIL" "0" ""
  echo "18100-18199 范围内没有空闲端口"
  fail_step "e2e: 选端口" ""
fi
echo "==> e2e: 启动服务（监听 $BIND_HOST:$PORT，E2E 访问 http://$E2E_HOST:$PORT）"
SERVER_LOG="$TMP_DIR/server.log"
CHECK_LOG="$TMP_DIR/checks.log"
(
  cd "$REPO_ROOT" || exit 1
  export JUMCP_MCP_TRANSPORT=streamable-http
  export JUMCP_MCP_HOST="$BIND_HOST"
  export JUMCP_MCP_PORT="$PORT"
  export JUMCP_MCP_CONFIG_FILE="$POLICY_FILE"
  export JUMCP_MCP_REGISTRY_PATH="$TMP_DIR/registry.sqlite3"
  export JUMCP_MCP_AUDIT_PATH="$TMP_DIR/audit.sqlite3"
  export JUMCP_ADMIN_TOKENS="$ADMIN_TOKENS"
  export JUMCP_AGENT_TOKENS="$AGENT_TOKENS"
  exec "$SERVER_BIN"
) >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

BASE_URL="http://$E2E_HOST:$PORT"
HEALTH_FILE="$TMP_DIR/health.json"

# 4a. 健康检查：最多 30s
echo "==> e2e: health /health"
HEALTH_START="$(now_ms)"
HEALTH_DEADLINE=$(( HEALTH_START + 30000 ))
HEALTH_OK=0
code="000"
while [ "$(now_ms)" -lt "$HEALTH_DEADLINE" ]; do
  code="$(curl -sS --connect-timeout 2 --max-time 3 -o "$HEALTH_FILE" -w "%{http_code}" "$BASE_URL/health" 2>/dev/null || echo 000)"
  if [ "$code" = "200" ]; then
    HEALTH_OK=1
    break
  fi
  sleep 1
done
if [ "$HEALTH_OK" != "1" ]; then
  record "e2e: health /health" "FAIL" "$(( $(now_ms) - HEALTH_START ))" "$SERVER_LOG"
  echo "服务在 30s 内未就绪（最后一次状态码：${code:-none}）"
  fail_step "e2e: health /health" "$SERVER_LOG"
fi
if ! node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(d.audit_chain!=="valid"){console.error("audit_chain="+JSON.stringify(d.audit_chain));process.exit(1)}' "$HEALTH_FILE" >>"$CHECK_LOG" 2>&1; then
  record "e2e: health /health" "FAIL" "$(( $(now_ms) - HEALTH_START ))" "$CHECK_LOG"
  fail_step "e2e: health /health（audit_chain 非 valid）" "$CHECK_LOG" "$SERVER_LOG"
fi
record "e2e: health /health" "PASS" "$(( $(now_ms) - HEALTH_START ))" "$SERVER_LOG"

# 4b. 管理面鉴权：匿名 401 / alice 200 且含 items
echo "==> e2e: admin auth /admin/review-queue"
AUTH_FILE="$TMP_DIR/review_queue.json"
AUTH_START="$(now_ms)"
code="$(curl -sS --connect-timeout 2 --max-time 5 -o "$AUTH_FILE" -w "%{http_code}" "$BASE_URL/admin/review-queue" 2>/dev/null || echo 000)"
if [ "$code" != "401" ]; then
  record "e2e: admin auth /admin/review-queue" "FAIL" "$(( $(now_ms) - AUTH_START ))" ""
  echo "匿名访问 /admin/review-queue 期望 401，实际 $code"
  fail_step "e2e: admin auth /admin/review-queue" ""
fi
code="$(curl -sS --connect-timeout 2 --max-time 5 -o "$AUTH_FILE" -w "%{http_code}" -H "Authorization: Bearer $ALICE_TOKEN" "$BASE_URL/admin/review-queue" 2>/dev/null || echo 000)"
if [ "$code" != "200" ]; then
  record "e2e: admin auth /admin/review-queue" "FAIL" "$(( $(now_ms) - AUTH_START ))" ""
  echo "带 alice token 访问 /admin/review-queue 期望 200，实际 $code"
  fail_step "e2e: admin auth /admin/review-queue" ""
fi
if ! node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(!Array.isArray(d.items)){console.error("响应缺少 items 字段");process.exit(1)}' "$AUTH_FILE" >>"$CHECK_LOG" 2>&1; then
  record "e2e: admin auth /admin/review-queue" "FAIL" "$(( $(now_ms) - AUTH_START ))" "$CHECK_LOG"
  fail_step "e2e: admin auth /admin/review-queue（响应缺少 items）" "$CHECK_LOG" "$SERVER_LOG"
fi
record "e2e: admin auth /admin/review-queue" "PASS" "$(( $(now_ms) - AUTH_START ))" "$SERVER_LOG"

# 4c. MCP：initialize（带 _meta.mcp.groups）→ tools/list
echo "==> e2e: mcp initialize + tools/list"
MCP_START="$(now_ms)"
INIT_FILE="$TMP_DIR/mcp_init.json"
TOOLS_FILE="$TMP_DIR/mcp_tools.json"
INIT_BODY='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"_meta":{"mcp":{"groups":["db.read.core"]}}}}'
code="$(curl -sS --connect-timeout 2 --max-time 5 -o "$INIT_FILE" -w "%{http_code}" -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $ANALYST_AGENT_TOKEN" --data "$INIT_BODY" "$BASE_URL/mcp" 2>/dev/null || echo 000)"
if [ "$code" != "200" ]; then
  record "e2e: mcp initialize + tools/list" "FAIL" "$(( $(now_ms) - MCP_START ))" ""
  echo "POST /mcp initialize 期望 200，实际 $code"
  fail_step "e2e: mcp initialize + tools/list" ""
fi
if ! node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(!d.result||!d.result.serverInfo){console.error("initialize 响应缺少 result.serverInfo: "+JSON.stringify(d).slice(0,300));process.exit(1)}' "$INIT_FILE" >>"$CHECK_LOG" 2>&1; then
  record "e2e: mcp initialize + tools/list" "FAIL" "$(( $(now_ms) - MCP_START ))" "$CHECK_LOG"
  fail_step "e2e: mcp initialize + tools/list（initialize 响应异常）" "$CHECK_LOG" "$SERVER_LOG"
fi
LIST_BODY='{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
code="$(curl -sS --connect-timeout 2 --max-time 5 -o "$TOOLS_FILE" -w "%{http_code}" -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $ANALYST_AGENT_TOKEN" --data "$LIST_BODY" "$BASE_URL/mcp" 2>/dev/null || echo 000)"
if [ "$code" != "200" ]; then
  record "e2e: mcp initialize + tools/list" "FAIL" "$(( $(now_ms) - MCP_START ))" ""
  echo "POST /mcp tools/list 期望 200，实际 $code"
  fail_step "e2e: mcp initialize + tools/list" ""
fi
if ! node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));const names=((d.result||{}).tools||[]).map(t=>t.name);const want=["search_capabilities","describe_capability","list_groups"];const missing=want.filter(n=>!names.includes(n));if(missing.length){console.error("tools/list 缺少: "+missing.join(","));process.exit(1)}' "$TOOLS_FILE" >>"$CHECK_LOG" 2>&1; then
  record "e2e: mcp initialize + tools/list" "FAIL" "$(( $(now_ms) - MCP_START ))" "$CHECK_LOG"
  fail_step "e2e: mcp initialize + tools/list（工具清单不完整）" "$CHECK_LOG" "$SERVER_LOG"
fi
record "e2e: mcp initialize + tools/list" "PASS" "$(( $(now_ms) - MCP_START ))" "$SERVER_LOG"

# 4d. 审计事件
echo "==> e2e: admin /admin/audit-events"
AUDIT_FILE="$TMP_DIR/audit_events.json"
AUDIT_START="$(now_ms)"
code="$(curl -sS --connect-timeout 2 --max-time 5 -o "$AUDIT_FILE" -w "%{http_code}" -H "Authorization: Bearer $ALICE_TOKEN" "$BASE_URL/admin/audit-events?limit=5" 2>/dev/null || echo 000)"
if [ "$code" != "200" ]; then
  record "e2e: admin /admin/audit-events" "FAIL" "$(( $(now_ms) - AUDIT_START ))" ""
  echo "GET /admin/audit-events 期望 200，实际 $code"
  fail_step "e2e: admin /admin/audit-events" ""
fi
if ! node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(typeof d.total!=="number"||d.total<1){console.error("audit-events total="+JSON.stringify(d.total));process.exit(1)}if(d.order!=="desc"){console.error("audit-events 默认 order 应为 desc，实际 "+JSON.stringify(d.order));process.exit(1)}const K=["action","approval","changes","correlation_id","data","error_code","error_message","from_status","object_id","object_type","outcome","reason","schema_version","to_status"].sort().join(",");const LEG=["diff","approval_id","capability_id","before_hash","after_hash","version"];for(const e of d.events||[]){const p=e.payload||{};if(p.schema_version!==1){console.error("payload.schema_version="+JSON.stringify(p.schema_version)+" event="+e.event_id);process.exit(1)}if(Object.keys(p).sort().join(",")!==K){console.error("payload 键集不是 14 键: "+Object.keys(p).join(","));process.exit(1)}if(LEG.some(k=>k in p)){console.error("payload 残留旧字段: "+e.event_id);process.exit(1)}if(!p.object_type||!p.object_id||!p.action||!p.outcome){console.error("四件套缺失: "+e.event_id);process.exit(1)}}' "$AUDIT_FILE" >>"$CHECK_LOG" 2>&1; then
  record "e2e: admin /admin/audit-events" "FAIL" "$(( $(now_ms) - AUDIT_START ))" "$CHECK_LOG"
  fail_step "e2e: admin /admin/audit-events（total/order/payload 契约断言失败）" "$CHECK_LOG" "$SERVER_LOG"
fi
record "e2e: admin /admin/audit-events" "PASS" "$(( $(now_ms) - AUDIT_START ))" "$SERVER_LOG"

# 4e. 平台 token：创建（仅 db.read.core + 两项内置工具）→ tools/list 过滤 → 撤销后拒绝
echo "==> e2e: admin token create + mcp tools/list filter"
TOKEN_START="$(now_ms)"
TOKEN_FILE="$TMP_DIR/token_create.json"
TOKEN_BODY='{"name":"e2e-只读组","role":"analyst","groups":["db.read.core"],"builtin_tools":["search_capabilities","describe_capability"],"source":"e2e","caller":"e2e-agent","expires_at":null}'
# body 走临时文件而不是 argv：Windows 原生 curl.exe 会把含中文的 argv 按 ANSI 代码页（如 CP936）转码，
# 服务端 serde_json 因非法 UTF-8 直接 400。@文件 原样传 UTF-8 字节；文件在 TMP_DIR 内随脚本清理。
TOKEN_BODY_FILE="$TMP_DIR/token_create_body.json"
printf '%s' "$TOKEN_BODY" >"$TOKEN_BODY_FILE"
code="$(curl -sS --connect-timeout 2 --max-time 5 -o "$TOKEN_FILE" -w "%{http_code}" -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $ALICE_TOKEN" --data-binary "@$TOKEN_BODY_FILE" "$BASE_URL/admin/tokens" 2>/dev/null || echo 000)"
if [ "$code" != "201" ]; then
  record "e2e: admin token create + mcp tools/list filter" "FAIL" "$(( $(now_ms) - TOKEN_START ))" ""
  echo "POST /admin/tokens 期望 201，实际 $code"
  fail_step "e2e: admin token create + mcp tools/list filter" ""
fi
if ! node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));const t=d.token||"";if(!t.startsWith("jumcp_")||t.length!==70){console.error("token 明文形状非法: "+t.slice(0,8));process.exit(1)}const i=d.item||{};if(i.status!=="active"){console.error("item.status="+i.status);process.exit(1)}if((i.builtin_tools||[]).length!==2){console.error("builtin_tools="+JSON.stringify(i.builtin_tools));process.exit(1)}if(i.token!==undefined||i.token_hash!==undefined){console.error("item 泄露明文/hash");process.exit(1)}' "$TOKEN_FILE" >>"$CHECK_LOG" 2>&1; then
  record "e2e: admin token create + mcp tools/list filter" "FAIL" "$(( $(now_ms) - TOKEN_START ))" "$CHECK_LOG"
  fail_step "e2e: admin token create + mcp tools/list filter（创建响应契约不符）" "$CHECK_LOG" "$SERVER_LOG"
fi
PLATFORM_TOKEN="$(node -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).token)' "$TOKEN_FILE")"
PLATFORM_TOKEN_ID="$(node -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).item.id)' "$TOKEN_FILE")"
PLATFORM_TOOLS="$TMP_DIR/platform_tools.json"
curl -sS --connect-timeout 2 --max-time 5 -o "$PLATFORM_TOOLS" -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $PLATFORM_TOKEN" --data "$LIST_BODY" "$BASE_URL/mcp" >/dev/null 2>&1 || true
if ! node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));const names=((d.result||{}).tools||[]).map(t=>t.name);const need=["search_capabilities","describe_capability"];const miss=need.filter(n=>!names.includes(n));if(miss.length){console.error("缺少 allowlist 内工具: "+miss.join(",")+" names="+names.join(","));process.exit(1)}const forbidden=names.filter(n=>["list_groups"].includes(n)||n.startsWith("admin_"));if(forbidden.length){console.error("出现未授权工具: "+forbidden.join(","));process.exit(1)}' "$PLATFORM_TOOLS" >>"$CHECK_LOG" 2>&1; then
  record "e2e: admin token create + mcp tools/list filter" "FAIL" "$(( $(now_ms) - TOKEN_START ))" "$CHECK_LOG"
  fail_step "e2e: admin token create + mcp tools/list filter（allowlist 过滤失败）" "$CHECK_LOG" "$SERVER_LOG"
fi
REVOKE_FILE="$TMP_DIR/token_revoke.json"
code="$(curl -sS --connect-timeout 2 --max-time 5 -o "$REVOKE_FILE" -w "%{http_code}" -X POST -H "Authorization: Bearer $ALICE_TOKEN" "$BASE_URL/admin/tokens/$PLATFORM_TOKEN_ID/revoke" 2>/dev/null || echo 000)"
if [ "$code" != "200" ]; then
  record "e2e: admin token create + mcp tools/list filter" "FAIL" "$(( $(now_ms) - TOKEN_START ))" ""
  echo "POST /admin/tokens/{id}/revoke 期望 200，实际 $code"
  fail_step "e2e: admin token create + mcp tools/list filter" ""
fi
if ! node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(d.status!=="revoked"||!d.revoked_at){console.error("撤销响应非 revoked/item: "+JSON.stringify(d).slice(0,200));process.exit(1)}if(d.item!==undefined){console.error("撤销响应不得再包 item");process.exit(1)}' "$REVOKE_FILE" >>"$CHECK_LOG" 2>&1; then
  record "e2e: admin token create + mcp tools/list filter" "FAIL" "$(( $(now_ms) - TOKEN_START ))" "$CHECK_LOG"
  fail_step "e2e: admin token create + mcp tools/list filter（撤销响应契约不符）" "$CHECK_LOG" "$SERVER_LOG"
fi
AFTER_REVOKE="$TMP_DIR/platform_tools_revoked.json"
curl -sS --connect-timeout 2 --max-time 5 -o "$AFTER_REVOKE" -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $PLATFORM_TOKEN" --data "$LIST_BODY" "$BASE_URL/mcp" >/dev/null 2>&1 || true
if ! node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));const code=(((d.error||{}).data)||{}).mcp_code;if(code!=="MCP_UNAUTHORIZED"){console.error("撤销后应拒绝，实际 mcp_code="+code);process.exit(1)}' "$AFTER_REVOKE" >>"$CHECK_LOG" 2>&1; then
  record "e2e: admin token create + mcp tools/list filter" "FAIL" "$(( $(now_ms) - TOKEN_START ))" "$CHECK_LOG"
  fail_step "e2e: admin token create + mcp tools/list filter（撤销后仍可用）" "$CHECK_LOG" "$SERVER_LOG"
fi
record "e2e: admin token create + mcp tools/list filter" "PASS" "$(( $(now_ms) - TOKEN_START ))" "$SERVER_LOG"

print_summary
if [ "$FAILED" -gt 0 ]; then
  exit 1
fi
exit 0
