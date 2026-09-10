"""Bounded development-only observed episodes; never recommendation targets."""
from copy import deepcopy
import re
import hashlib
import json
import os
from pathlib import Path

from north_star.actions import validate_action

from north_star.transaction_truth import _base58_size


ACTION_FIELDS = frozenset('''schema_version action_id event_id parent_ids wallet mint
signature chain venue origin recommended canonical_identity_status executed_venue_fill
instruction_layout_status transaction_status finality action decimals token_unit cash_unit
inventory_before_raw inventory_after_raw quantity_raw consideration_lamports
embedded_venue_fee_lamports additional_fee_lamports fee_scope_id fee_semantics slot
transaction_index instruction_index decision_cutoff_unix_ms event_at_unix_ms
available_at_unix_ms evidence_refs'''.split())


def _require(condition, reason):
    if not condition:
        raise ValueError(reason)


def _validate(row):
    _require(isinstance(row, dict), 'invalid_action')
    _require(row.get('canonical_identity_status') != 'quarantined', 'identity_quarantined')
    _require(row.get('schema_version') == 'proven_executed_venue_action_v1', 'not_proven_venue_action')
    _require(row.get('canonical_identity_status') == 'verified' and
             row.get('executed_venue_fill') is True and
             row.get('instruction_layout_status') == 'verified' and
             row.get('transaction_status') == 'confirmed' and row.get('finality') == 'finalized',
             'unverified_execution')
    _require(row.get('origin') == 'observed_action' and row.get('recommended') is False,
             'not_observed_action')
    _require(not (set(row) - ACTION_FIELDS), 'unexpected_action_fields')
    for name, size in (('wallet', 32), ('mint', 32), ('signature', 64)):
        _require(_base58_size(row.get(name), size), 'invalid_' + name)
    _require(row.get('chain') == 'solana' and row.get('venue') in ('pumpfun', 'pumpswap'),
             'unsupported_scope')

    for name in ('inventory_before_raw', 'inventory_after_raw', 'quantity_raw', 'decimals',
                 'consideration_lamports', 'embedded_venue_fee_lamports', 'additional_fee_lamports',
                 'slot', 'transaction_index', 'instruction_index', 'event_at_unix_ms',
                 'available_at_unix_ms', 'decision_cutoff_unix_ms'):
        value = row.get(name)
        _require(type(value) is int and 0 <= value < 2**64, 'invalid_' + name)
    _require(row['decimals'] <= 255, 'invalid_decimals')
    _require(row.get('token_unit') == 'raw' and row.get('cash_unit') == 'lamports', 'invalid_units')
    for name in ('action_id', 'event_id', 'fee_scope_id'):
        _text(row.get(name), 'invalid_' + name)
    _parents(row.get('parent_ids'), 'invalid_parent_ids')
    kind = row.get('action')
    _require(isinstance(kind, str) and kind in ('BUY', 'ADD', 'REDUCE', 'EXIT_ALL'), 'not_executed_trade')
    before, after, quantity = (row[k] for k in ('inventory_before_raw', 'inventory_after_raw', 'quantity_raw'))
    _require(quantity > 0 and abs(after - before) == quantity, 'inventory_quantity_mismatch')
    _require((kind == 'BUY' and before == 0 and after > 0) or
             (kind == 'ADD' and after > before > 0) or
             (kind == 'REDUCE' and before > after > 0) or
             (kind == 'EXIT_ALL' and before > 0 and after == 0), 'action_inventory_mismatch')
    validate_action(dict(action=kind, origin=row['origin'], chain=row['chain'], venue=row['venue'],
                         evidence_id=row['event_id'], position_raw=before, sell_raw=quantity,
                         recommended=False))
    _require(row.get('fee_semantics') == 'consideration_includes_venue_fee_additional_fee_excludes_it',
             'invalid_fee_semantics')
    _require(row['embedded_venue_fee_lamports'] <= row['consideration_lamports'], 'invalid_embedded_fee')
    _require(row['event_at_unix_ms'] <= row['available_at_unix_ms'], 'invalid_event_availability')
    _require(row['decision_cutoff_unix_ms'] <= row['event_at_unix_ms'], 'invalid_decision_cutoff')
    refs = row.get('evidence_refs')
    _require(isinstance(refs, list) and 1 <= len(refs) <= 128, 'missing_evidence_roles')
    roles, ids = set(), set()
    for ref in refs:
        _require(isinstance(ref, dict), 'invalid_evidence')
        _require(not (set(ref) - {'evidence_id', 'role', 'action_id', 'source_sha256',
                                'parent_ids', 'available_at_unix_ms'}), 'unexpected_evidence_fields')
        _text(ref.get('evidence_id'), 'invalid_evidence_id')
        _require(ref['evidence_id'] not in ids, 'duplicate_evidence_id')
        ids.add(ref['evidence_id'])
        role = ref.get('role')
        _require(isinstance(role, str) and role in EVIDENCE_ROLES, 'invalid_evidence_role')
        roles.add(role)
        _require(ref.get('action_id') == row['action_id'], 'unbound_evidence')
        digest = ref.get('source_sha256')
        _require(isinstance(digest, str) and re.fullmatch('[0-9a-f]{64}', digest) is not None,
                 'invalid_evidence_source_sha256')
        _parents(ref.get('parent_ids'), 'invalid_evidence_parent_ids')
        available = ref.get('available_at_unix_ms')
        _require(type(available) is int and 0 <= available < 2**64, 'invalid_evidence_availability')
        if role in ('causal', 'opening_inventory'):
            _require(available <= row['decision_cutoff_unix_ms'], 'postdecision_' + role + '_evidence')
        _require(available <= row['available_at_unix_ms'], 'evidence_after_action_availability')
    _require(roles == EVIDENCE_ROLES, 'missing_evidence_roles')


