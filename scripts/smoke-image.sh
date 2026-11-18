#!/usr/bin/env bash
# 镜像冒烟：起容器（非 root、/data 用 tmpfs）后校验健康、同源管理台静态资源、管理面鉴权、/mcp 与内置 MCP 包安装链路。
# 用途：CI 的镜像 job 与 release 发布后的 digest 回拉校验；本地 `docker build` 后也可直接跑。
#
# 用法：scripts/smoke-image.sh <image> [--port <port>] [--timeout <seconds>]
#   <image>    DOCKER_IMAGE（如 jumcp:ci、ghcr.io/nnyjk/jumcp@sha256:...）
#   --port     宿主机端口（默认随机，通过 `-p 127.0.0.1::8000` 分配）
#   --timeout  等待 /health 就绪的秒数（默认 120）
#
# 需要：docker、curl、jq（GitHub ubuntu-latest 自带）。
# 退出码：0 = 全部断言通过；1 = 断言失败；2 = 参数/工具缺失。

set -euo pipefail

IMAGE=""
PORT=""
TIMEOUT_S=120

# 运行时注入的凭证：镜像本身不含任何 token（HTTP 模式没有 JUMCP_ADMIN_TOKENS 就拒绝启动）。
ADMIN_ID="smoke"
ADMIN_TOKEN="${JUMCP_SMOKE_ADMIN_TOKEN:-tok_smoke_admin_0001}"
AGENT_ID="agent-analyst"
AGENT_ROLE="analyst"
AGENT_TOKEN="${JUMCP_SMOKE_AGENT_TOKEN:-tok_agent_analyst_0001}"
CONTAINER_PORT=8000
CONTAINER_UID=10001

usage() {
  sed -n '2,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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
    -*) echo "错误：未知参数 '$1'" >&2; usage >&2; exit 2 ;;
    *)
      [ -z "$IMAGE" ] || { echo "错误：只接受一个镜像参数" >&2; exit 2; }
      IMAGE="$1"
      ;;
  esac
  shift
done

[ -n "$IMAGE" ] || { echo "错误：缺少镜像参数" >&2; usage >&2; exit 2; }
for tool in docker curl jq; do
  command -v "$tool" >/dev/null 2>&1 || { echo "错误：缺少 $tool" >&2; exit 2; }
done

NAME="jumcp-smoke-$$"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/jumcp-image-smoke.XXXXXX")"
STARTED=0

cleanup() {
  local rc=$?
  trap - EXIT
  if [ "$STARTED" = "1" ]; then
    docker rm -f "$NAME" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP_ROOT"
  exit $rc
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  if [ "$STARTED" = "1" ]; then
    echo "---- 容器状态 ----" >&2
    docker ps -a --filter "name=^${NAME}$" >&2 || true
    echo "---- 容器日志尾部（$NAME）----" >&2
    docker logs --tail 120 "$NAME" >&2 || true
    echo "-------------------" >&2
  fi
  exit 1
}

# 启动容器
PUBLISH_ARGS=(-p "127.0.0.1::${CONTAINER_PORT}")
if [ -n "$PORT" ]; then
  PUBLISH_ARGS=(-p "127.0.0.1:${PORT}:${CONTAINER_PORT}")
fi

# /data 用 tmpfs：既保证非 root（UID 10001）可写，也让每次冒烟都从干净的空库开始。
echo "==> smoke-image: 启动容器（image=$IMAGE，/data=tmpfs）"
docker run -d --name "$NAME" \
  "${PUBLISH_ARGS[@]}" \
  --tmpfs "/data:rw,mode=1777,size=64m" \
  -e "JUMCP_ADMIN_TOKENS=${ADMIN_ID}:${ADMIN_TOKEN}" \
  -e "JUMCP_AGENT_TOKENS=${AGENT_ID}:${AGENT_ROLE}:${AGENT_TOKEN}" \
  -e "JUMCP_SECRET_KEY=${JUMCP_SECRET_KEY:-3333333333333333333333333333333333333333333333333333333333333333}" \
  -e "JUMCP_TRANSPORT_KEY=${JUMCP_TRANSPORT_KEY:-4444444444444444444444444444444444444444444444444444444444444444}" \
  "$IMAGE" >/dev/null || fail "docker run 失败"
STARTED=1

