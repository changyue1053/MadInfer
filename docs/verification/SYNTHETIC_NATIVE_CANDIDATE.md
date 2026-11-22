# Synthetic native CPU candidate: NOT YET runtime-qualified

Base public source: 519c113512a4a277829665db5ddfde1b7fac1038.
Engine: ggml-org/llama.cpp 91f6a6cf361385700bbe15981f0f39909df77498, b10883.
No private repositories used. No changes to the engine, production API or iOS.

## Provenance and purpose

A new stdlib-only Python generator produces deterministic untrained numeric
matrices and a mechanically enumerated byte-fallback vocabulary. No upstream
fixture, pretrained weights, tokenizer file, downloaded model or training corpus
is used. Generated GGUF files stay under ignored target directories and are never
committed, uploaded as artifacts or redistributed by this phase. Source code and
hashes are reviewable. This qualifies synthetic arithmetic through real native
CPU decode, not language understanding, model accuracy, model safety, performance,
or compatibility with arbitrary pretrained models.

## Exact source contract

All links below refer to the exact upstream pin, not its moving default branch:

- [GGUF layout/version/alignment](https://github.com/ggml-org/llama.cpp/blob/91f6a6cf361385700bbe15981f0f39909df77498/ggml/include/gguf.h#L1-L46): v3, little-endian fields, 32-byte aligned F32 tensor data.
- [Llama tensors](https://github.com/ggml-org/llama.cpp/blob/91f6a6cf361385700bbe15981f0f39909df77498/src/models/llama.cpp#L35-L94): one non-MoE block, explicit output tensor (no tied fallback), 12 tensors.
- [Common hparams](https://github.com/ggml-org/llama.cpp/blob/91f6a6cf361385700bbe15981f0f39909df77498/src/llama-model.cpp#L1225-L1356): train context128, embedding32, head1/KV1, rotary32, FF64, RMS epsilon1e-5. One layer is valid; UNKNOWN model-size classification is not an unsupported architecture.
- [SPM metadata](https://github.com/ggml-org/llama.cpp/blob/91f6a6cf361385700bbe15981f0f39909df77498/src/llama-vocab.cpp#L1933-L1974): model `llama`, 261 tokens: unk0/bos1/eos2, 256 byte tokens at3..258, A259/B260. Explicit BOS yes/EOS no/prefix-space no; scores0; types2,3,3,256x6,1,1. No learned tokenizer assets.
- [Actual context alignment](https://github.com/ggml-org/llama.cpp/blob/91f6a6cf361385700bbe15981f0f39909df77498/src/llama-context.cpp#L287-L299): requested128 rounds to256. Tests distinguish actual native context256 from train-context metadata128; they do not assume equality.

Tensor shapes are ggml ne[] order: embeddings/output [32,261], three norms [32],
Q/K/V/attention-output [32,32], FF gate/up [32,64], FF down [64,32]. Total27040
F32 parameters /108160 tensor bytes. Each serialized file is115584 bytes.
Embeddings and norms are1, attention/FFN matrices0. Only the selected token's
output row is1. The real residual/RMS/matmul graph predicts a unique A (or B)
argmax; actual native execution, not the mathematical prediction, decides CI pass.

Fixture v1 SHA256:
- A: `23696b2e27a65419bc4b81ddb49014063c09af369ea542188bb98b70c15ff9c2`
- B: `1cfe72cce2f88fdae105088ad24574ff47421e1b8e9bdbcd569d7e8347cb3703`

## Gate and scope

`python3 -m unittest discover -s tests/synthetic -v` independently parses the
serialized layout, tensor counts/shapes/data, tokenizer, hashes, corruption, and
checks that A/B differ only in output tensor. These tests do NOT load llama.cpp.

`ci/synthetic-native.sh` requires a clean exact40-hex reviewed HEAD, Linux x86_64,
pinned vendored source-tree SHA256, llama-sys and native build10883. It uses the
existing source build.rs/C bridge, not foreign prebuilt libraries. It enumerates
all five required tests and then runs the entire isolated integration binary,
serially, with no filter, skip fallback, external model or existing pretrained gate.

Native tests must load both fixtures, assert metadata/counts/KV sizing and exact
prompt[1,259], generate[259,259,259]/AAA versus[260,260,260]/BBB twice each, allocate
an actual context256, perform prefill plus three incremental decodes and clear/
reuse, and reject missing/bad-magic/truncated inputs. The integration test compiles
the unmodified existing private sys.rs RAII bridge for context introspection;
there is no added production API and no copied/emulated native implementation.
Its existing thread-budget unit test is the fifth explicitly enumerated test.

This historical checkpoint ran five native Linux tests successfully at the
[pinned public input](https://github.com/changyue1053/MadInfer/actions/runs/44493346592).
The main source bundle does not ship that workflow. On a prepared Linux host
`ci/synthetic-native.sh` revalidates the actual commit and builds with
`--locked --offline`; see [qualification](QUALIFIED_SOURCE.md). No semantic
inference, throughput, iOS, or production readiness follows from this result.
