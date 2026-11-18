# Support matrix (development preview)

| Target | Build/test status | Artifact/support claim |
|---|---|---|
| Linux x86_64, Rust 1.100.0 | Default/emulation tests executed; native evidence in STAGE1.md | Development harness only |
| Android arm64 | Build script inherited, NOT run here | Unverified; no AAR/device support claim |
| iOS arm64 / simulator | Build scripts inherited, NOT run here | Unverified; no XCFramework/device support claim |
| HarmonyOS arm64 | Build script inherited, NOT run here | Unverified; no HAR/NAPI/device support claim |
| MadObs adapter | Not implemented | Host telemetry/no-op works without MadObs |

Native backend CPU only. Metal/Vulkan and three-platform combinations are not validated. Minimum SDK versions in inherited scripts are configuration, not evidence of compatibility. Source-stage publication is not a GA release.
