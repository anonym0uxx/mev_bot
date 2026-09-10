"""Independent synthetic event/finality fixtures; not chain admission evidence."""
import copy
import importlib.util
import itertools
import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def module():
    path = ROOT / "src/north_star/event_revisions.py"
    assert path.exists(), "event revision journal not implemented"
    spec = importlib.util.spec_from_file_location("event_revisions_test", path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def event(index=1, *, slot=42, fork=None, digest=None, **changes):
    projection = dict(schema_version="raw_projection_v1", event_id=f"raw:{index}",
                      source_path="source.zst", source_sha256="a" * 64,
                      record_index=index, raw_record_sha256=f"{index:064x}",
                      record_type="transaction", slot=slot, signature="Signature",
                      account_pubkey=None, account_write_version=None, blockhash=None,
                      source_commitment="CONFIRMED", observed_at_unix_ms=100,
                      slot_status=None, revision=0, training_eligible=False,
                      source_admission="unadmitted")
    projection.update(changes)
    return dict(kind="event", chain="solana", network="mainnet-beta",
                projection=projection, fork_id=fork,
                fork_evidence_ref="synthetic:block-membership" if fork else None,
                payload_sha256=digest)


def test_raw_identity_separate_from_logical_identity_and_history_is_immutable():
    api = module()
    row = event()
    journal = api.append_revisions((), [row])
    original = copy.deepcopy(journal)
    row["projection"]["signature"] = "mutation"
    entry = json.loads(journal[0])
    assert entry["input"]["projection"]["signature"] == "Signature"
    assert entry["raw_delivery_id"] != entry["event_id"]
    assert entry["revision"] == 0
    assert entry["input"]["projection"]["event_id"] == "raw:1"
    more = api.append_revisions(journal, [event(2)])
    assert more[:1] == original == journal
    assert json.loads(more[1])["event_id"] == entry["event_id"]
    assert json.loads(more[1])["revision"] == 1
    view = api.summarize(more)
    assert len(view["events"]) == 1
    assert view["events"][0]["later_finality"] is None
    assert view["events"][0]["observed_commitments"] == ["CONFIRMED"]
    assert view["training_eligible"] is False


@pytest.mark.parametrize("kwargs", [{"max_events": 0}, {"max_events": 101},
                                     {"max_events": True}, {"max_revisions": 0},
                                     {"max_revisions": 401}, {"max_revisions": 1.5}])
def test_hard_limits_reject_before_iteration(kwargs):
    class Poison(list):
        def __iter__(self):
            raise AssertionError("work began before budget validation")
    with pytest.raises(ValueError, match="bounds"):
        module().append_revisions((), Poison([event()]), **kwargs)


def test_revision_and_event_budget_reject_whole_batch_without_partial_append():
    api = module()
    first = api.append_revisions((), [event()])
    with pytest.raises(ValueError, match="revision_bound"):
        api.append_revisions(first, [event(2)], max_revisions=1)
    with pytest.raises(ValueError, match="event_bound"):
        api.append_revisions(first, [event(2, signature="Other")], max_events=1)
    assert len(first) == 1
    with pytest.raises(ValueError, match="bounded_sequence"):
        api.append_revisions((), iter([event()]))


def test_identity_preserves_network_slot_fork_and_account_write_version():
    api = module()
    rows = [event(), event(2, slot=43), event(3, fork="BlockA"),
            event(4, fork="BlockB"),
            event(5, record_type="account", account_pubkey="Account", account_write_version=7),
            event(6, record_type="account", account_pubkey="Account", account_write_version=8),
            event(7, record_type="account", account_pubkey="Other", account_write_version=7),
            event(8)]
    rows[-1]["network"] = "devnet"
    journal = api.append_revisions((), rows)
    assert len({json.loads(r)["event_id"] for r in journal}) == 8
    assert json.loads(journal[4])["identity"] == ["solana", "mainnet-beta", "account", 42, None, "Account", 7]
    assert all(json.loads(r)["revision"] == 0 for r in journal)


def test_block_and_slot_notification_identities_are_not_transactions():
    api = module()
    rows = [event(1, record_type="block_meta", blockhash="A"),
            event(2, record_type="block_meta", blockhash="B"),
            event(3, record_type="slot", slot_status="Processed"),
            event(4, record_type="slot", slot_status="Finalized")]
    entries = [json.loads(r) for r in api.append_revisions((), rows)]
    assert entries[0]["event_id"] != entries[1]["event_id"]
    assert entries[2]["event_id"] == entries[3]["event_id"]


@pytest.mark.parametrize("change", [dict(fork_id="A", fork_evidence_ref=None),
                                     dict(chain=""), dict(network=None)])
def test_identity_scope_requires_literal_nonempty_provenance(change):
    row = event()
    row.update(change)
    with pytest.raises(ValueError):
        module().append_revisions((), [row])


def test_duplicate_delivery_conflicting_delivery_and_payload_conflicts_are_distinct():
    api = module()
    a = event(digest="b" * 64)
    duplicate = copy.deepcopy(a)
    conflicting = event(digest="c" * 64, raw_record_sha256="d" * 64)
    replay = event(2, digest="b" * 64)
    original = [a, duplicate, conflicting, replay]
    for ordered in itertools.permutations(original):
        view = api.summarize(api.append_revisions((), list(ordered)))
        assert view["counts"]["duplicate_raw_delivery"] == 1
        assert view["counts"]["conflicting_raw_delivery"] == 1
        assert view["counts"]["repeated_logical_payload"] == 1
        assert view["counts"]["conflicting_logical_payload"] == 1
        state = view["events"][0]
        assert state["payload_candidates"] == ["b" * 64, "c" * 64]
        assert state["resolved_payload_sha256"] is None
        assert state["status"] == "conflicted"
        assert state["later_finality"] is None


def test_envelope_and_producer_hash_are_not_full_payload_equivalence():
    rows = [event(source_payload_raw_hash="b" * 64),
            event(2, source_payload_raw_hash="b" * 64)]
    view = module().summarize(module().append_revisions((), rows))
    assert view["counts"]["payload_unverified_deliveries"] == 2
    assert view["counts"]["repeated_logical_payload"] == 0
    assert view["events"][0]["resolved_payload_sha256"] is None
    assert view["events"][0]["payload_null_reason"] == "full_payload_digest_not_supplied"


def test_conflicting_raw_locator_taints_both_logical_events():
    rows = [event(digest="b" * 64), event(signature="Different", digest="b" * 64)]
    view = module().summarize(module().append_revisions((), rows))
    assert view["counts"]["conflicting_raw_delivery"] == 1
    assert all(e["status"] == "conflicted" for e in view["events"])


def evidence(index=20, *, slot=42, fork="A", status="Finalized", authority="slot_notification"):
    row = event(index, slot=slot, fork=fork, record_type="slot", slot_status=status)
    row.update(kind="slot_evidence", evidence_authority=authority,
               evidence_ref="synthetic:slot-notification")
    return row


def test_explicit_matching_finality_is_later_evidence_not_observed_commitment():
    api = module()
    first = api.append_revisions((), [event(fork="A")])
    later = api.append_revisions(first, [evidence()])
    assert later[:len(first)] == first
    assert api.summarize(first)["events"][0]["later_finality"] is None
    view = api.summarize(later)
    state = view["events"][0]
    assert state["later_finality"] == "finalized"
    assert state["observed_commitments"] == ["CONFIRMED"]
    assert len(state["finality_evidence_ids"]) == 1
    assert state["correction_available_at"] is None
    assert state["correction_clock"] == "UNKNOWN"
    assert view == api.summarize(api.append_revisions((), [evidence(), event(fork="A")]))


@pytest.mark.parametrize("proof", [evidence(slot=999999), evidence(fork="B"),
                                     evidence(status="Unknown"), evidence(status="Confirmed"),
                                     evidence(status="Processed"),
                                     evidence(authority="subscription_commitment"),
                                     evidence(fork=None)])
def test_no_finality_from_other_slot_fork_unknown_or_subscription(proof):
    view = module().summarize(module().append_revisions((), [event(fork="A"), proof]))
    assert view["events"][0]["later_finality"] is None


def test_unknown_fork_and_bare_slot_projection_remain_unfinalized():
    api = module()
    for rows in ([event(), evidence(fork=None)],
                 [event(fork="A"), event(2, record_type="slot", slot_status="Finalized")],
                 [event(observed_at_unix_ms=2**63, source_commitment="FINALIZED")]):
        view = api.summarize(api.append_revisions((), rows))
        assert all(e["later_finality"] is None for e in view["events"])


@pytest.mark.parametrize("status", ["Dead", "Reorg"])
def test_dead_and_reorg_append_explicit_supersession_without_deleting_other_fork(status):
    api = module()
    rows = [event(fork="A"), event(2, fork="B"), evidence(status=status)]
    expected = None
    for ordered in itertools.permutations(rows):
        journal = api.append_revisions((), list(ordered))
        assert len(journal) == 3
        assert any(json.loads(r)["operation"] == "supersede_slot" for r in journal)
        view = api.summarize(journal)
        if expected is None:
            expected = view
        assert view == expected
        a = next(e for e in view["events"] if e["identity"][4] == "A")
        b = next(e for e in view["events"] if e["identity"][4] == "B")
        assert a["status"] == "superseded" and a["superseded_by"]
        assert a["later_finality"] is None
        assert b["status"] == "observed" and not b["superseded_by"]


def test_contradictory_finalized_dead_never_wins_by_order():
    api = module()
    rows = [event(fork="A"), evidence(), evidence(21, status="Dead")]
    for ordered in itertools.permutations(rows):
        state = api.summarize(api.append_revisions((), list(ordered)))["events"][0]
        assert state["later_finality"] is None
        assert state["status"] == "conflicted"
        assert state["superseded_by"]
        assert state["finality_null_reason"] == "conflicting_slot_evidence"


def test_finality_conflicting_duplicate_delivery_and_cross_network_fail_closed():
    api = module()
    proof = evidence()
    changed = copy.deepcopy(proof)
    changed["projection"]["slot_status"] = "Unknown"
    other_network = evidence(21)
    other_network["network"] = "devnet"
    state = api.summarize(api.append_revisions((), [event(fork="A"), proof, changed, other_network]))["events"][0]
    assert state["later_finality"] is None
    assert state["finality_null_reason"] == "conflicting_slot_evidence"


@pytest.mark.parametrize("field,value", [("slot", True), ("slot", -1), ("slot", "42"),
                                         ("record_index", 1.2), ("source_sha256", "A" * 64),
                                         ("raw_record_sha256", "bad"), ("revision", -1),
                                         ("schema_version", "other"), ("training_eligible", True),
                                         ("source_admission", "admitted"),
                                         ("observed_at_unix_ms", False), ("slot", 2**64)])
def test_malformed_projection_is_rejected_without_coercion(field, value):
    with pytest.raises(ValueError):
        module().append_revisions((), [event(**{field: value})])


@pytest.mark.parametrize("version", [None, True, -1, "7", 2**64])
def test_account_write_version_is_required_exact_u64(version):
    with pytest.raises(ValueError):
        module().append_revisions((), [event(record_type="account", account_pubkey="Account",
                                            account_write_version=version)])


def test_payload_digest_and_kind_validation():
    for row in [event(digest="bad"), dict(event(), kind="unexpected"),
                dict(evidence(), evidence_ref=""), dict(event(), projection={})]:
        with pytest.raises(ValueError):
            module().append_revisions((), [row])


def test_journal_tampering_and_summary_bounds_are_rejected():
    api = module()
    journal = api.append_revisions((), [event()])
    for key, value in [("revision", 4), ("event_id", "forged"),
                       ("operation", "supersede_slot"), ("assertion_id", "forged")]:
        row = json.loads(journal[0])
        row[key] = value
        changed = (json.dumps(row),)
        with pytest.raises(ValueError, match="journal"):
            api.append_revisions(changed, [])
        with pytest.raises(ValueError, match="journal"):
            api.summarize(changed)
    with pytest.raises(ValueError, match="revision_bound"):
        api.summarize(("not-json",) * 401)
    with pytest.raises(ValueError, match="bounded_sequence"):
        api.summarize(iter(journal))


def test_incremental_and_batch_journals_identical_and_boundary_is_inclusive():
    api = module()
    rows = [event(i + 1, slot=i) for i in range(100)]
    whole = api.append_revisions((), rows, max_events=100, max_revisions=100)
    first = api.append_revisions((), rows[:50])
    assert api.append_revisions(first, rows[50:]) == whole
    repeated = api.append_revisions((), [event()] * 400)
    assert len(repeated) == 400
    assert api.summarize(repeated)["counts"]["duplicate_raw_delivery"] == 399


def test_event_budget_preflight_before_projection_serialization():
    bad_payload = event(2, slot=43, nonserializable=object())
    with pytest.raises(ValueError, match="event_bound"):
        module().append_revisions((), [event(), bad_payload], max_events=1)


def test_two_finalized_forks_at_same_slot_are_conflicting_not_both_finalized():
    api = module()
    rows = [event(fork="A"), event(2, fork="B"), evidence(), evidence(21, fork="B")]
    for ordered in itertools.permutations(rows):
        view = api.summarize(api.append_revisions((), list(ordered)))
        assert all(e["later_finality"] is None for e in view["events"])
        assert all(e["finality_null_reason"] == "conflicting_slot_evidence" for e in view["events"])


def test_real_adapter_envelope_integration_remains_unadmitted_and_unknown():
    spec = importlib.util.spec_from_file_location("raw_adapter_integration", ROOT / "src/north_star/raw_adapter.py")
    raw_adapter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(raw_adapter)
    raw = json.dumps(dict(record_type="slot", slot=42, recv_unix_ms=123,
                          record_index=1, payload=dict(slot=42, parent=41, status="Finalized"))).encode()
    p = raw_adapter.project_record(raw, source_path="synthetic.ndjson", source_sha256="a" * 64,
                                   source_commitment="CONFIRMED")
    row = dict(event(), projection=p)
    view = module().summarize(module().append_revisions((), [row]))
    assert view["events"][0]["later_finality"] is None
    assert view["source_admission"] == "unadmitted"


def test_size_bounds_reject_before_json_decode_or_serialization():
    api = module()
    with pytest.raises(ValueError, match="entry_size_bound"):
        api.summarize(("x" * (1_048_576 + 1),))
    with pytest.raises(ValueError, match="entry_size_bound"):
        api.append_revisions((), [event(extra="x" * 1_048_576)])
