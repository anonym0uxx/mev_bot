"""Pure development-only append journal; immutable serialized revision entries.

Not a consumer schema, source admission or consensus verifier. No I/O or clock.
"""
import hashlib
import json
import re

MAX_ENTRY_CHARS = 1_048_576


def _size_preflight(value, depth=0):
    if depth > 32:
        raise ValueError("entry_depth_bound")
    if isinstance(value, str):
        cost = len(value)
    elif type(value) in (dict, list, tuple):
        if len(value) > 10000:
            raise ValueError("entry_item_bound")
        cost = 0
        values = value.items() if type(value) is dict else value
        for child in values:
            cost += _size_preflight(child, depth + 1) + 1
            if cost > MAX_ENTRY_CHARS:
                raise ValueError("entry_size_bound")
    else:
        cost = 32
    if cost > MAX_ENTRY_CHARS:
        raise ValueError("entry_size_bound")
    return cost


def _encode(value):
    text = json.dumps(value, sort_keys=True, ensure_ascii=True,
                      separators=(",", ":"), allow_nan=False)
    if len(text) > MAX_ENTRY_CHARS:
        raise ValueError("entry_size_bound")
    return text


def _id(prefix, value):
    return prefix + hashlib.sha256(_encode(value).encode()).hexdigest()


def _identity(item):
    p = item["projection"]
    for key in ("chain", "network"):
        _text(item.get(key))
    if item["fork_id"] is not None:
        _text(item["fork_id"])
        _text(item.get("fork_evidence_ref"))
    scope = [item["chain"], item["network"], p["record_type"], p["slot"], item["fork_id"]]
    kind = p["record_type"]
    if kind == "transaction":
        _text(p["signature"])
        return scope + [p["signature"]]
    if kind == "account":
        _text(p["account_pubkey"])
        _uint(p["account_write_version"])
        return scope + [p["account_pubkey"], p["account_write_version"]]
    if kind == "block_meta":
        _text(p["blockhash"])
        return scope + [p["blockhash"]]
    if kind == "slot":
        return scope
    raise ValueError("unknown_record_type")


def _text(value):
    if not isinstance(value, str) or not value.strip():
        raise ValueError("nonempty_literal_required")


def _operation(item):
    if item["kind"] == "event":
        return "observe_event"
    if item["kind"] != "slot_evidence" or item["projection"]["record_type"] != "slot":
        raise ValueError("invalid_evidence_kind")
    _text(item.get("evidence_authority"))
    _text(item.get("evidence_ref"))
    return "supersede_slot" if item["projection"]["slot_status"] in ("Dead", "Reorg") else "observe_slot_evidence"


def _uint(value):
    if type(value) is not int or not 0 <= value < 2**64:
        raise ValueError("exact_u64_required")


