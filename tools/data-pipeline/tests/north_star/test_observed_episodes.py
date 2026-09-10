"""Software fixtures only: none are source-admitted training examples."""
import importlib
from copy import deepcopy


import pytest


def api():
    try:
        module = importlib.import_module("north_star.observed_episodes")
    except ModuleNotFoundError:
        pytest.fail("observed episode assembler is not implemented")
    assert callable(getattr(module, "assemble_observed_episodes", None))
    return module


def action(number=1, *, before=0, after=10, kind="BUY", wallet=None, **changes):
    wallet = wallet or "11111111111111111111111111111111"
    row = {
        "schema_version": "proven_executed_venue_action_v1",
        "action_id": f"action-{number}", "event_id": f"event-{number}",
        "parent_ids": [f"parent-{number}", "shared-parent"],
        "wallet": wallet, "mint": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        "signature": "1" * 64, "chain": "solana", "venue": "pumpfun",
        "origin": "observed_action", "recommended": False,
        "canonical_identity_status": "verified", "executed_venue_fill": True,
        "instruction_layout_status": "verified", "transaction_status": "confirmed",
        "finality": "finalized", "action": kind, "decimals": 6,
        "token_unit": "raw", "cash_unit": "lamports",
        "inventory_before_raw": before, "inventory_after_raw": after,
        "quantity_raw": abs(after - before), "consideration_lamports": 100,
        "embedded_venue_fee_lamports": 1, "additional_fee_lamports": 5,
        "fee_scope_id": f"allocated-fee-{number}",
        "fee_semantics": "consideration_includes_venue_fee_additional_fee_excludes_it",
        "slot": number, "transaction_index": 0, "instruction_index": 0,
        "decision_cutoff_unix_ms": number * 10,
        "event_at_unix_ms": number * 10 + 1,
        "available_at_unix_ms": number * 10 + 2,
        "evidence_refs": [
            {"evidence_id": f"{role}-{number}", "role": role,
             "action_id": f"action-{number}", "source_sha256": "a" * 64,
             "parent_ids": [f"evidence-parent-{role}-{number}"],
             "available_at_unix_ms": number * 10 if role in ("causal", "opening_inventory") else number * 10 + 2}
            for role in ("causal", "execution", "identity", "layout", "inventory", "fees", "ordering", "opening_inventory")
        ],
    }
    row.update(changes)
    return row


def test_source_backed_buy_retains_open_inventory_and_parents_without_targets():
    row = action()
    result = api().assemble_observed_episodes([row], cutoff_unix_ms=100)
    assert len(result) == 1
    episode = result[0]
    assert episode["state"] == "open"
    assert episode["remaining_inventory_raw"] == 10
    assert episode["cash_delta_lamports"] == -105
    assert episode["actions"] == [row]
    assert set(episode["parent_ids"]) == set(row["parent_ids"]) | {
        p for ref in row["evidence_refs"] for p in ref["parent_ids"]
    }
    assert episode["training_eligible"] is False
    assert episode["imitation_eligible"] is False
    assert "rationale" not in episode and "recommended_action" not in episode


def test_partial_exits_adds_close_and_reentry_split_only_at_flat():
    rows = [action(), action(2, before=10, after=6, kind="REDUCE"),
            action(3, before=6, after=8, kind="ADD"),
            action(4, before=8, after=0, kind="EXIT_ALL"), action(5)]
    episodes = api().assemble_observed_episodes(rows, cutoff_unix_ms=100)
    assert [x["state"] for x in episodes] == ["closed", "open"]
    assert [len(x["actions"]) for x in episodes] == [4, 1]
    assert episodes[0]["cash_delta_lamports"] == -20
    assert episodes[0]["remaining_inventory_raw"] == 0
    assert set(episodes[0]["parent_ids"]) >= {"parent-1", "parent-4", "shared-parent"}
    assert "parent-5" not in episodes[0]["parent_ids"]
    rows[0]["parent_ids"].append("mutated")
    assert "mutated" not in episodes[0]["actions"][0]["parent_ids"]


