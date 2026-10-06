"""Wiring test for the size slot in the arm-joint KL.

The joint MATH is covered by grpo_loss.py --self-check (sums to one, recovers the
BUY marginal, self-KL is zero, gradient reaches BOTH slots). What that cannot cover is
the PLUMBING: does size_slot_logits read the right position for every row of a
right-padded batch, and does it refuse (ok=False) instead of inventing a value.

So the model here is a fake whose logits are PLANTED: it writes a unique sentinel into
the size-token columns at each row's own last real position. If the offset arithmetic
is wrong (left-padding assumption, a single row's length used for every row, off-by-one)
the extracted values will not match the sentinels and this fails loudly.

    python test_joint_kl_wiring.py
"""
import os
import sys
import types

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import torch  # noqa: E402

from grpo_loop import size_slot_logits  # noqa: E402

VOCAB = 128
SIZE_IDS = [101, 102, 103]          # stand-ins for " SMALL"/" MID"/" FULL"


class FakeTok:
    """Right-padding tokenizer over a toy vocabulary.

    Each prompt maps to a row of length (min_len + index mod k) so the rows genuinely
    differ in length - a single-row length used for every row is exactly the bug this
    test exists to catch.
    """

    padding_side = "right"

    def __init__(self, lengths):
        self.lengths = lengths

    def __call__(self, texts, return_tensors=None, padding=None, truncation=None,
                 max_length=None):
        assert return_tensors == "pt"
        rows = []
        for i, _t in enumerate(texts):
            n = self.lengths[i % len(self.lengths)]
            rows.append([7] * n)
        L = max(len(r) for r in rows)
        ids = torch.zeros(len(rows), L, dtype=torch.long)
        am = torch.zeros(len(rows), L, dtype=torch.long)
        for i, r in enumerate(rows):
            ids[i, :len(r)] = torch.tensor(r)
            am[i, :len(r)] = 1
        return {"input_ids": ids, "attention_mask": am}


class FakeModel:
    """Emits logits carrying a per-row sentinel at that row's last real position."""

    def __init__(self, sentinel_for_row):
        self.sentinel_for_row = sentinel_for_row

    def __call__(self, input_ids, attention_mask):
        B, T = input_ids.shape
        logits = torch.zeros(B, T, VOCAB)
        for b in range(B):
            n = int(attention_mask[b].sum().item())
            if n == 0:
                continue
            s = self.sentinel_for_row[b]
            for j, sid in enumerate(SIZE_IDS):
                logits[b, n - 1, sid] = s[j]
        return types.SimpleNamespace(logits=logits)


def main():
    fails = []

    def chk(name, cond, extra=""):
        print(f"  [{'ok' if cond else 'FAIL'}] {name}{'' if cond else '  ' + str(extra)}")
        if not cond:
            fails.append(name)

    lengths = [5, 9, 3, 7]                     # ragged -> real padding
    prompts = [f"p{i}" for i in range(len(lengths))]
    sentinels = [[1.0, 2.0, 3.0], [-1.5, 0.25, 4.0],
                 [0.0, 0.5, -0.5], [9.0, -9.0, 0.0]]
    model = FakeModel(sentinels)
    tok = FakeTok(lengths)

    rows, ok = size_slot_logits(model, tok, prompts, SIZE_IDS, "cpu")
    chk("shape_is_B_by_3", tuple(rows.shape) == (len(lengths), 3), tuple(rows.shape))
    chk("all_rows_have_a_slot", bool(ok.all()))
    for b in range(len(lengths)):
        chk(f"row{b}_reads_its_own_last_position",
            torch.allclose(rows[b], torch.tensor(sentinels[b], dtype=torch.float32)),
            (rows[b].tolist(), sentinels[b]))

    # a row that tokenises to nothing must be REFUSED, not given a plausible value
    tok0 = FakeTok([0, 4])
    rows0, ok0 = size_slot_logits(model, tok0, ["a", "b"], SIZE_IDS, "cpu")
    chk("empty_row_is_not_ok", bool(ok0[0].item()) is False)
    chk("empty_row_is_zeros", float(rows0[0].abs().sum()) == 0.0)
    chk("other_row_still_ok", bool(ok0[1].item()) is True)

    # the sentinel must be read from the SIZE columns only
    chk("reads_size_columns_not_others",
        float(rows.abs().sum()) > 0.0 and rows.shape[-1] == len(SIZE_IDS))

    print(f"\nwiring checks: {len(fails) == 0 and 'PASS' or 'FAIL'} "
          f"({len(fails)} failed)")
    return 1 if fails else 0


if __name__ == "__main__":
    raise SystemExit(main())
