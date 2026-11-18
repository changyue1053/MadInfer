# MadInfer 协议 schema（v1）

事实源。mad-repo 的 `schema/infer/` 是本目录的逐字节镜像，由 mad 侧门禁 `check_infer_schema_sync.py` 判红（《MadAI 产品接入计划》§8.1、决策 5）。
设计依据：《端智能通用 SDK 架构设计（Rust 核心）》§5 P1～P9。

约定（全部文档共用）：JSON、UTF-8、字段 snake_case、顶层 `schema: 1`；时间 RFC 3339 UTC；字节数为整数；比例用万分比整数（`_bp`）或千分比（`permille`）。
读者忽略未知字段（R5），因此除 telemetry / device 两份"白名单型"文档外不写 `additionalProperties: false`。

| 文件 | 设计文档 | 方向 | 说明 |
|---|---|---|---|
| `request.v1.json` | P1 | 宿主 → 核 | infer_stream / infer_run / infer_submit 入参；隐私标签必填 |
| `event.v1.json` | P1 | 核 → 宿主 | started / delta / complete / failed 四型；顺序保证写在 description |
| `capabilities.v1.json` | P2 | 核 → 宿主 | 能力快照，整份推送 |
| `manifest.v1.json` | P3 | 云 → 核 | 算法包清单，唯一被签名对象 |
| `config.v1.json` | P4 | 云 → 核 | 映射表 / 阈值 / 开关 / 密钥轮换；`when` 禁含 privacy |
| `telemetry.v1.json` | P5 | 核 → 宿主 → 云 | 事件类型闭合、字段白名单（additionalProperties = false） |
| `cloud.v1.json` | P6 | 核 ↔ 云 | POST /v1/generate 请求体与 SSE 事件载荷 |
| `eval.v1.json` | P7 | 工具链 | 评估用例行与 evalrun 报告 |
| `device.v1.json` | P8 | 核 → 云 | 设备画像，白名单型，无标识符 |
| `signal.v1.json` | P9 | 宿主 → 核 | 资源信号六型 + 主链路忙 |
| `init.v1.json` | P9 | 宿主 → 核 | infer_create 入参 |

P9 的 `download.v1` / `http.v1` 是 FFI 本地结构体（UniFFI record），不以 JSON 传输，故无 schema 文件；字段定义随 I3 的 `madinfer-ffi` UDL 落地。

## 演进纪律

- 只加不改：加可选字段不升版；改必填、改枚举含义、删字段升 `schema: 2` 并新建 `*.v2.json`，v1 文件保留。
- 枚举只准在尾部追加；Rust 侧 `#[repr(u8)]` 判别值与 KMP 侧 `enum` ordinal 同步（`madinfer-core-types` 单测锁 `wire()` 字符串）。
- 改任何一份 schema 的同一 PR 必须同步 mad-repo `schema/infer/`（决策 5：同一批合入，两仓门禁双向判红）。

## 校验

```
python3 tools/check_schemas.py          # 每份文件：合法 JSON、$schema / $id / title 齐、$id 与文件名一致、schema 常量 = 1
```
