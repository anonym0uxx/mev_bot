# Base58 encoder correction — isolated source/test receipt

## Scope and finding

- Worktree: `D:/repos/mev_bot-north-star`, HEAD at start `dac3e1e1a1f56e925a0ea312cf1e31fb4a0cdc4d`.
- Only this receipt and `tools/stream-capture-rs/grpc-server-only/src/encoding.rs` are edited.
- Root cause: `digits = vec![0]` retains a significant zero digit for an empty or entirely zero input, after leading zero bytes have already emitted their `1` characters. Empty input returned `1`; a 32-byte zero public key returned 33 ones.
- Fix: initialize significant digits with `Vec::new()`. Carry propagation creates digits only when a nonzero value is encountered. Leading zero bytes remain preserved exactly once. No decoder, record normalization, capture data, quarantine, or historical identity is changed.
- This is source-level Base58 correctness evidence, **not** a capture build, deployment, integration certification, or replay repair. The original live worktree is read-only for hash checks; no live rebuild/restart/deployment was performed.

## Actual strict Rust RED → GREEN

All seven regression tests were added before changing the implementation. Existing nonzero behavior passed at RED; empty/all-zero behavior failed for the expected output mismatch, not compilation errors.

Compiler: WSL Ubuntu `/home/alon/.cargo/bin/rustc`, `rustc 1.97.1 (8bab26f4f 2026-07-14)`.

The standalone harness extracts the exact source substring from `pub fn b58_encode` to immediately before `/// Standard-alphabet base64 encoder.`, normalizing only CRLF to LF. This includes the actual production encoder and its entire `#[cfg(test)]` test module. It excludes `sha2`, SHA-256, Base64, hex, clock helpers, and the capture application. No dependency downloads, package installations, Cargo/full build, or capture executable were needed.

Actual compiler commands:

```text
rustc --edition=2021 -D warnings --test /tmp/north-star-b58-sny8s7w1/encoding_b58.rs -o /tmp/north-star-b58-sny8s7w1/red_tests
/tmp/north-star-b58-sny8s7w1/red_tests --nocapture
rustc --edition=2021 -D warnings --test /tmp/north-star-b58-sny8s7w1/encoding_b58_green.rs -o /tmp/north-star-b58-sny8s7w1/green_tests
/tmp/north-star-b58-sny8s7w1/green_tests --nocapture
```

| Test | RED | GREEN |
| --- | --- | --- |
| `b58_empty_is_empty` | FAIL: `1` versus empty | PASS |
| `b58_one_zero_is_one_one` | FAIL: `11` versus `1` | PASS |
| `b58_zero_pubkey_is_32_ones` | FAIL: 33 versus 32 ones | PASS |
| `b58_zero_signature_is_64_ones` | FAIL: 65 versus 64 ones | PASS |
| `b58_all_zero_lengths_have_no_extra_digit` | FAIL at length 0 | PASS for lengths 0 through 256 |
| `b58_leading_zeros_preserve_nonzero_value` | PASS | PASS |
| `b58_known_bitcoin_alphabet_fixtures` | PASS | PASS |

```text
RED:   test result: FAILED. 2 passed; 5 failed; 0 ignored; 0 measured; 0 filtered out
       test executable exit = 101
GREEN: test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
       test executable exit = 0
```

Known fixtures cover byte values 1, 57, 58, 255, `[1, 0]`, `a`, `bbb`, `ccc`, `simply a long string`, and `Hello World`. Leading-zero fixtures cover `[0, 1]`, `[0, 0, 1]`, `[0, 58]`, `[0, 0, 255]`, and `[0, 1, 0]`.

### SHA-256 provenance (raw source bytes unless marked harness)