EVIDENCE_ROLES = frozenset(('causal', 'execution', 'identity', 'layout', 'inventory', 'fees', 'ordering', 'opening_inventory'))


def _text(value, reason):
    _require(isinstance(value, str) and 0 < len(value) <= 2048 and value.strip() == value, reason)


def _parents(value, reason):
    _require(isinstance(value, list) and 1 <= len(value) <= 128, reason)
    for parent in value:
        _text(parent, reason)
    _require(len(value) == len(set(value)), reason)


def assemble_observed_episodes(actions, *, cutoff_unix_ms, observation_end="open", max_actions=1000):
    """Fail atomically on any bad row; never sort, repair or drop hidden gaps.

    The caller supplies independently proven actions, not raw instructions. Proof
    references are a structural contract, not a verifier or admission decision.
    Causal refs precede each decision; execution refs remain outcome evidence.
    Cash is signed observed flow, NOT realized PnL or inferred opening cost basis.
    """
    _require(type(cutoff_unix_ms) is int and 0 <= cutoff_unix_ms < 2**64, 'invalid_cutoff')
    _require(observation_end in ('open', 'censored'), 'invalid_observation_end')
    _require(type(max_actions) is int and 1 <= max_actions <= 10000, 'invalid_max_actions')
    episodes, active, inventory, decimals, ordering, clocks = [], {}, {}, {}, {}, {}
    ids, fees = set(), set()
    for index, source in enumerate(actions):
        _require(index < max_actions, 'action_bound_exceeded')
        _validate(source)
        row = deepcopy(source)
        _require(row['available_at_unix_ms'] <= cutoff_unix_ms, 'action_after_cutoff')
        _require(row['action_id'] not in ids, 'duplicate_action_id')
        _require(row['fee_scope_id'] not in fees, 'duplicate_fee_scope_id')
        ids.add(row['action_id'])
        fees.add(row['fee_scope_id'])
        wallet, mint = row['wallet'], row['mint']
        key = wallet, mint
        order = tuple(row[k] for k in ('slot', 'transaction_index', 'instruction_index'))
        _require(wallet not in ordering or order > ordering[wallet], 'nonchronological_wallet_actions')
        _require(wallet not in clocks or row['event_at_unix_ms'] >= clocks[wallet], 'nonchronological_wallet_events')
        _require(mint not in decimals or decimals[mint] == row['decimals'], 'inconsistent_mint_decimals')
        _require(key not in inventory or inventory[key] == row['inventory_before_raw'], 'inventory_discontinuity')
        ordering[wallet], clocks[wallet] = order, row['event_at_unix_ms']
        inventory[key], decimals[mint] = row['inventory_after_raw'], row['decimals']
        if key not in active:
            episode = {
                "schema_version": "observed_episode_v1", "state": "open",
                "episode_id": 'observed_episode:' + hashlib.sha256(_encode(
                    [wallet, mint, row['action_id']])).hexdigest(),
                "origin": "observed_action", "action_ids": [], "event_ids": [], "evidence_ids": [],
                "wallet": row["wallet"], "mint": row["mint"],
                "actions": [], "parent_ids": [], "cash_delta_lamports": 0,
                "opening_inventory_raw": row['inventory_before_raw'],
                "left_censored": row['inventory_before_raw'] > 0, "right_censored": False,
                "cutoff_unix_ms": cutoff_unix_ms, "decimals": row['decimals'],
                "token_unit": "raw", "cash_unit": "lamports",
                "training_eligible": False, "imitation_eligible": False,
            }
            active[key] = episode
            episodes.append(episode)
        episode = active[key]
        parents = set(episode["parent_ids"]) | set(row["parent_ids"])
        for ref in row["evidence_refs"]:
            parents.update(ref["parent_ids"])
        episode["parent_ids"] = sorted(parents)
        episode["actions"].append(row)
        episode['action_ids'].append(row['action_id'])
        episode['event_ids'].append(row['event_id'])
        episode['evidence_ids'] = sorted(set(episode['evidence_ids']) |
                                          {ref['evidence_id'] for ref in row['evidence_refs']})
        episode["remaining_inventory_raw"] = row["inventory_after_raw"]
        sign = -1 if row["action"] in ("BUY", "ADD") else 1
        episode["cash_delta_lamports"] += sign * row["consideration_lamports"] - row["additional_fee_lamports"]
        if row["inventory_after_raw"] == 0:
            episode["state"] = "closed"
            del active[key]
    for episode in episodes:
        episode['right_censored'] = episode['state'] != 'closed' and observation_end == 'censored'
        if episode['left_censored'] or episode['right_censored']:
            episode['state'] = 'censored'
    return episodes



