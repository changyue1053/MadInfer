#!/usr/bin/env bash
# 按 tools/models/manifest.toml 拉测试模型到 ~/.cache/madinfer/models/<id>.gguf（madai-w16 I-C-15）。
# 用法：bash tools/models/fetch.sh <id>            例：bash tools/models/fetch.sh smollm2-135m-instruct-q4_k_m
#       MADINFER_MODEL_CACHE=<dir> 改缓存目录（缺省 ~/.cache/madinfer/models）。
# 行为：已在场且 sha256 相符 → 直接 RESULT ok（幂等）；否则 curl 到临时文件、核 sha256 与字节数，相符才落名；
#       不符即删临时文件并 exit 1。只用 bash 3.2 + curl + shasum / sha256sum + awk（无 toml 解析器）。
# 退出码：0 成功；1 校验失败 / 下载失败；2 参数或登记缺失。末行 `RESULT fetch <id> <ok|fail> …` 供日志 grep。
set -euo pipefail

id="${1:-}"
if [[ -z "$id" ]]; then
  echo "用法：bash tools/models/fetch.sh <id>（登记见 tools/models/manifest.toml）" >&2
  exit 2
fi
here="$(cd "$(dirname "$0")" && pwd)"
manifest="$here/manifest.toml"
cache="${MADINFER_MODEL_CACHE:-$HOME/.cache/madinfer/models}"

# 取 [models.<id>] 段内的 key = "value" / key = 数字。
field() {
  awk -v section="[models.$1]" -v key="$2" '
    $0 == section { inside = 1; next }
    /^\[/ { inside = 0 }
    inside && $1 == key && $2 == "=" {
      value = $3
      gsub(/"/, "", value)
      print value
      exit
    }' "$manifest"
}

url="$(field "$id" url)"
sha_expect="$(field "$id" sha256)"
bytes_expect="$(field "$id" bytes)"
if [[ -z "$url" || -z "$sha_expect" || -z "$bytes_expect" ]]; then
  echo "[fetch] manifest.toml 无 [models.$id] 或缺 url / sha256 / bytes" >&2
  echo "RESULT fetch $id fail reason=unregistered"
  exit 2
fi

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

mkdir -p "$cache"
target="$cache/$id.gguf"
if [[ -f "$target" ]]; then
  have="$(sha256_of "$target")"
  if [[ "$have" == "$sha_expect" ]]; then
    echo "RESULT fetch $id ok cached=1 bytes=$(wc -c < "$target" | tr -d '[:space:]') sha256=$have path=$target"
    exit 0
  fi
  echo "[fetch] 缓存文件 sha256 不符（$have），重新拉取" >&2
  rm -f "$target"
fi

tmp="$target.part"
rm -f "$tmp"
echo "[fetch] $url → $tmp" >&2
if ! curl -fSL --retry 3 --connect-timeout 30 -o "$tmp" "$url"; then
  rm -f "$tmp"
  echo "RESULT fetch $id fail reason=download url=$url"
  exit 1
fi
bytes_have="$(wc -c < "$tmp" | tr -d '[:space:]')"
sha_have="$(sha256_of "$tmp")"
if [[ "$sha_have" != "$sha_expect" || "$bytes_have" != "$bytes_expect" ]]; then
  rm -f "$tmp"
  echo "RESULT fetch $id fail reason=checksum sha256=$sha_have expected=$sha_expect bytes=$bytes_have expected_bytes=$bytes_expect"
  exit 1
fi
mv "$tmp" "$target"
echo "RESULT fetch $id ok cached=0 bytes=$bytes_have sha256=$sha_have path=$target"
