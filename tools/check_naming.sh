#!/usr/bin/env bash
# MadInfer 命名门：复用 mad 仓的禁词扫描器（词表全栈同名，mad-naming-rules §1；机器可读副本 gates/banned_words.txt）。
# 用法：bash tools/check_naming.sh [mad 仓路径]
#   缺省依次探测 ../mad-repo（平铺形态：MadInfer 与 mad-repo 同级 checkout）→ ../mad（容器形态：madai-w<N> 容器目录里
#   MadInfer 冻结树与 mad 工作树同级，lane-rules 1；madai-w12 I-C-11，R-w11-7）——取第一处带 gates/scan_banned_words.py 的。
# 扫描面：crates/ README.md schemas/README.md（.rs / .md）；放行清单沿用 mad 的 banned_words_allow.txt（表驱动、LogLevel::Info 等）。
# 退出码：同扫描器（0 零命中；1 有命中；2 参数错）。两处都不在时以 3 退出并提示——CI 里由 sibling checkout 保证在位。
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
mad="${1:-}"
if [[ -z "$mad" ]]; then
  for candidate in "$here/../mad-repo" "$here/../mad"; do
    if [[ -f "$candidate/gates/scan_banned_words.py" ]]; then
      mad="$candidate"
      break
    fi
  done
  if [[ -z "$mad" ]]; then
    echo "[naming] 找不到 mad 仓扫描器：已探测 $here/../mad-repo 与 $here/../mad（传入 mad 仓路径或在同级目录 checkout）" >&2
    exit 3
  fi
fi
scanner="$mad/gates/scan_banned_words.py"
allow="$mad/gates/banned_words_allow.txt"
if [[ ! -f "$scanner" ]]; then
  echo "[naming] 找不到 mad 仓扫描器：$scanner（传入 mad 仓路径或在同级目录 checkout）" >&2
  exit 3
fi
cd "$here"
# 扫描面 = 手写源；生成物（uniffi 绑定落盘目录 crates/*/bindings，gitignore 内、不入库）不在词表管辖——
# 以文件清单方式喂扫描器，排除 bindings/ 与 target/（madai-w06 总控接替 I-CD-01：绑定生成物曾被扫到 29 处）。
# bash 3.2（macOS 自带）兼容：不用 mapfile；仓内路径无空白，词分割安全。
files="$(find crates -type f \( -name '*.rs' -o -name '*.md' -o -name '*.kt' \) -not -path '*/bindings/*' -not -path '*/target/*' | sort)"
# shellcheck disable=SC2086
python3 "$scanner" $files README.md schemas/README.md --allow-file "$allow"