def _digest(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ValueError("literal_sha256_required")


def _validate(item):
    try:
        p = item["projection"]
        if (p["schema_version"] != "raw_projection_v1" or p["training_eligible"] is not False
                or p["source_admission"] != "unadmitted"):
            raise ValueError("development_projection_required")
        for key in ("slot", "record_index", "revision", "observed_at_unix_ms"):
            _uint(p[key])
        for key in ("source_sha256", "raw_record_sha256"):
            _digest(p[key])
        for key in ("source_path", "event_id"):
            _text(p[key])
        if p["source_commitment"] is not None:
            _text(p["source_commitment"])
        if item["payload_sha256"] is not None:
            _digest(item["payload_sha256"])
        _identity(item)
        _operation(item)
        _encode(item)
    except (KeyError, TypeError, OverflowError, RecursionError) as exc:
        raise ValueError("invalid_input") from exc


def _row(item, revision):
    p = item["projection"]
    identity = _identity(item)
    return dict(schema_version="event_revision_v1", input=item,
                event_id=_id("event:", identity), identity=identity,
                operation=_operation(item), assertion_id=_id("assertion:", item),
                raw_delivery_id=_id("raw_delivery:", [p["source_path"], p["source_sha256"], p["record_index"]]),
                revision=revision)


def _read_journal(journal):
    if type(journal) is not tuple:
        raise ValueError("bounded_sequence_required")
    if len(journal) > 400:
        raise ValueError("revision_bound")
    rows, counts = [], {}
    try:
        for raw in journal:
            if not isinstance(raw, str) or len(raw) > MAX_ENTRY_CHARS:
                raise ValueError("entry_size_bound")
            row = json.loads(raw)
            _validate(row["input"])
            event_id = _id("event:", _identity(row["input"]))
            expected = _row(row["input"], counts.get(event_id, 0))
            if _encode(row) != _encode(expected):
                raise ValueError("journal_revision_mismatch")
            counts[event_id] = expected["revision"] + 1
            if len(counts) > 100:
                raise ValueError("event_bound")
            rows.append(row)
    except (ValueError, TypeError, KeyError) as exc:
        raise ValueError("invalid_journal: " + str(exc)) from exc
    return rows


def append_revisions(journal, updates, *, max_events=100, max_revisions=400):
    """Return new tuple of JSON strings; never mutate historical entries."""
    if (type(max_events) is not int or not 1 <= max_events <= 100 or
            type(max_revisions) is not int or not 1 <= max_revisions <= 400):
        raise ValueError("invalid_bounds")
    if type(journal) is not tuple or type(updates) not in (list, tuple):
        raise ValueError("bounded_sequence_required")
    if len(journal) + len(updates) > max_revisions:
        raise ValueError("revision_bound")
    old_rows = _read_journal(journal)
    identities = {row["event_id"] for row in old_rows}
    try:
        identities.update(_id("event:", _identity(item)) for item in updates)
    except (KeyError, TypeError) as exc:
        raise ValueError("invalid_identity") from exc
    if len(identities) > max_events:
        raise ValueError("event_bound")
    for item in updates:
        _size_preflight(item)
        _validate(item)
    result = list(journal)
    counts = {}
    for row in old_rows:
        counts[row["event_id"]] = counts.get(row["event_id"], 0) + 1
    for item in updates:
        event_id = _id("event:", _identity(item))
        row = _row(item, counts.get(event_id, 0))
        counts[event_id] = row["revision"] + 1
        result.append(_encode(row))
    return tuple(result)


def summarize(journal):
    """Order-independent set reconciliation; no candidate is last-write-wins.

    Duplicate raw = repeated exact input assertion at the same source locator;
    conflicting raw = extra distinct assertions at that locator. Logical payload
    counts exclude exact-input repeats and require caller full-payload digests.
    """
    rows = _read_journal(journal)
    locators = {}
    events = {}
    evidence_rows = []
    for row in rows:
        locators.setdefault(row["raw_delivery_id"], set()).add(_encode(row["input"]))
        if row["input"]["kind"] == "event":
            events.setdefault(row["event_id"], []).append(row)
        else:
            evidence_rows.append(row)
    counts = dict(duplicate_raw_delivery=len(rows) - sum(map(len, locators.values())),
                  conflicting_raw_delivery=sum(len(v) - 1 for v in locators.values()),
                  repeated_logical_payload=0, conflicting_logical_payload=0,
                  payload_unverified_deliveries=0)
    states = []
    for event_id, group in sorted(events.items()):
        distinct = {_encode(row["input"]): row for row in group}.values()
        digests = [row["input"]["payload_sha256"] for row in distinct
                   if row["input"]["payload_sha256"] is not None]
        candidates = sorted(set(digests))
        missing = sum(row["input"]["payload_sha256"] is None for row in distinct)
        counts["payload_unverified_deliveries"] += missing
        counts["repeated_logical_payload"] += len(digests) - len(candidates)
        counts["conflicting_logical_payload"] += max(0, len(candidates) - 1)
        conflict = len(candidates) > 1 or any(len(locators[r["raw_delivery_id"]]) > 1 for r in group)
        identity = group[0]["identity"]
        # A bare slot number cannot prove membership in a particular bank/fork.
        proofs = [r for r in evidence_rows if identity[4] is not None
                  and r["identity"][:2] == identity[:2]
                  and r["identity"][3:5] == identity[3:5]
                  and r["input"]["evidence_authority"] == "slot_notification"]
        finalized = sorted({r["assertion_id"] for r in proofs
                            if r["input"]["projection"]["slot_status"] == "Finalized"})
        superseded = sorted({r["assertion_id"] for r in proofs
                             if r["input"]["projection"]["slot_status"] in ("Dead", "Reorg")})
        evidence_conflict = bool(finalized and superseded) or any(
            len(locators[r["raw_delivery_id"]]) > 1 for r in proofs)
        finalized_forks = {r["identity"][4] for r in evidence_rows
                           if r["identity"][:2] == identity[:2]
                           and r["identity"][3] == identity[3]
                           and r["identity"][4] is not None
                           and r["input"]["evidence_authority"] == "slot_notification"
                           and r["input"]["projection"]["slot_status"] == "Finalized"}
        evidence_conflict = evidence_conflict or len(finalized_forks) > 1
        finality = "finalized" if finalized and not superseded and not evidence_conflict and not conflict else None
        reason = (None if finality else "conflicting_slot_evidence" if evidence_conflict
                  else "conflicting_event_evidence" if conflict else "superseded_slot" if superseded
                  else "fork_membership_unknown" if identity[4] is None
                  else "no_matching_finalized_slot_evidence")
        states.append(dict(event_id=event_id, identity=group[0]["identity"],
                           later_finality=finality, finality_null_reason=reason,
                           finality_evidence_ids=finalized, superseded_by=superseded,
                           correction_available_at=None, correction_clock="UNKNOWN",
                           observed_commitments=sorted({r["input"]["projection"]["source_commitment"]
                                                        for r in group}, key=_encode),
                           payload_candidates=candidates,
                           resolved_payload_sha256=candidates[0] if candidates and not missing and not conflict else None,
                           payload_null_reason="conflicting_evidence" if conflict else
                           "full_payload_digest_not_supplied" if missing else None,
                           status="conflicted" if conflict or evidence_conflict else "superseded" if superseded else "observed"))
    return dict(events=states, counts=counts, training_eligible=False,
                source_admission="unadmitted")
