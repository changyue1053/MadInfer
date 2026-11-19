#!/usr/bin/env python3
"""First-party synthetic arithmetic fixture. No trained weights or model assets.

Format and tensor contract: vendored llama.cpp 91f6a6cf... (b10883).
Only Python stdlib is used. This does not emulate inference or test language quality.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct

VERSION = 1
ENGINE_COMMIT = '91f6a6cf361385700bbe15981f0f39909df77498'
U32, I32, F32, BOOL, STRING, ARRAY = 4, 5, 6, 7, 8, 9
ALIGNMENT = 32


def string(value):
    data = value.encode('utf-8')
    return struct.pack('<Q', len(data)) + data


def scalar(kind, value):
    if kind == STRING:
        return string(value)
    return struct.pack({U32: '<I', I32: '<i', F32: '<f', BOOL: '<?'}[kind], value)


def encode_value(kind, value):
    if kind != ARRAY:
        return scalar(kind, value)
    element_kind, elements = value
    return struct.pack('<IQ', element_kind, len(elements)) + b''.join(
        scalar(element_kind, element) for element in elements)


def generate(variant):
    if variant not in ('A', 'B'):
        raise ValueError('variant must be A or B')
    tokens = ['<unk>', '<s>', '</s>'] + [f'<0x{b:02X}>' for b in range(256)] + ['A', 'B']
    metadata = [
        ('general.architecture', STRING, 'llama'),
        ('general.name', STRING, 'MadInfer first-party synthetic arithmetic fixture v1'),
        ('general.alignment', U32, ALIGNMENT),
        ('general.file_type', U32, 0),
        ('llama.context_length', U32, 128),
        ('llama.embedding_length', U32, 32),
        ('llama.block_count', U32, 1),
        ('llama.feed_forward_length', U32, 64),
        ('llama.attention.head_count', U32, 1),
        ('llama.attention.head_count_kv', U32, 1),
        ('llama.rope.dimension_count', U32, 32),
        ('llama.attention.layer_norm_rms_epsilon', F32, 1e-5),
        ('tokenizer.ggml.model', STRING, 'llama'),
        ('tokenizer.ggml.tokens', ARRAY, (STRING, tokens)),
        ('tokenizer.ggml.scores', ARRAY, (F32, [0.0] * len(tokens))),
        ('tokenizer.ggml.token_type', ARRAY, (I32, [2, 3, 3] + [6] * 256 + [1, 1])),
        ('tokenizer.ggml.bos_token_id', U32, 1),
        ('tokenizer.ggml.eos_token_id', U32, 2),
        ('tokenizer.ggml.unknown_token_id', U32, 0),
        ('tokenizer.ggml.add_bos_token', BOOL, True),
        ('tokenizer.ggml.add_eos_token', BOOL, False),
        ('tokenizer.ggml.add_space_prefix', BOOL, False),
    ]
    # ne[0] is contiguous. Each vocabulary output row contains 32 F32 values.
    output = [0.0] * (32 * 261)
    selected = 259 if variant == 'A' else 260
    output[selected * 32:(selected + 1) * 32] = [1.0] * 32
    tensors = [
        ('token_embd.weight', (32, 261), [1.0] * (32 * 261)),
        ('output_norm.weight', (32,), [1.0] * 32),
        ('output.weight', (32, 261), output),
        ('blk.0.attn_norm.weight', (32,), [1.0] * 32),
        *[(f'blk.0.attn_{name}.weight', (32, 32), [0.0] * 1024)
          for name in ('q', 'k', 'v', 'output')],
        ('blk.0.ffn_norm.weight', (32,), [1.0] * 32),
        ('blk.0.ffn_gate.weight', (32, 64), [0.0] * 2048),
        ('blk.0.ffn_up.weight', (32, 64), [0.0] * 2048),
        ('blk.0.ffn_down.weight', (64, 32), [0.0] * 2048),
    ]
    header = bytearray(b'GGUF' + struct.pack('<IQQ', 3, len(tensors), len(metadata)))
    for key, kind, value in metadata:
        header += string(key) + struct.pack('<I', kind) + encode_value(kind, value)
    data = bytearray()
    for name, shape, values in tensors:
        data += b'\0' * (-len(data) % ALIGNMENT)
        header += string(name) + struct.pack('<I', len(shape))
        header += struct.pack('<' + 'Q' * len(shape), *shape)
        header += struct.pack('<IQ', 0, len(data))  # GGML_TYPE_F32
        data += struct.pack('<' + 'f' * len(values), *values)
    header += b'\0' * (-len(header) % ALIGNMENT)
    return bytes(header + data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-dir', required=True, type=Path)
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    manifest = {'generator_version': VERSION, 'engine_commit': ENGINE_COMMIT,
                'scope': 'synthetic CPU decode qualification; not inference quality', 'files': []}
    for variant in ('A', 'B'):
        data = generate(variant)
        name = f'synthetic-{variant}.gguf'
        (args.output_dir / name).write_bytes(data)
        manifest['files'].append({'name': name, 'bytes': len(data),
                                  'sha256': hashlib.sha256(data).hexdigest()})
    (args.output_dir / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(json.dumps(manifest, sort_keys=True))


if __name__ == '__main__':
    main()
