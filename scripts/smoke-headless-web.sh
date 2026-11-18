#!/usr/bin/env bash
# 无头 Web 服务校验：用 `vite preview` 提供 admin-web/dist 生产构建产物，断言同源静态资源与 SPA 回退可用。
# 用途：CI 的「无头 Web 服务」产物校验（构建产物真的能当静态站点跑起来，而不是只有 dist 目录）。
#
# 用法：scripts/smoke-headless-web.sh [--port <port>] [--timeout <seconds>]
#   --port     监听端口（默认 4173，可用 JUMCP_SMOKE_WEB_PORT 覆盖）
#   --timeout  等待就绪的秒数（默认 30）
#
# 前置：admin-web/node_modules 与 admin-web/dist 已就绪（先 `npm ci && npm run build`）。
# 退出码：0 = 全部断言通过；1 = 断言失败；2 = 参数/前置缺失。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WEB_DIR="$REPO_ROOT/admin-web"
DIST_DIR="$WEB_DIR/dist"
HOST=127.0.0.1
PORT="${JUMCP_SMOKE_WEB_PORT:-4173}"
TIMEOUT_S=30

usage() {
  sed -n '2,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --port)
      shift
      [ $# -gt 0 ] || { echo "错误：--port 需要一个参数" >&2; exit 2; }
      PORT="$1"
      ;;
    --port=*) PORT="${1#*=}" ;;
    --timeout)
      shift
      [ $# -gt 0 ] || { echo "错误：--timeout 需要一个参数" >&2; exit 2; }
      TIMEOUT_S="$1"
      ;;
    --timeout=*) TIMEOUT_S="${1#*=}" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "错误：未知参数 '$1'" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

TMP_ROOT=""
VITE_PID=""

cleanup() {
  local rc=$?
  trap - EXIT
  if [ -n "$VITE_PID" ] && kill -0 "$VITE_PID" 2>/dev/null; then
    kill "$VITE_PID" 2>/dev/null || true
    i=0
    while [ "$i" -lt 25 ] && kill -0 "$VITE_PID" 2>/dev/null; do
      sleep 0.2
      i=$((i + 1))
    done
    kill -9 "$VITE_PID" 2>/dev/null || true
    wait "$VITE_PID" 2>/dev/null || true
  fi
  VITE_PID=""
  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    rm -rf "$TMP_ROOT"
  fi
  exit $rc
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  if [ -n "$TMP_ROOT" ] && [ -f "$TMP_ROOT/vite.log" ]; then
    echo "---- vite preview 日志尾部 ----" >&2
    tail -n 40 "$TMP_ROOT/vite.log" >&2
    echo "-------------------------------" >&2
  fi
  exit 1
}

# 前置检查
command -v curl >/dev/null 2>&1 || { echo "错误：缺少 curl" >&2; exit 2; }
command -v node >/dev/null 2>&1 || { echo "错误：缺少 node" >&2; exit 2; }
[ -f "$DIST_DIR/index.html" ] || {
  echo "错误：未找到 $DIST_DIR/index.html —— 先执行 (cd admin-web && npm ci && npm run build)" >&2
  exit 2
}
VITE_ENTRY="$WEB_DIR/node_modules/vite/bin/vite.js"
[ -f "$VITE_ENTRY" ] || {
  echo "错误：未找到 $VITE_ENTRY —— 先执行 (cd admin-web && npm ci)" >&2
  exit 2
}

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/jumcp-web-smoke.XXXXXX")"

# 起 vite preview
echo "==> smoke: vite preview（dist=$DIST_DIR，$HOST:$PORT）"
(
  cd "$WEB_DIR" || exit 1
  exec node "$VITE_ENTRY" preview --host "$HOST" --port "$PORT" --strictPort
) >"$TMP_ROOT/vite.log" 2>&1 &
VITE_PID=$!

BASE="http://$HOST:$PORT"
ready=0
deadline=$((SECONDS + TIMEOUT_S))
while [ "$SECONDS" -lt "$deadline" ]; do
  if ! kill -0 "$VITE_PID" 2>/dev/null; then
    fail "vite preview 启动失败（端口 $PORT 可能被占用，可用 --port 换端口）"
  fi
  if curl -fsS --max-time 3 -o "$TMP_ROOT/probe.html" -H 'Accept: text/html' "$BASE/" 2>/dev/null; then
    ready=1
    break
  fi
  sleep 0.5
done
[ "$ready" = "1" ] || fail "vite preview 在 ${TIMEOUT_S}s 内未就绪（$BASE/）"
echo "    preview 已就绪"