@pytest.mark.parametrize("changes,reason", [
    ({"canonical_identity_status": "quarantined"}, "identity_quarantined"),
    ({"schema_version": "transaction_balance_reconciliation_v1"}, "not_proven_venue_action"),
    ({"instruction_layout_status": "unknown"}, "unverified_execution"),
    ({"executed_venue_fill": 1}, "unverified_execution"),
    ({"executed_venue_fill": False}, "unverified_execution"),
    ({"transaction_status": "failed"}, "unverified_execution"),
    ({"finality": None}, "unverified_execution"),
    ({"canonical_identity_status": "format_valid_only"}, "unverified_execution"),
    ({"recommended": 0}, "not_observed_action"),
    ({"recommended": True}, "not_observed_action"),
    ({"origin": "deterministic_recommendation"}, "not_observed_action"),
    ({"rationale": "I knew it would go up"}, "unexpected_action_fields"),
    ({"wallet": "1" * 33}, "invalid_wallet"),
    ({"mint": " " + "1" * 32}, "invalid_mint"),
    ({"signature": "1" * 63}, "invalid_signature"),
    ({"chain": "ethereum"}, "unsupported_scope"),
    ({"venue": "unverified_venue"}, "unsupported_scope"),
])
def test_only_explicit_proven_observed_venue_execution_is_accepted(changes, reason):
    with pytest.raises(ValueError, match=reason):
        api().assemble_observed_episodes([action(**changes)], cutoff_unix_ms=100)


@pytest.mark.parametrize("field", [
    "inventory_before_raw", "inventory_after_raw", "quantity_raw", "decimals",
    "consideration_lamports", "embedded_venue_fee_lamports", "additional_fee_lamports",
    "slot", "transaction_index", "instruction_index", "event_at_unix_ms",
    "available_at_unix_ms", "decision_cutoff_unix_ms",
])
@pytest.mark.parametrize("bad", [True, None, "10", 1.5, -1, 2**64])
def test_exact_integer_units_no_bool_or_unknown(field, bad):
    with pytest.raises(ValueError, match="invalid_" + field):
        api().assemble_observed_episodes([action(**{field: bad})], cutoff_unix_ms=100)


@pytest.mark.parametrize("changes,reason", [
    ({"token_unit": "ui"}, "invalid_units"),
    ({"cash_unit": "SOL"}, "invalid_units"),
    ({"decimals": 256}, "invalid_decimals"),
    ({"quantity_raw": 9}, "inventory_quantity_mismatch"),
    ({"quantity_raw": 0}, "inventory_quantity_mismatch"),
    ({"action": "HOLD"}, "not_executed_trade"),
    ({"action": "SELL"}, "not_executed_trade"),
    ({"action": "ADD"}, "action_inventory_mismatch"),
    ({"fee_semantics": "unknown"}, "invalid_fee_semantics"),
    ({"embedded_venue_fee_lamports": 101}, "invalid_embedded_fee"),
    ({"parent_ids": []}, "invalid_parent_ids"),
    ({"action_id": ""}, "invalid_action_id"),
    ({"event_id": None}, "invalid_event_id"),
    ({"fee_scope_id": False}, "invalid_fee_scope_id"),
    ({"available_at_unix_ms": 10}, "invalid_event_availability"),
    ({"decision_cutoff_unix_ms": 12}, "invalid_decision_cutoff"),
    ({"evidence_refs": []}, "missing_evidence_roles"),
])
def test_inventory_fees_clocks_and_required_lineage(changes, reason):
    with pytest.raises(ValueError, match=reason):
        api().assemble_observed_episodes([action(**changes)], cutoff_unix_ms=100)


@pytest.mark.parametrize("field,bad,reason", [
    ("action_id", "other", "unbound_evidence"),
    ("source_sha256", "x" * 64, "invalid_evidence_source_sha256"),
    ("parent_ids", [], "invalid_evidence_parent_ids"),
    ("available_at_unix_ms", True, "invalid_evidence_availability"),
    ("available_at_unix_ms", 11, "postdecision_causal_evidence"),
    ("evidence_id", "", "invalid_evidence_id"),
    ("role", "outcome_rationale", "invalid_evidence_role"),
])
def test_causal_evidence_is_bound_hashed_and_available_at_decision(field, bad, reason):
    row = action()
    row["evidence_refs"][0][field] = bad
    with pytest.raises(ValueError, match=reason):
        api().assemble_observed_episodes([row], cutoff_unix_ms=100)


def test_execution_evidence_cannot_become_causal_and_missing_role_rejects():
    row = action()
    row["evidence_refs"][1]["available_at_unix_ms"] = 101
    with pytest.raises(ValueError, match="evidence_after_action_availability"):
        api().assemble_observed_episodes([row], cutoff_unix_ms=100)
    row = action()
    row["evidence_refs"].pop()
    with pytest.raises(ValueError, match="missing_evidence_roles"):
        api().assemble_observed_episodes([row], cutoff_unix_ms=100)