def _read_bounded(path, limit):
    with path.open('rb') as stream:
        data = stream.read(limit + 1)
    _require(len(data) <= limit, 'report_bound_exceeded')
    return data


def _encode(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False) + '\n').encode('utf-8')


def audit_transaction_truth_report(source_dir, output_dir):
    """Rejection-only diagnostic over a bounded immutable first-ten report.

    Reads ONLY receipt/reconciled/rejected, never the capture paths referenced by
    them. Does not reinterpret balances or repair identities. A report containing
    action-schema rows is rejected, not opportunistically converted into trades.
    New directory/exclusive files only; receipt written last is completion marker.
    Files are hash-manifested, read-back verified and read-only (not OS WORM).
    """
    source, output = Path(source_dir).resolve(), Path(output_dir).resolve()
    if output.exists():
        raise FileExistsError(output)
    _require(source != output and source not in output.parents, 'output_inside_source')
    raw = {name: _read_bounded(source / name, 4 * 1024 * 1024)
           for name in ('receipt.json', 'reconciled.ndjson', 'rejected.ndjson')}
    receipt = json.loads(raw['receipt.json'])
    _require(isinstance(receipt, dict) and
             receipt.get('schema_version') == 'transaction_truth_sample_receipt_v1', 'invalid_report_schema')
    for field in ('transactions_attempted', 'max_transactions', 'reconciled_count',
                  'rejected_count', 'canonical_identity_quarantined_count'):
        _require(type(receipt.get(field)) is int and receipt[field] >= 0, 'invalid_report_count')
    _require(receipt['transactions_attempted'] <= 10 and receipt['max_transactions'] <= 10,
             'report_bound_exceeded')
    _require(receipt.get('training_eligible') is False, 'invalid_report_admission')
    rows = {}
    for name in ('reconciled.ndjson', 'rejected.ndjson'):
        _require(hashlib.sha256(raw[name]).hexdigest() == receipt.get('output_sha256', {}).get(name),
                 'report_hash_mismatch')
        lines = raw[name].splitlines()
        _require(len(lines) <= 10, 'report_bound_exceeded')
        rows[name] = [json.loads(line) for line in lines]
        _require(all(isinstance(row, dict) for row in rows[name]), 'invalid_report_row')
    reconciled, rejected = rows['reconciled.ndjson'], rows['rejected.ndjson']
    _require(len(reconciled) == receipt['reconciled_count'] and
             len(rejected) == receipt['rejected_count'] and
             len(reconciled) + len(rejected) == receipt['transactions_attempted'] and
             receipt['transactions_attempted'] == receipt['max_transactions'] == 10,
             'report_count_mismatch')
    _require(sum(r.get('canonical_identity_status') == 'quarantined' for r in reconciled) ==
             receipt['canonical_identity_quarantined_count'], 'report_count_mismatch')
    indices = [r.get('record_index') for r in reconciled + rejected]
    selected = receipt.get('selected_record_indices')
    _require(all(type(i) is int and i >= 0 for i in indices) and len(set(indices)) == 10 and
             isinstance(selected, list) and all(type(i) is int for i in selected) and
             sorted(indices) == sorted(selected), 'report_record_index_mismatch')
    audit = []
    for number, row in enumerate(reconciled, 1):
        _require(row.get('schema_version') == 'transaction_balance_reconciliation_v1', 'invalid_report_row_schema')
        try:
            assemble_observed_episodes([row], cutoff_unix_ms=2**64 - 1)
        except ValueError as exc:
            reason = str(exc)
        else:
            raise ValueError('unexpected_endpoint_admission')
        audit.append(dict(status='refused_episode_construction', reason=reason,
                          report_file='reconciled.ndjson', report_line=number,
                          original_record=row, training_eligible=False))
    for number, row in enumerate(rejected, 1):
        _text(row.get('reason'), 'invalid_upstream_rejection')
        audit.append(dict(status='upstream_rejection_retained', reason=row['reason'],
                          report_file='rejected.ndjson', report_line=number,
                          original_record=row, training_eligible=False))
    payloads = {'rejection_audit.ndjson': b''.join(_encode(row) for row in audit),
                'upstream_rejected.ndjson': raw['rejected.ndjson'], 'episodes.ndjson': b''}
    summary = dict(schema_version='observed_episode_rejection_audit_v1', source_dir=str(source),
                   source_hashes={name: hashlib.sha256(data).hexdigest() for name, data in raw.items()},
                   output_hashes={name: hashlib.sha256(data).hexdigest() for name, data in payloads.items()},
                   valid_episode_count=0, refused_episode_construction_count=len(reconciled),
                   upstream_rejection_count=len(rejected), attempted_transaction_count=len(audit),
                   training_eligible=False, imitation_eligible=False, stage5_complete=False,
                   scope='development_rejection_only_no_admission')
    # Detect source edits before publication, without reading any referenced raw capture.
    for name, data in raw.items():
        _require(_read_bounded(source / name, 4 * 1024 * 1024) == data, 'source_changed_during_audit')
    output.mkdir(parents=True, exist_ok=False)
    payloads['receipt.json'] = _encode(summary)
    for name, data in payloads.items():
        target = output / name
        with target.open('xb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        _require(target.read_bytes() == data, 'output_readback_mismatch')
        target.chmod(0o444)
    return summary