# 断言工具
CODE=""
CTYPE=""
get() {
  # get <url> <outfile> [curl 额外参数...]；结果写入 CODE / CTYPE
  local url="$1" out="$2"
  shift 2
  local meta
  meta="$(curl -sS --max-time 10 -o "$out" -w '%{http_code} %{content_type}' "$@" "$url" 2>/dev/null || true)"
  meta="${meta%%$'\n'*}"
  CODE="${meta%% *}"
  CTYPE=""
  case "$meta" in
    *' '*) CTYPE="${meta#* }" ;;
  esac
  case "$CODE" in
    ''|*[!0-9]*) CODE=000 ;;
  esac
}

html_has_root() {
  # 从 index.html 里取 <div id="root"></div>：dist 产物必须保留挂载点，否则前端白屏。
  local html
  html="$(cat "$1")"
  [[ $html == *'id="root"'* ]]
}

# 1. `/` 返回 HTML 且含 #root
get "$BASE/" "$TMP_ROOT/index.html" -H 'Accept: text/html'
[ "$CODE" = "200" ] || fail "GET / 期望 200，实际 $CODE"
case "$CTYPE" in
  text/html*) ;;
  *) fail "GET / 的 Content-Type 期望 text/html，实际 '$CTYPE'" ;;
esac
html_has_root "$TMP_ROOT/index.html" || fail 'GET / 的 HTML 缺少 id="root"'
echo "    ok: GET / → 200 text/html 且含 id=\"root\""

# 2. `/review` 是前端路由，静态托管必须回退到 index.html（200，不是 404）
get "$BASE/review" "$TMP_ROOT/review.html" -H 'Accept: text/html'
[ "$CODE" = "200" ] || fail "GET /review 期望 200（SPA 回退），实际 $CODE"
case "$CTYPE" in
  text/html*) ;;
  *) fail "GET /review 的 Content-Type 期望 text/html，实际 '$CTYPE'" ;;
esac
html_has_root "$TMP_ROOT/review.html" || fail 'GET /review 的回退页缺少 id="root"'
echo "    ok: GET /review → 200 SPA 回退"

# 3. 构建后的 hashed JS 资源真实可取
HTML="$(cat "$TMP_ROOT/index.html")"
ASSET=""
if [[ $HTML =~ src=\"(/assets/[^\"]+\.js)\" ]]; then
  ASSET="${BASH_REMATCH[1]}"
fi
[ -n "$ASSET" ] || fail 'GET / 的 HTML 里找不到 /assets/*.js 构建产物引用'
get "$BASE$ASSET" "$TMP_ROOT/asset.js"
[ "$CODE" = "200" ] || fail "GET $ASSET 期望 200，实际 $CODE"
case "$CTYPE" in
  *javascript*) ;;
  *) fail "GET $ASSET 的 Content-Type 期望 javascript，实际 '$CTYPE'" ;;
esac
ASSET_BYTES="$(wc -c <"$TMP_ROOT/asset.js" | tr -d ' ')"
[ "$ASSET_BYTES" -gt 0 ] || fail "GET $ASSET 返回空文件"
echo "    ok: $ASSET → 200（${ASSET_BYTES} bytes）"

# 4. 未登录访问：静态托管不鉴权，首屏是登录页壳——`/login` 走同一条 SPA 回退，
#    响应里不能出现 Set-Cookie/Location（否则说明静态层在做鉴权或重定向，与管理面契约冲突）。
get "$BASE/login" "$TMP_ROOT/login.html" -H 'Accept: text/html' -D "$TMP_ROOT/login.headers"
[ "$CODE" = "200" ] || fail "未登录 GET /login 期望 200（登录页壳），实际 $CODE"
case "$CTYPE" in
  text/html*) ;;
  *) fail "GET /login 的 Content-Type 期望 text/html，实际 '$CTYPE'" ;;
esac
html_has_root "$TMP_ROOT/login.html" || fail 'GET /login 的登录页壳缺少 id="root"'
if grep -qi '^set-cookie:' "$TMP_ROOT/login.headers"; then
  fail '未登录 GET /login 不应下发 Set-Cookie（静态站不签发会话）'
fi
if grep -qi '^location:' "$TMP_ROOT/login.headers"; then
  fail '未登录 GET /login 不应重定向（SPA 自己决定跳登录页）'
fi
echo "    ok: 未登录 GET /login → 200 登录页壳（无 Set-Cookie / 无重定向）"

echo "==> smoke-headless-web 全部通过"