@pytest.mark.parametrize("case,reason", [
    ("duplicate", "duplicate_action_id"), ("fee", "duplicate_fee_scope_id"),
    ("order", "nonchronological_wallet_actions"), ("gap", "inventory_discontinuity"),
    ("decimals", "inconsistent_mint_decimals"), ("clock", "nonchronological_wallet_events"),
    ("same_order", "nonchronological_wallet_actions"),
])
def test_no_silent_sorting_gaps_duplicate_actions_or_double_fee_charges(case, reason):
    rows = [action(), action(2, before=10, after=0, kind="EXIT_ALL")]
    if case == "duplicate":
        rows.append(deepcopy(rows[0]))
    if case == "fee":
        rows[1]["fee_scope_id"] = rows[0]["fee_scope_id"]
    if case == "order":
        rows.reverse()
    if case == "gap":
        rows[1] = action(2, before=11, after=0, kind="EXIT_ALL")
    if case == "decimals":
        rows[1]["decimals"] = 8
    if case == "clock":
        rows[0]["slot"], rows[1]["slot"] = 2, 1
        rows.reverse()
    if case == "same_order":
        rows[1]["slot"] = 1
    with pytest.raises(ValueError, match=reason):
        api().assemble_observed_episodes(rows, cutoff_unix_ms=100)


def test_order_is_per_wallet_not_global_and_mints_are_separate():
    other = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
    episodes = api().assemble_observed_episodes([action(2), action(1, wallet=other)], cutoff_unix_ms=100)
    assert len(episodes) == 2
    assert {e["wallet"] for e in episodes} == {action()["wallet"], other}


def test_known_positive_opening_is_left_censored_not_fabricated_buy():
    row = action(before=10, after=0, kind="EXIT_ALL")
    episode = api().assemble_observed_episodes([row], cutoff_unix_ms=100)[0]
    assert episode["state"] == "censored"
    assert episode["left_censored"] is True
    assert episode["right_censored"] is False
    assert episode["opening_inventory_raw"] == 10
    assert episode["actions"] == [row]
    assert episode["cash_delta_lamports"] == 95
    assert "pnl_lamports" not in episode


def test_closed_capture_right_censors_unclosed_episode():
    episode = api().assemble_observed_episodes([action()], cutoff_unix_ms=100,
                                               observation_end="censored")[0]
    assert episode["state"] == "censored" and episode["right_censored"] is True
    assert episode["remaining_inventory_raw"] == 10


@pytest.mark.parametrize("cutoff", [True, None, -1, 1.5, "100", 2**64])
def test_invalid_cutoff_even_for_empty_input(cutoff):
    with pytest.raises(ValueError, match="invalid_cutoff"):
        api().assemble_observed_episodes([], cutoff_unix_ms=cutoff)


def test_cutoff_and_strict_work_bounds_no_partial_return():
    with pytest.raises(ValueError, match="action_after_cutoff"):
        api().assemble_observed_episodes([action()], cutoff_unix_ms=11)
    with pytest.raises(ValueError, match="invalid_observation_end"):
        api().assemble_observed_episodes([], cutoff_unix_ms=100, observation_end=True)
    with pytest.raises(ValueError, match="action_bound_exceeded"):
        api().assemble_observed_episodes([action(), action(2)], cutoff_unix_ms=100, max_actions=1)
    with pytest.raises(ValueError, match="invalid_max_actions"):
        api().assemble_observed_episodes([], cutoff_unix_ms=100, max_actions=True)
    assert api().assemble_observed_episodes([], cutoff_unix_ms=100) == []



def report_fixture(tmp_path):
    import hashlib
    import json
    folder = tmp_path / "source"
    folder.mkdir()
    reconciled = [{"schema_version": "transaction_balance_reconciliation_v1",
                   "canonical_identity_status": "quarantined", "record_index": i,
                   "event_id": f"event-{i}", "accounts": [{"account_key": "1" * 33}],
                   "source_sha256": "a" * 64, "raw_record_sha256": "b" * 64}
                  for i in range(9)]
    rejected = [{"reason": "missing_token_counterpart", "record_index": 9,
                 "training_eligible": False}]
    hashes = {}
    for name, rows in (("reconciled.ndjson", reconciled), ("rejected.ndjson", rejected)):
        data = ''.join(json.dumps(x) + '\n' for x in rows).encode()
        (folder / name).write_bytes(data)
        hashes[name] = hashlib.sha256(data).hexdigest()
    receipt = {"schema_version": "transaction_truth_sample_receipt_v1",
               "transactions_attempted": 10, "max_transactions": 10,
               "reconciled_count": 9, "rejected_count": 1,
               "canonical_identity_quarantined_count": 9,
               "output_sha256": hashes, "training_eligible": False,
               "selected_record_indices": list(range(10))}
    (folder / "receipt.json").write_text(json.dumps(receipt))
    return folder, reconciled, rejected