| Artifact | SHA-256 |
| --- | --- |
| Original live source at start (LF) | `14c35252dc1069f7e7bf2c7e5dd330f15bcced1d7ddbb86d78aba1542ea1ca42` |
| Worktree source before tests (CRLF) | `a878aa52eb53b0f6925c69fd63fdc97fdc1c8760cd4f4a7c3b694c42d4bdfc21` |
| RED source with tests, unfixed encoder | `f7c55c74eaead015c60f4e7af712aa10f9e2af66d04bb609010a1c0cab01a0e8` |
| RED extracted harness (LF) | `082d59e5e9248c74ccf9d4e3cd952e07102caeb3e782443d45db9e65f7e441b5` |
| GREEN source with tests and fix | `67094ac31d128ea6d63a644ce363e0bfc68b8736e78ba40b6797d060a7ea6fec` |
| GREEN extracted harness (LF) | `e21c5faebd21f52e8489146675c419cfd12eb05bdfad1a8560e639506534c764` |

The different initial live/worktree hashes are solely line endings: their LF-normalized bytes were verified equal; live had 0 CRLFs and the worktree had 79. Production changes are only significant-digit initialization and its explanatory comment; the remaining additions are tests.

## Reproducible independent differential verification

The Python reference below uses a single big-endian arbitrary-precision integer and repeated `divmod(58)`, independently of the Rust per-byte/per-digit carry algorithm. It is a standard-library reference, not the third-party `base58` package. It compares a newly compiled executable from the exact fixed encoder against exhaustive inputs of lengths 0–2, all-zero lengths 0–256, and seeded ordinary/leading-zero random vectors. Duplicate vectors are removed and the actual unique count is reported. The same extracted Rust unit tests also run again.

Run this Python block inside WSL Ubuntu with Python 3; all generated source/executables go into an automatically cleaned temporary directory, outside either worktree. No network is used.

```python
from pathlib import Path
import hashlib
import json
import random
import subprocess
import tempfile

source = Path('/mnt/d/repos/mev_bot-north-star/tools/stream-capture-rs/grpc-server-only/src/encoding.rs')
raw = source.read_bytes()
assert hashlib.sha256(raw).hexdigest() == '67094ac31d128ea6d63a644ce363e0bfc68b8736e78ba40b6797d060a7ea6fec'
text = raw.decode('utf-8').replace('\r\n', '\n')
unit = text[text.index('pub fn b58_encode'):text.index('/// Standard-alphabet base64 encoder.')]
assert hashlib.sha256(unit.encode()).hexdigest() == 'e21c5faebd21f52e8489146675c419cfd12eb05bdfad1a8560e639506534c764'
function = unit[:unit.index('#[cfg(test)]')]
alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'

def reference(data):
    leading = len(data) - len(data.lstrip(b'\0'))
    number = int.from_bytes(data, 'big')
    encoded = ''
    while number:
        number, remainder = divmod(number, 58)
        encoded = alphabet[remainder] + encoded
    return '1' * leading + encoded

vectors = [bytes(n) for n in range(257)]
vectors += [bytes([n]) for n in range(256)]
vectors += [n.to_bytes(2, 'big') for n in range(65536)]
rng = random.Random(0xB58)
for _ in range(4096):
    vectors.append(bytes(rng.randrange(256) for _ in range(rng.randrange(257))))
for _ in range(4096):
    zeros = bytes(rng.randrange(1, 65))
    nonzero = bytes([rng.randrange(1, 256)])
    tail = bytes(rng.randrange(256) for _ in range(rng.randrange(193)))
    vectors.append(zeros + nonzero + tail)
vectors = list(dict.fromkeys(vectors))
payload = ''.join(data.hex() + '\n' for data in vectors)
main = r'''
fn main() {
    use std::io::{self, BufRead, Write};
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        assert_eq!(line.len() % 2, 0);
        let data: Vec<u8> = (0..line.len()).step_by(2)
            .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
            .collect();
        writeln!(out, "{}", b58_encode(&data)).unwrap();
    }
}
'''
with tempfile.TemporaryDirectory(prefix='north-star-b58-diff-') as scratch:
    directory = Path(scratch)
    harness = directory / 'unit.rs'
    harness.write_text(unit)
    compiler = '/home/alon/.cargo/bin/rustc'
    subprocess.run([compiler, '--edition=2021', '-D', 'warnings', '--test', str(harness), '-o', str(directory / 'unit')], check=True)
    subprocess.run([str(directory / 'unit'), '--nocapture'], check=True)
    cli = directory / 'cli.rs'
    cli.write_text(function + main)
    subprocess.run([compiler, '--edition=2021', '-D', 'warnings', str(cli), '-o', str(directory / 'cli')], check=True)
    result = subprocess.run([str(directory / 'cli')], input=payload, text=True, capture_output=True, check=True)
    actual = result.stdout.splitlines()
    assert len(actual) == len(vectors), (len(actual), len(vectors))
    mismatches = 0
    for data, output in zip(vectors, actual):
        expected = reference(data)
        if output != expected:
            mismatches += 1
            raise AssertionError((data.hex(), output, expected))
    print(json.dumps({
        'unique_vectors': len(vectors),
        'ordinary_random_draws': 4096,
        'leading_zero_random_draws': 4096,
        'seed': '0xB58',
        'mismatches': mismatches,
        'cli_exit': result.returncode,
        'input_hex_lines_sha256': hashlib.sha256(payload.encode()).hexdigest(),
        'output_base58_lines_sha256': hashlib.sha256(result.stdout.encode()).hexdigest(),
        'cli_source_sha256': hashlib.sha256(cli.read_bytes()).hexdigest(),
    }, sort_keys=True))
```