MAPPING="$(docker port "$NAME" "${CONTAINER_PORT}/tcp" 2>/dev/null | head -n 1)"
[ -n "$MAPPING" ] || fail "未能解析宿主端口映射（docker port ${NAME} ${CONTAINER_PORT}/tcp 为空）"
HOST_PORT="${MAPPING##*:}"
BASE="http://127.0.0.1:${HOST_PORT}"
echo "    容器 $NAME → $BASE"

# 等待就绪
echo "==> smoke-image: 等待 /health（最长 ${TIMEOUT_S}s）"
HEALTH_FILE="$TMP_ROOT/health.json"
ready=0
deadline=$((SECONDS + TIMEOUT_S))
while [ "$SECONDS" -lt "$deadline" ]; do
  running="$(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null || echo false)"
  if [ "$running" != "true" ]; then
    fail "容器已退出，未通过 /health 就绪"
  fi
  if curl -fsS --max-time 3 -o "$HEALTH_FILE" "$BASE/health" 2>/dev/null; then
    ready=1
    break
  fi
  sleep 1
done
[ "$ready" = "1" ] || fail "容器在 ${TIMEOUT_S}s 内未通过 /health 就绪"

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

patch_json() {
  # patch_json <url> <outfile> <body> <bearer token>；结果写入 CODE
  local url="$1" out="$2" body="$3" token="$4"
  CODE="$(curl -sS --max-time 15 -o "$out" -w '%{http_code}' \
    -X PATCH -H 'Content-Type: application/json' -H "Authorization: Bearer $token" \
    --data "$body" "$url" 2>/dev/null || true)"
  CODE="${CODE%%$'\n'*}"
  case "$CODE" in
    ''|*[!0-9]*) CODE=000 ;;
  esac
}

post_json() {
  # post_json <url> <outfile> <body> <bearer token>；结果写入 CODE
  local url="$1" out="$2" body="$3" token="$4"
  CODE="$(curl -sS --max-time 15 -o "$out" -w '%{http_code}' \
    -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $token" \
    --data "$body" "$url" 2>/dev/null || true)"
  CODE="${CODE%%$'\n'*}"
  case "$CODE" in
    ''|*[!0-9]*) CODE=000 ;;
  esac
}

json_is() {
  # json_is <file> <jq 过滤表达式> <失败描述>
  jq -e "$2" "$1" >/dev/null 2>&1 || fail "$3（jq 条件：$2）"
}

html_has_root() {
  local html
  html="$(cat "$1")"
  [[ $html == *'id="root"'* ]]
}

# 1. /health 探针分支：不带 text/html 的请求必须是后端 JSON（监控/容器探针不能被前端劫持），
#    且审计链 valid（HTTP 模式强制要求审计路径可写）。
health_json() {
  # health_json <outfile> <场景描述> [curl 额外参数...]
  local out="$1" desc="$2"
  shift 2
  get "$BASE/health" "$out" "$@"
  [ "$CODE" = "200" ] || fail "GET /health（$desc）期望 200，实际 $CODE"
  case "$CTYPE" in
    application/json*) ;;
    *) fail "GET /health（$desc）的 Content-Type 期望 application/json，实际 '$CTYPE'" ;;
  esac
  json_is "$out" '.audit_chain == "valid"' "/health（$desc）的 audit_chain 不是 \"valid\""
}

health_json "$TMP_ROOT/health.json" "无 Accept 头"
health_json "$TMP_ROOT/health-xhr.json" "Accept: application/json" -H 'Accept: application/json'
echo "    ok: /health 探针分支 → 200 JSON 且 audit_chain=valid（无 Accept / Accept: application/json）"

# 2. /health 浏览器导航分支：同源下 UI 健康页可达（Accept 含 text/html 回退到管理台 index.html）
get "$BASE/health" "$TMP_ROOT/health-page.html" -H 'Accept: text/html,application/xhtml+xml,*/*;q=0.8'
[ "$CODE" = "200" ] || fail "GET /health（Accept: text/html）期望 200，实际 $CODE"
case "$CTYPE" in
  text/html*) ;;
  *) fail "GET /health（Accept: text/html）的 Content-Type 期望 text/html，实际 '$CTYPE'" ;;
esac
html_has_root "$TMP_ROOT/health-page.html" || fail 'GET /health（Accept: text/html）的回退页缺少 id="root"'
echo "    ok: GET /health（Accept: text/html）→ 200 text/html 且含 id=\"root\""

