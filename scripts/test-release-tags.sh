#!/usr/bin/env bash
# release-tags.sh 的离线用例：三段/四段版本、dev 快照、main、非法格式、镜像前缀与 GITHUB_OUTPUT。
# 只依赖 bash，不需要 docker、网络或 CI 环境；CI 的 scripts job 与本地 test-all 都跑它。
#
# 用法：
#   bash scripts/test-release-tags.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GEN="$ROOT/scripts/release-tags.sh"
SHA=0123456789abcdef
total=0
failed=0

run() { # ref_type ref_name [额外参数...]
  local ref_type="$1" ref_name="$2"
  shift 2
  GITHUB_REF_TYPE="$ref_type" GITHUB_REF_NAME="$ref_name" GITHUB_SHA="$SHA" \
    bash "$GEN" "$@"
}

expect_ok() { # 名称 期望 version 期望 tags ref_type ref_name
  local name="$1" want_version="$2" want_tags="$3" ref_type="$4" ref_name="$5"
  local out version tags
  total=$((total + 1))
  if ! out="$(run "$ref_type" "$ref_name" 2>&1)"; then
    echo "FAIL ${name}：脚本退出非 0：${out}" >&2
    failed=$((failed + 1))
    return 0
  fi
  version="$(printf '%s\n' "$out" | sed -n 's/^version=//p')"
  tags="$(printf '%s\n' "$out" | sed -n 's/^tags=//p')"
  if [ "$version" = "$want_version" ] && [ "$tags" = "$want_tags" ]; then
    echo "ok   ${name}：${ref_name} → ${version} | ${tags}"
  else
    echo "FAIL ${name}：version='${version}'（期望 '${want_version}'）tags='${tags}'（期望 '${want_tags}'）" >&2
    failed=$((failed + 1))
  fi
}

expect_fail() { # 名称 ref_type ref_name
  local name="$1" ref_type="$2" ref_name="$3" out
  total=$((total + 1))
  if out="$(run "$ref_type" "$ref_name" 2>&1)"; then
    echo "FAIL ${name}：应失败却成功，输出：${out}" >&2
    failed=$((failed + 1))
  else
    echo "ok   ${name}：按预期拒绝（${ref_name}）"
  fi
}

expect_ok "dev 快照（不带 latest）" dev "dev,sha-0123456" branch dev
expect_ok "main push" main "latest,main,sha-0123456" branch main
expect_ok "四段版本 v0.0.1.000" 0.0.1.000 \
  "0.0.1.000,0.0.1,0.0,sha-0123456" tag v0.0.1.000
expect_ok "三段版本 v1.2.3" 1.2.3 "1.2.3,1.2,sha-0123456" tag v1.2.3
expect_ok "四段任意值 v1.2.3.4" 1.2.3.4 \
  "1.2.3.4,1.2.3,1.2,sha-0123456" tag v1.2.3.4

# 带镜像前缀时每个 tag 都要拼上完整仓库名
total=$((total + 1))
prefixed="$(run tag v0.0.1.000 --image ghcr.io/nnyjk/jumcp | sed -n 's/^tags=//p')"
want_prefixed="ghcr.io/nnyjk/jumcp:0.0.1.000,ghcr.io/nnyjk/jumcp:0.0.1,ghcr.io/nnyjk/jumcp:0.0,ghcr.io/nnyjk/jumcp:sha-0123456"
if [ "$prefixed" = "$want_prefixed" ]; then
  echo "ok   --image 前缀：${prefixed}"
else
  echo "FAIL --image 前缀：'${prefixed}'（期望 '${want_prefixed}'）" >&2
  failed=$((failed + 1))
fi

# dev 快照同样要拼前缀，并且不带 latest
total=$((total + 1))
prefixed_dev="$(run branch dev --image ghcr.io/nnyjk/jumcp | sed -n 's/^tags=//p')"
want_prefixed_dev="ghcr.io/nnyjk/jumcp:dev,ghcr.io/nnyjk/jumcp:sha-0123456"
if [ "$prefixed_dev" = "$want_prefixed_dev" ]; then
  echo "ok   --image 前缀（dev）：${prefixed_dev}"
else
  echo "FAIL --image 前缀（dev）：'${prefixed_dev}'（期望 '${want_prefixed_dev}'）" >&2
  failed=$((failed + 1))
fi

# --github-output 写 $GITHUB_OUTPUT，不污染 stdout
total=$((total + 1))
gh_out="$(mktemp)"
out="$(GITHUB_OUTPUT="$gh_out" bash "$GEN" --ref-type tag --ref-name v0.0.1.000 \
  --sha "$SHA" --github-output)"
if [ -z "$out" ] \
  && grep -qx 'version=0.0.1.000' "$gh_out" \
  && grep -qx 'tags=0.0.1.000,0.0.1,0.0,sha-0123456' "$gh_out"; then
  echo "ok   --github-output：写入 GITHUB_OUTPUT 且 stdout 为空"
else
  echo "FAIL --github-output：stdout='${out}'，文件内容：$(printf '%s' "$(cat "$gh_out")")" >&2
  failed=$((failed + 1))
fi
rm -f "$gh_out"

expect_fail "非法 tag v1.2" tag v1.2
expect_fail "标签缺 v 前缀 1.2.3" tag 1.2.3
expect_fail "非法 tag v1.2.3.4.5" tag v1.2.3.4.5
expect_fail "非数字段 v1.2.x" tag v1.2.x
expect_fail "feature 分支不发布" branch feature/split-cd
expect_fail "其它 ref 类型不发布" pull_request 42

echo "----"
if [ "$failed" -ne 0 ]; then
  echo "release-tags 用例失败：${failed}/${total}" >&2
  exit 1
fi
echo "release-tags 用例全部通过：${total}/${total}"
