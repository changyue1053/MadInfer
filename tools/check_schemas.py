#!/usr/bin/env python3
"""MadInfer schema 形式校验（零依赖；CI schema-check job 与 mad 侧 check_infer_schema_sync.py 的前置）。

每份 schemas/*.v1.json 检查：
  S1  合法 JSON（UTF-8）。
  S2  顶层含 $schema（draft 2020-12）、$id、title、description。
  S3  $id 以文件名结尾（改名必须同步 $id）。
  S4  文档某处出现 "schema": {"const": 1}（顶层或 oneOf 分支 / $defs 内），保证 schema 版本字段落在协议里。
  S5  白名单型文档（telemetry、device）顶层 additionalProperties 必须为 false。

用法：python3 tools/check_schemas.py [schemas 目录，默认脚本上级的 schemas/]
退出码：0 全绿；1 有违规（逐条打印）。
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

WHITELIST_DOCS = {"telemetry.v1.json", "device.v1.json"}


def has_schema_const(node) -> bool:
    if isinstance(node, dict):
        props = node.get("properties")
        if isinstance(props, dict) and props.get("schema") == {"const": 1}:
            return True
        return any(has_schema_const(v) for v in node.values())
    if isinstance(node, list):
        return any(has_schema_const(v) for v in node)
    return False


def main(argv: list[str]) -> int:
    root = Path(argv[1]) if len(argv) > 1 else Path(__file__).resolve().parent.parent / "schemas"
    files = sorted(root.glob("*.v*.json"))
    if not files:
        print(f"[schemas] 目录无 schema 文件：{root}")
        return 1
    problems: list[str] = []
    for path in files:
        try:
            doc = json.loads(path.read_text(encoding="utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            problems.append(f"{path.name}: S1 非法 JSON：{exc}")
            continue
        for key in ("$schema", "$id", "title", "description"):
            if key not in doc:
                problems.append(f"{path.name}: S2 缺 {key}")
        if "2020-12" not in str(doc.get("$schema", "")):
            problems.append(f"{path.name}: S2 $schema 不是 draft 2020-12")
        if not str(doc.get("$id", "")).endswith(path.name):
            problems.append(f"{path.name}: S3 $id 与文件名不一致：{doc.get('$id')}")
        if not has_schema_const(doc):
            problems.append(f"{path.name}: S4 未见 schema 常量 1")
        if path.name in WHITELIST_DOCS and doc.get("additionalProperties") is not False:
            problems.append(f"{path.name}: S5 白名单型文档顶层 additionalProperties 必须为 false")
    for line in problems:
        print(line)
    print(f"[schemas] 扫描 {len(files)} 份，违规 {len(problems)} 条")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