# 3. 管理台首页：同源静态资源 + #root 挂载点
get "$BASE/" "$TMP_ROOT/index.html" -H 'Accept: text/html'
[ "$CODE" = "200" ] || fail "GET / 期望 200，实际 $CODE"
case "$CTYPE" in
  text/html*) ;;
  *) fail "GET / 的 Content-Type 期望 text/html，实际 '$CTYPE'" ;;
esac
html_has_root "$TMP_ROOT/index.html" || fail 'GET / 的 HTML 缺少 id="root"（管理台未随镜像提供？）'
echo "    ok: GET / → 200 text/html 且含 id=\"root\""

# 4. /review：前端路由必须由后端回退到 index.html
get "$BASE/review" "$TMP_ROOT/review.html" -H 'Accept: text/html'
[ "$CODE" = "200" ] || fail "GET /review 期望 200（SPA 回退），实际 $CODE"
case "$CTYPE" in
  text/html*) ;;
  *) fail "GET /review 的 Content-Type 期望 text/html，实际 '$CTYPE'" ;;
esac
html_has_root "$TMP_ROOT/review.html" || fail 'GET /review 的回退页缺少 id="root"'
echo "    ok: GET /review → 200 SPA 回退"

# 5. 构建后的 hashed JS 资源真实可取
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

# 6. 管理面鉴权：匿名 401，带管理 token 200 且返回 items 数组
get "$BASE/admin/review-queue" "$TMP_ROOT/anon.json"
[ "$CODE" = "401" ] || fail "匿名 GET /admin/review-queue 期望 401，实际 $CODE"
echo "    ok: 匿名 /admin/review-queue → 401"

get "$BASE/admin/review-queue" "$TMP_ROOT/queue.json" -H "Authorization: Bearer $ADMIN_TOKEN"
[ "$CODE" = "200" ] || fail "带管理 token GET /admin/review-queue 期望 200，实际 $CODE"
json_is "$TMP_ROOT/queue.json" '(.items | type) == "array"' '/admin/review-queue 响应缺少 items 数组'
echo "    ok: 带管理 token /admin/review-queue → 200"

# 7. /mcp：agent token 走完 initialize + tools/list
INIT_BODY='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"_meta":{"mcp":{"groups":["db.read.core"]}}}}'
post_json "$BASE/mcp" "$TMP_ROOT/mcp_init.json" "$INIT_BODY" "$AGENT_TOKEN"
[ "$CODE" = "200" ] || fail "POST /mcp initialize 期望 200，实际 $CODE"
json_is "$TMP_ROOT/mcp_init.json" '.result.serverInfo != null' '/mcp initialize 响应缺少 result.serverInfo'
echo "    ok: POST /mcp initialize → 200"

LIST_BODY='{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
post_json "$BASE/mcp" "$TMP_ROOT/mcp_tools.json" "$LIST_BODY" "$AGENT_TOKEN"
[ "$CODE" = "200" ] || fail "POST /mcp tools/list 期望 200，实际 $CODE"
json_is "$TMP_ROOT/mcp_tools.json" \
  '((.result.tools // []) | map(.name) | index("search_capabilities")) != null' \
  '/mcp tools/list 缺少 search_capabilities'
echo "    ok: POST /mcp tools/list → 200 且含 search_capabilities"

# 8. 内置 MCP 包：目录自带 HTTP 接入信息 → 安装（密码以传输密文提交、响应脱敏）→ PATCH 改配置
#    → 刷新（内嵌端点 tools/list，不连库）→ agent 面可见 → 卸载后目录无残留安装。
#    url 指向 127.0.0.1:8123（不可达）是对的：安装只写配置，刷新只握手 tools/list。
PKG_ID="clickhouse-basic"
PKG_UPSTREAM="clickhouse"
get "$BASE/admin/mcp/catalog" "$TMP_ROOT/catalog.json" -H "Authorization: Bearer $ADMIN_TOKEN"
[ "$CODE" = "200" ] || fail "GET /admin/mcp/catalog 期望 200，实际 $CODE"
json_is "$TMP_ROOT/catalog.json" \
  '(.items[] | select(.id == "clickhouse-basic"))
   | (.transport == "http")
     and (has("binary_present") | not)
     and (has("stdio_enabled") | not)
     and (.fields | map(.key) | index("url") != null)
     and (.fields | map(.key) | index("password") != null)
     and ((.installations | length) == 0)' \
  'GET /admin/mcp/catalog 里 clickhouse-basic 的契约不对（应为 http、无 binary/stdio 字段、含 url/password 字段、无残留安装）'
