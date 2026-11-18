# Third-party notices and source-publication inventory

First-party imported code remains Proprietary; public visibility adds no license grant.

## Vendored native engine

`third_party/llama.cpp` contains the exact 373-file source subset from the selected IMInfer baseline, independently checked against official ggml-org/llama.cpp commit `91f6a6cf361385700bbe15981f0f39909df77498` (b10883). No engine source modifications. The commit archive differs in bytes from the historical tag archive, but **every retained Git blob matched** the locked source tree. Historical tag archive metadata is retained in PINS.toml; actual fetched archive hash is in MIGRATION_SOURCE.toml.

- llama.cpp/ggml: MIT, retained `third_party/llama.cpp/LICENSE`.
- rotate-bits: MIT, retained `third_party/llama.cpp/vendor/hash/rotate-bits/LICENSE.md`.
- xxHash: BSD-2-Clause, retained `third_party/llama.cpp/vendor/hash/xxhash/LICENSE`.
- SHA-256: Igor Pavlov public-domain notice retained under `vendor/hash/sha256/LICENSE`.

## Rust dependencies

`Cargo.lock` is the exact dependency resolution. `docs/verification/dependency-licenses.json` lists package/version/license/source metadata for the resolved graph. Original available license/notice files are copied under `third_party/licenses/<package>-<version>/`. Registry source archives are available at `https://crates.io/api/v1/crates/<package>/<version>/download`; lockfile checksums identify them. No third-party registry source modifications are distributed.

UniFFI-related MPL-2.0 components require keeping their notices and source availability obligations when distributing binaries; future binary releases must include this notice, license texts, exact source archive references and build/feature manifest. Dual-licensed dependencies may use their permissive MIT/Apache option; LGPL is not selected. Dependency metadata is not a completed vulnerability or legal audit.

## Models

No weights are committed or bundled. The opt-in test downloader identifies SmolLM2-135M-Instruct Q4_K_M by exact SHA256 and size; its historical manifest states Apache-2.0. Model redistribution is not part of this source stage. Before any redistributed model/binary release, independently recheck the model-card/license and provenance, complete SBOM/security review, and record distribution obligations.

Registry archive exceptions: r-efi embeds its full MIT grant and copyrights in AUTHORS (retained); UniFFI 0.29.4 crates reference MPL-2.0 but omit a root LICENSE in their crate archives, so the exact `mozilla/uniffi-rs/v0.29.4/LICENSE` text was retrieved from the official upstream and retained for each component.