def test_report_attempt_retains_all_nine_quarantines_and_original_rejection(tmp_path):
    import json
    source, reconciled, rejected = report_fixture(tmp_path)
    output = tmp_path / "new-audit"
    summary = api().audit_transaction_truth_report(source, output)
    assert summary["valid_episode_count"] == 0
    assert summary["refused_episode_construction_count"] == 9
    assert summary["upstream_rejection_count"] == 1
    assert summary["training_eligible"] is False
    audit = [json.loads(x) for x in (output / "rejection_audit.ndjson").read_text().splitlines()]
    assert [r["original_record"] for r in audit[:9]] == reconciled
    assert [r["original_record"] for r in audit[9:]] == rejected
    assert all(r["reason"] == "identity_quarantined" for r in audit[:9])
    assert audit[9]["reason"] == "missing_token_counterpart"
    assert (output / "episodes.ndjson").read_bytes() == b""
    assert json.loads((output / "receipt.json").read_text()) == summary
    assert (output / "upstream_rejected.ndjson").read_bytes() == (source / "rejected.ndjson").read_bytes()
    with pytest.raises(FileExistsError):
        api().audit_transaction_truth_report(source, output)


@pytest.mark.parametrize("case,reason", [
    ("hash", "report_hash_mismatch"), ("count", "report_count_mismatch"),
    ("bound", "report_bound_exceeded"), ("bool", "invalid_report_count"),
])
def test_report_audit_fails_before_creating_output_for_corrupt_report(tmp_path, case, reason):
    import json
    source, _, _ = report_fixture(tmp_path)
    receipt = json.loads((source / "receipt.json").read_text())
    if case == "hash":
        receipt["output_sha256"]["reconciled.ndjson"] = "0" * 64
    elif case == "count":
        receipt["reconciled_count"] = 8
    elif case == "bound":
        receipt["transactions_attempted"] = 11
    elif case == "bool":
        receipt["rejected_count"] = True
    (source / "receipt.json").write_text(json.dumps(receipt))
    output = tmp_path / "audit"
    with pytest.raises(ValueError, match=reason):
        api().audit_transaction_truth_report(source, output)
    assert not output.exists()



def test_stable_episode_identity_and_all_ancestry_survive_without_crosswallet_pooling():
    rows = [action(), action(2, before=10, after=0, kind="EXIT_ALL")]
    first = api().assemble_observed_episodes(rows, cutoff_unix_ms=100)[0]
    second = api().assemble_observed_episodes(rows, cutoff_unix_ms=100)[0]
    assert first["episode_id"] == second["episode_id"]
    assert first["action_ids"] == [r["action_id"] for r in rows]
    assert first["event_ids"] == [r["event_id"] for r in rows]
    assert set(first["evidence_ids"]) == {r["evidence_id"] for a in rows for r in a["evidence_refs"]}
    assert first["origin"] == "observed_action"
    other = action(3, mint="TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")
    assert len(api().assemble_observed_episodes([rows[0], other], cutoff_unix_ms=100)) == 2


def test_no_arbitrary_nested_narrative_or_duplicate_evidence():
    row = action()
    row["evidence_refs"][0]["rationale"] = "invented"
    with pytest.raises(ValueError, match="unexpected_evidence_fields"):
        api().assemble_observed_episodes([row], cutoff_unix_ms=100)
    row = action()
    row["evidence_refs"].append(deepcopy(row["evidence_refs"][0]))
    with pytest.raises(ValueError, match="duplicate_evidence_id"):
        api().assemble_observed_episodes([row], cutoff_unix_ms=100)


def test_decision_inventory_evidence_must_precede_decision():
    row = action()
    row["evidence_refs"].append({**row["evidence_refs"][0],
                               "evidence_id": "opening-inventory-proof",
                               "role": "opening_inventory",
                               "available_at_unix_ms": 11})
    with pytest.raises(ValueError, match="postdecision_opening_inventory_evidence"):
        api().assemble_observed_episodes([row], cutoff_unix_ms=100)
