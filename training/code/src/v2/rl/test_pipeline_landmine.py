#!/usr/bin/env python
"""REGRESSION TEST for the build_slinky_reserves.py pipeline landmine.

The failure that stopped the builder:

    File .../build_slinky_reserves.py, line ~142, in main
        agg['mayhem_rows'] += int(may[good].sum())
    TypeError: unsupported operand type(s) for +: 'int' and 'NoneType'

Root cause: when the mask ndarray has dtype=object (which happens whenever a
boolean column is materialised without an explicit dtype, e.g. straight out of
``to_pylist()`` on a part that stores SQL NULL for unavailable flags), NumPy's
object reduction returns None for an array containing a None element, and
``int(None)`` raises exactly that TypeError. It is data-dependent: it only fires
on a part that carries a NULL in that column, which is why the builder can look
green for dozens of parts and then die mid-run.

This test pins all three facts:
  1. the ORIGINAL expression raises TypeError('int' + 'NoneType') on NULL data;
  2. the FIXED helper (bool_col/count_true) handles the same data, and counts
     NULL as False by policy (documented, not silent);
  3. the FIXED helper refuses to guess on a null-typed or malformed column.

Run:  /home/alon/qwen27b-venv/bin/python /training/v2/code/src/v2/rl/test_pipeline_landmine.py
"""
from __future__ import annotations

import importlib.util
import os
import sys

import numpy as np
import pyarrow as pa

BUILDER = "/training/v2/code/src/v2/reserves/build_slinky_reserves.py"


def load_builder():
    spec = importlib.util.spec_from_file_location("build_slinky_reserves", BUILDER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_original_expression_raises():
    """The pre-fix pattern must raise the exact reported TypeError.

    Two accumulator seeds are pinned, because that is what decides whether the
    message names 'bool' or 'int':
      * ndarray.sum() over an object-dtype mask seeds from element 0 -> 'bool';
      * Python's builtin sum() / any int-seeded reduction     -> 'int'.
    The reported failure was the 'int' variant.
    """
    may = np.array([True, None, False, True], dtype=object)   # NULL -> None
    good = np.ones(4, dtype=bool)
    msgs = []
    try:
        v = int(may[good].sum())
        raise AssertionError(f"expected TypeError, got {v!r}")
    except TypeError as e:
        assert "NoneType" in str(e), str(e)
        msgs.append(f"ndarray.sum(): {e}")
    try:
        v = int(sum(may[good]))
        raise AssertionError(f"expected TypeError, got {v!r}")
    except TypeError as e:
        assert "int' and 'NoneType" in str(e), str(e)
        msgs.append(f"builtin sum(): {e}")
    return "original expressions raised TypeError -> " + " | ".join(msgs)


def test_fixed_helper_is_null_safe(mod):
    """The fixed helper must survive NULLs and must not return None."""
    t = pa.table({"is_mayhem": pa.array([True, None, False, True], type=pa.bool_())})
    may = mod.bool_col(t, "is_mayhem", "synthetic")
    assert may.dtype == np.bool_, may.dtype
    assert list(may) == [True, False, False, True], list(may)
    good = np.ones(4, dtype=bool)
    n = mod.count_true(may, good, "is_mayhem")
    assert n == 2 and isinstance(n, int), (n, type(n))
    # an object-dtype mask must be refused, NOT summed (the landmine itself)
    try:
        mod.count_true(np.array([True, None], dtype=object), np.ones(2, bool), "x")
        raise AssertionError("count_true accepted an object-dtype mask")
    except TypeError as e:
        assert "landmine" in str(e), str(e)
    return f"bool_col+count_true null-safe (mayhem_rows={n}); object mask refused"


def test_fixed_helper_refuses_garbage(mod):
    """A null-typed or non-boolean column must fail loudly, not silently coerce."""
    msgs = []
    for name, col, want in (
        ("null_typed", pa.array([None, None], type=pa.null()), "null-typed"),
        ("strings", pa.array(["yes", "no"]), "non-boolean value"),
        ("ints_2", pa.array([0, 2], type=pa.int64()), "non-boolean value"),
    ):
        try:
            mod.bool_col(pa.table({"c": col}), "c", name)
            raise AssertionError(f"{name}: expected a loud refusal")
        except ValueError as e:
            assert want in str(e), (name, str(e))
            msgs.append(f"{name}->ValueError")
    # int8 0/1 columns are still accepted (defensive but well-defined)
    ok = mod.bool_col(pa.table({"c": pa.array([0, 1], type=pa.int8())}), "c", "int8_01")
    assert list(ok) == [False, True]
    return "refusals: " + ", ".join(msgs) + "; int8 0/1 accepted"


def main():
    mod = load_builder()
    out = {
        "builder": BUILDER,
        "original_expression": test_original_expression_raises(),
        "fixed_helper": test_fixed_helper_is_null_safe(mod),
        "refusal_discipline": test_fixed_helper_refuses_garbage(mod),
    }
    ok = True
    for k, v in out.items():
        if k != "builder":
            print(f"[PASS] {k}: {v}")
    print("ALL PASS" if ok else "FAIL")
    return 0


if __name__ == "__main__":
    sys.exit(main())
