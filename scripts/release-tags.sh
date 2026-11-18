#!/usr/bin/env bash
# 发布镜像产物（GHCR 镜像、无头 Web 包）的版本与 tag 计算：CD 流水线与本地共用同一份规则。
#
# 用法：
#   scripts/release-tags.sh [--ref-type branch|tag] [--ref-name <ref>] [--sha <sha>]
#                           [--image <registry/repo>] [--github-output]
#
# 输入缺省取 GITHUB_REF_TYPE / GITHUB_REF_NAME / GITHUB_SHA；本地模拟直接给参数。
# 输出：stdout 两行 `version=…` 与 `tags=…`（逗号分隔）；--github-output 改写 $GITHUB_OUTPUT。
# --image 给定时每个 tag 拼成 `<image>:<tag>`，否则只打印裸 tag。
#
# 规则：
#   dev 分支  → version=dev，tags=dev,sha-<7 位>（快照，不带 latest）
#   main 分支 → version=main，tags=latest,main,sha-<7 位>（latest 只在这里产出）
#   版本 tag  → version=<去掉 v 的 tag>，
#               tags=<version>,<major>.<minor>.<patch>（仅四段版本）,<major>.<minor>,sha-<7 位>
#   其它 ref  → 非 0 退出：发布只发生在 dev/main 与版本 tag 上
#   tag 必须是 v + 3 或 4 段数字（v1.2.3 / v0.0.1.000），其它格式非 0 退出

set -euo pipefail

ref_type="${GITHUB_REF_TYPE:-}"
ref_name="${GITHUB_REF_NAME:-}"
sha="${GITHUB_SHA:-}"
image=""
to_github_output=0

usage() { sed -n '2,18p' "$0" >&2; }
die() { echo "release-tags: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --ref-type) ref_type="${2:?--ref-type 需要值}"; shift 2 ;;
    --ref-name) ref_name="${2:?--ref-name 需要值}"; shift 2 ;;
    --sha) sha="${2:?--sha 需要值}"; shift 2 ;;
    --image) image="${2:?--image 需要值}"; shift 2 ;;
    --github-output) to_github_output=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" ;;
  esac
done

[ -n "$ref_type" ] || die "缺少 ref 类型（--ref-type 或 GITHUB_REF_TYPE）"
[ -n "$ref_name" ] || die "缺少 ref 名（--ref-name 或 GITHUB_REF_NAME）"
[ -n "$sha" ] || die "缺少提交 sha（--sha 或 GITHUB_SHA）"

short_sha="${sha:0:7}"

case "$ref_type" in
  branch)
    case "$ref_name" in
      dev)
        # 集成分支快照：只给可追溯的 dev / sha-<7>，不产出 latest（latest 只属于 main）。
        version="dev"
        tags=(dev "sha-${short_sha}")
        ;;
      main)
        version="main"
        tags=(latest main "sha-${short_sha}")
        ;;
      *)
        die "分支 '$ref_name' 不发布：只有 dev（快照）、main（发布）与版本 tag 会产出镜像"
        ;;
    esac
    ;;
  tag)
    case "$ref_name" in
      v*) ;;
      *) die "tag '$ref_name' 不是版本 tag：必须形如 v1.2.3 或 v0.0.1.000" ;;
    esac
    if ! [[ "${ref_name#v}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
      die "tag '$ref_name' 格式非法：必须是 v + 3 或 4 段数字（如 v1.2.3、v0.0.1.000）"
    fi
    version="${ref_name#v}"
    IFS=. read -r major minor patch rest <<<"$version"
    tags=("$version")
    # 四段版本（0.0.1.000）额外产出三段兼容 tag，让只认 semver 的消费方也能 pin。
    [ -z "${rest:-}" ] || tags+=("${major}.${minor}.${patch}")
    tags+=("${major}.${minor}" "sha-${short_sha}")
    ;;
  *)
    die "ref 类型 '$ref_type' 不发布：只支持 dev/main 分支与 v* 版本 tag"
    ;;
esac

joined="$(printf '%s,' "${tags[@]}")"
joined="${joined%,}"

if [ -n "$image" ]; then
  qualified=()
  for t in "${tags[@]}"; do qualified+=("${image}:${t}"); done
  out_tags="$(printf '%s,' "${qualified[@]}")"
  out_tags="${out_tags%,}"
else
  out_tags="$joined"
fi

if [ "$to_github_output" -eq 1 ]; then
  [ -n "${GITHUB_OUTPUT:-}" ] || die "--github-output 需要 GITHUB_OUTPUT"
  {
    echo "version=${version}"
    echo "tags=${out_tags}"
  } >>"$GITHUB_OUTPUT"
else
  echo "version=${version}"
  echo "tags=${out_tags}"
fi
