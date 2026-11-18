# Changelog

## 0.1.0 source preview
- Imported independent IMInfer baseline with all sixteen crates and existing tests.
- Renamed standalone Cargo/UniFFI/C namespaces to MadInfer.
- Added C explicit backend selection/query; native never silently falls back.
- Made missing native test model fail; fixed Linux/macOS model size checking.
- Preserved proprietary declaration and third-party notices; model weights excluded.
- Platform packages, bounded delivery queue, safe generation handles and MadObs adapter remain future gates.