echo "    ok: GET /admin/mcp/catalog → 200 且 ${PKG_ID} 为 HTTP 包（无 binary/stdio 字段，无残留安装）"

# secret 字段必须密文传输：浏览器用 `GET /admin/transport-key` 下发的同一个
# JUMCP_TRANSPORT_KEY 在本机加密。这里内联一个预先算好的常量（宿主机不一定有 python）：
#   key       = 64 个 4（上面的 JUMCP_TRANSPORT_KEY）
#   nonce     = 000102030405060708090a0b（12 字节）
#   plaintext = smoke-pass
#   ciphertext= enc:v1:‖hex(nonce‖AES-256-GCM(key, nonce, plaintext, aad=""))
# 复算（任意装了 cryptography 的环境）：
#   python -c 'from cryptography.hazmat.primitives.ciphers.aead import AESGCM as A;
#     k=bytes([0x44])*32; n=bytes.fromhex("000102030405060708090a0b");
#     print("enc:v1:"+(n+A(k).encrypt(n,b"smoke-pass",None)).hex())'
SMOKE_PASS_CIPHERTEXT='enc:v1:000102030405060708090a0bb87517b3dd71670b04fe6aaf42a005e3a0486ebca90f17516cae'

# 明文密码必须被拒：不允许"解不开就当明文用"。
PLAINTEXT_BODY='{"group":"db.read.core","config":{"url":"http://127.0.0.1:8123","username":"default","password":"smoke-plaintext"}}'
post_json "$BASE/admin/mcp/catalog/${PKG_ID}/install" "$TMP_ROOT/pkg_plaintext.json" "$PLAINTEXT_BODY" "$ADMIN_TOKEN"
[ "$CODE" = "400" ] || fail "安装时提交明文 password 期望 400，实际 $CODE"
json_is "$TMP_ROOT/pkg_plaintext.json" '.error.code == "MCP_TRANSPORT_CIPHERTEXT_REQUIRED"' \
  '明文 password 的错误码不是 MCP_TRANSPORT_CIPHERTEXT_REQUIRED'
json_is "$TMP_ROOT/pkg_plaintext.json" '((tostring | contains("smoke-plaintext")) | not)' \
  '明文 password 被回显到了错误响应里'
echo "    ok: 安装时提交明文 password → 400（MCP_TRANSPORT_CIPHERTEXT_REQUIRED，未回显明文）"

INSTALL_BODY="{\"group\":\"db.read.core\",\"config\":{\"url\":\"http://127.0.0.1:8123\",\"username\":\"default\",\"password\":\"${SMOKE_PASS_CIPHERTEXT}\"}}"
post_json "$BASE/admin/mcp/catalog/${PKG_ID}/install" "$TMP_ROOT/pkg_install.json" "$INSTALL_BODY" "$ADMIN_TOKEN"
[ "$CODE" = "200" ] || fail "POST /admin/mcp/catalog/${PKG_ID}/install 期望 200，实际 $CODE"
json_is "$TMP_ROOT/pkg_install.json" ".upstream.id == \"${PKG_UPSTREAM}\"" \
  "安装接口返回的上游 id 不是 ${PKG_UPSTREAM}"
json_is "$TMP_ROOT/pkg_install.json" \
  '(.upstream.transport == "http") and (.upstream.env.JUMCP_CLICKHOUSE_PASSWORD == "***")
     and ((tostring | contains("smoke-pass")) | not)
     and ((tostring | contains("enc:v1:")) | not)' \
  '安装响应不合契约（transport 应为 http、密码应为 "***"，且不得出现明文或密文）'
echo "    ok: POST /admin/mcp/catalog/${PKG_ID}/install → 200（上游 ${PKG_UPSTREAM}，transport=http，密码已脱敏）"

post_json "$BASE/admin/upstreams/${PKG_UPSTREAM}/refresh" "$TMP_ROOT/pkg_refresh.json" '{}' "$ADMIN_TOKEN"
[ "$CODE" = "200" ] || fail "POST /admin/upstreams/${PKG_UPSTREAM}/refresh 期望 200，实际 $CODE"
json_is "$TMP_ROOT/pkg_refresh.json" '.status == "ok"' \
  "POST /admin/upstreams/${PKG_UPSTREAM}/refresh 的 status 不是 \"ok\"（内嵌端点未响应或 Bearer 注入失败）"