### Differential execution result

The block above was executed directly from this receipt in WSL Ubuntu. Its repeated Rust test run passed all 7 tests, and the Rust executable matched the independent Python reference for **74,194 unique vectors, 0 mismatches**, including 4,096 ordinary and 4,096 leading-zero randomized draws before deduplication. CLI exit was 0.

```json
{"cli_exit": 0, "cli_source_sha256": "183b7d34b5f21ca6cce4ec8ffefae0f844fd6320a1e7ce30710516b430c5d567", "input_hex_lines_sha256": "f6e1f3db835ad10054fb29bd69a73fbe527738a8975d5baa2bcfffd05c7103de", "leading_zero_random_draws": 4096, "mismatches": 0, "ordinary_random_draws": 4096, "output_base58_lines_sha256": "d68bd9a9902c88fc44e4169610f8be33a6f92b256911c34bec02eade877d9df9", "seed": "0xB58", "unique_vectors": 74194}
```

Execution wrapper (from Windows Git Bash):

```text
wsl.exe -d Ubuntu -- python3 -c 'from pathlib import Path; text=Path("/mnt/d/repos/mev_bot-north-star/tools/data-pipeline/north_star/BUILD_ENCODING_RECEIPT.md").read_text(); fence=chr(96)*3; code=text.split(fence+"python\n",1)[1].split("\n"+fence,1)[0]; exec(compile(code,"BUILD_ENCODING_RECEIPT.md:python","exec"))'
```

An initial wrapper using literal Markdown backticks was interpreted by the shell and failed before compilation/execution. Using `chr(96)*3` fixed quoting; the successful results above are from the corrected invocation. No dependency/install issue was encountered.

## Final verification

- `git diff --check` passed for both owned paths.
- Scoped status: `encoding.rs` modified; this receipt untracked. No staging or commit performed.
- Programmatically removing the test block and reverting the single initialization/comment change reproduces the HEAD source exactly (LF-normalized): no other production helper changed.
- Final fixed source SHA-256 remains `67094ac31d128ea6d63a644ce363e0bfc68b8736e78ba40b6797d060a7ea6fec`.
- Read-only final check confirmed original live encoder SHA-256 unchanged: `14c35252dc1069f7e7bf2c7e5dd330f15bcced1d7ddbb86d78aba1542ea1ca42`.
- Initial RED/GREEN scratch directory was removed after recording evidence. Differential scratch is automatically removed. No generated build artifacts remain in either repository from this task.

## Boundaries

- No commits, staging, credentials, trading, deployment, live process actions, or historical-record normalization.
- Full capture build/integration remains unverified by design; only the encoder substring and its tests are compiled here. SHA/helper imports are excluded, not mocked.
- Original malformed values remain evidence; a future source deployment does not authorize rewriting them.
