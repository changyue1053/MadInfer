"""Independent structural reader. These tests are NOT native decode evidence."""
import hashlib
import importlib.util
import math
from pathlib import Path
import struct
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('generator', ROOT / 'scripts/generate-synthetic-gguf.py')
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class Reader:
    def __init__(self, data):
        self.data, self.pos = data, 0

    def number(self, fmt):
        size = struct.calcsize('<' + fmt)
        if self.pos + size > len(self.data):
            raise ValueError('truncated')
        value = struct.unpack_from('<' + fmt, self.data, self.pos)[0]
        self.pos += size
        return value

    def string(self):
        size = self.number('Q')
        if self.pos + size > len(self.data):
            raise ValueError('truncated string')
        value = self.data[self.pos:self.pos + size].decode('utf-8')
        self.pos += size
        return value

    def value(self, kind):
        if kind == 8:
            return self.string()
        if kind == 9:
            element, count = self.number('I'), self.number('Q')
            return [self.value(element) for _ in range(count)]
        return self.number({4: 'I', 5: 'i', 6: 'f', 7: '?'}[kind])


def inspect(data):
    r = Reader(data)
    if r.number('I') != 0x46554747 or r.number('I') != 3:
        raise ValueError('not GGUF v3')
    count, kv_count = r.number('Q'), r.number('Q')
    kv = {}
    for _ in range(kv_count):
        key = r.string()
        if key in kv:
            raise ValueError('duplicate key')
        kv[key] = r.value(r.number('I'))
    descriptions = []
    for _ in range(count):
        name = r.string()
        dims = r.number('I')
        shape = tuple(r.number('Q') for _ in range(dims))
        kind, offset = r.number('I'), r.number('Q')
        descriptions.append((name, shape, kind, offset))
    base = (r.pos + 31) // 32 * 32
    tensors, end = {}, base
    for name, shape, kind, offset in descriptions:
        if kind != 0 or offset % 32 or name in tensors or base + offset != end:
            raise ValueError('invalid F32 tensor layout')
        size = math.prod(shape) * 4
        if base + offset + size > len(data):
            raise ValueError('truncated tensor')
        values = struct.unpack_from('<' + 'f' * math.prod(shape), data, base + offset)
        tensors[name] = (shape, values)
        end = base + offset + size
    if end != len(data):
        raise ValueError('trailing data')
    return kv, tensors


class GeneratorTests(unittest.TestCase):
    def test_deterministic_pins(self):
        expected = EXPECTED_HASHES
        for variant in ('A', 'B'):
            data = generator.generate(variant)
            self.assertEqual(data, generator.generate(variant))
            self.assertEqual(hashlib.sha256(data).hexdigest(), expected[variant])

    def test_metadata_and_tokenizer(self):
        kv, tensors = inspect(generator.generate('A'))
        for key, expected in {'general.architecture': 'llama', 'llama.context_length': 128,
                              'llama.embedding_length': 32, 'llama.block_count': 1,
                              'llama.feed_forward_length': 64, 'llama.attention.head_count': 1,
                              'llama.attention.head_count_kv': 1,
                              'llama.rope.dimension_count': 32}.items():
            self.assertEqual(kv[key], expected)
        self.assertEqual(kv['tokenizer.ggml.tokens'], ['<unk>', '<s>', '</s>'] +
                         [f'<0x{b:02X}>' for b in range(256)] + ['A', 'B'])
        self.assertEqual(kv['tokenizer.ggml.token_type'], [2, 3, 3] + [6] * 256 + [1, 1])
        self.assertEqual(kv['tokenizer.ggml.scores'], [0.0] * 261)
        self.assertEqual(kv['tokenizer.ggml.model'], 'llama')
        self.assertEqual([kv['tokenizer.ggml.' + s + '_token_id'] for s in ('bos', 'eos', 'unknown')], [1, 2, 0])
        self.assertTrue(kv['tokenizer.ggml.add_bos_token'])
        self.assertFalse(kv['tokenizer.ggml.add_eos_token'])
        self.assertFalse(kv['tokenizer.ggml.add_space_prefix'])
        self.assertAlmostEqual(kv['llama.attention.layer_norm_rms_epsilon'], 1e-5, places=10)
        self.assertEqual(len(tensors), 12)

    def test_tensor_shapes_and_values(self):
        expected_shapes = {'token_embd.weight': (32, 261), 'output.weight': (32, 261),
                           'output_norm.weight': (32,), 'blk.0.attn_norm.weight': (32,),
                           'blk.0.ffn_norm.weight': (32,), 'blk.0.ffn_gate.weight': (32, 64),
                           'blk.0.ffn_up.weight': (32, 64), 'blk.0.ffn_down.weight': (64, 32)}
        expected_shapes.update({f'blk.0.attn_{n}.weight': (32, 32) for n in ('q', 'k', 'v', 'output')})
        for variant, selected in [('A', 259), ('B', 260)]:
            _, tensors = inspect(generator.generate(variant))
            self.assertEqual({k: v[0] for k, v in tensors.items()}, expected_shapes)
            self.assertEqual(sum(len(v[1]) for v in tensors.values()), 27040)
            for name, (_, values) in tensors.items():
                if name == 'output.weight':
                    self.assertEqual(values, tuple(1.0 if selected * 32 <= i < (selected + 1) * 32 else 0.0
                                                   for i in range(32 * 261)))
                else:
                    expected = 1.0 if name == 'token_embd.weight' or 'norm.weight' in name else 0.0
                    self.assertTrue(all(v == expected for v in values))

    def test_variants_only_change_output_tensor(self):
        ka, ta = inspect(generator.generate('A'))
        kb, tb = inspect(generator.generate('B'))
        self.assertEqual(ka, kb)
        self.assertNotEqual(ta.pop('output.weight'), tb.pop('output.weight'))
        self.assertEqual(ta, tb)

    def test_hard_errors(self):
        with self.assertRaises(ValueError):
            generator.generate('other')
        data = generator.generate('A')
        for bad in (b'', b'BAD!' + data[4:], data[:-1], data + b'x'):
            with self.assertRaises(ValueError):
                inspect(bad)


EXPECTED_HASHES = {'A': '23696b2e27a65419bc4b81ddb49014063c09af369ea542188bb98b70c15ff9c2', 'B': '1cfe72cce2f88fdae105088ad24574ff47421e1b8e9bdbcd569d7e8347cb3703'}  # Filled once when fixture version is reviewed; not regenerated by tests.
if __name__ == '__main__':
    unittest.main()