json_is "$TMP_ROOT/pkg_refresh.json" '.tool_count == 3' \
  "POST /admin/upstreams/${PKG_UPSTREAM}/refresh 的 tool_count 期望 3（tools/list 不需要真连库）"
echo "    ok: POST /admin/upstreams/${PKG_UPSTREAM}/refresh → 200（status=ok，tool_count=3）"

PATCH_BODY='{"config":{"url":"http://127.0.0.1:8124","password":""}}'
patch_json "$BASE/admin/mcp/catalog/${PKG_ID}/install/${PKG_UPSTREAM}/config" "$TMP_ROOT/pkg_patch.json" "$PATCH_BODY" "$ADMIN_TOKEN"
[ "$CODE" = "200" ] || fail "PATCH /admin/mcp/catalog/${PKG_ID}/install/${PKG_UPSTREAM}/config 期望 200，实际 $CODE"
json_is "$TMP_ROOT/pkg_patch.json" \
  '(.installation.config.url == "http://127.0.0.1:8124")
     and (.installation.secrets_set.password == true)
     and ((tostring | contains("smoke-pass")) | not)
     and ((tostring | contains("enc:v1:")) | not)' \
  'PATCH 配置响应不合契约（url 应更新、secrets_set.password=true、且不得出现明文或密文）'
echo "    ok: PATCH .../config → 200（url 已更新，secrets_set.password=true，密码未回显）"

PKG_LIST_BODY='{"jsonrpc":"2.0","id":3,"method":"tools/list"}'
post_json "$BASE/mcp" "$TMP_ROOT/pkg_tools.json" "$PKG_LIST_BODY" "$AGENT_TOKEN"
[ "$CODE" = "200" ] || fail "POST /mcp tools/list（安装 ${PKG_ID} 后）期望 200，实际 $CODE"
json_is "$TMP_ROOT/pkg_tools.json" \
  '((.result.tools // []) | map(.name) | index("clickhouse__query")) != null' \
  "安装后 /mcp tools/list 缺少 clickhouse__query"
echo "    ok: 安装后 POST /mcp tools/list → 200 且含 clickhouse__query"

get "$BASE/admin/mcp/catalog/${PKG_ID}/install/${PKG_UPSTREAM}" "$TMP_ROOT/pkg_uninstall.json" \
  -X DELETE -H "Authorization: Bearer $ADMIN_TOKEN"
[ "$CODE" = "200" ] || fail "DELETE /admin/mcp/catalog/${PKG_ID}/install/${PKG_UPSTREAM} 期望 200，实际 $CODE"
json_is "$TMP_ROOT/pkg_uninstall.json" '.deleted == true' "卸载响应缺少 deleted=true"
get "$BASE/admin/mcp/catalog" "$TMP_ROOT/catalog_after.json" -H "Authorization: Bearer $ADMIN_TOKEN"
[ "$CODE" = "200" ] || fail "GET /admin/mcp/catalog（卸载后）期望 200，实际 $CODE"
json_is "$TMP_ROOT/catalog_after.json" \
  '(.items[] | select(.id == "clickhouse-basic") | .installations | length) == 0' \
  "卸载后 ${PKG_ID} 仍有残留安装"
echo "    ok: DELETE /admin/mcp/catalog/${PKG_ID}/install/${PKG_UPSTREAM} → 200 且目录无残留安装"

# 9. 非 root：容器内实际 UID 必须是 10001
# 用 `--entrypoint id`（不带绝对路径）而不是 `/usr/bin/id`：Git Bash/MSYS 会把绝对路径
# 当成宿主路径改写成 `C:\...`，传给 docker 后容器里找不到可执行文件。
UID_IN_CONTAINER="$(docker run --rm --entrypoint id "$IMAGE" -u 2>/dev/null || true)"
UID_IN_CONTAINER="${UID_IN_CONTAINER%%$'\r'*}"
[ "$UID_IN_CONTAINER" = "$CONTAINER_UID" ] \
  || fail "容器内 UID 期望 $CONTAINER_UID，实际 '${UID_IN_CONTAINER:-<空>}'"
echo "    ok: 容器内 UID=$UID_IN_CONTAINER"

echo "==> smoke-image 全部通过（$IMAGE）"
