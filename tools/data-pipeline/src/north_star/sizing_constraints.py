"""Pure offline per-order entry sizing checks, never order authorization.

Pinned to the operator update identified in each result. No file/network I/O,
configuration changes, or Rust exit changes occur here. A passing size check
is not full Stage 1 closure or an economic gate. The cumulative position cap
is unspecified; ADD semantics are not approved. This is not an order-splitting
policy. A cap lift requires an explicit operator change and reviewed repinning.
"""

MAX_ENTRY_LAMPORTS = 250000000
SOURCE_CONTRACT_REF = "north_star/OPERATOR_SCOPE_UPDATE.json"
SOURCE_CONTRACT_SHA256 = "ca1632456b13c93d398a4043a82c5a26d004ffdc1c3c4a15747f08da6dcdff27"


def evaluate_entry_size(size_lamports=None, *, available_cash_lamports=None,
                        pending_reservations_lamports=None):
    """Return sizing-only evidence, with unknown operating values left unknown.

    Amounts must be exact native nonnegative ints, never coerced. Zero passes
    this upper-bound predicate only; it is not an executable order. Optional
    None means unknown, not zero. Cash is a caller-supplied pre-reservation
    snapshot; pending is the total still to deduct from that same snapshot.
    Do not pass already-netted cash alongside the same reservations. With only
    cash known, check that upper bound but leave net spendable cash unknown.
    Pending without cash cannot establish affordability. Fees, freshness, ADD,
    cumulative exposure, and all other execution gates remain out of scope.
    """
    reasons = []
    if type(size_lamports) is not int or size_lamports < 0:
        reasons.append("entry_size_requires_native_nonnegative_integer")
    elif size_lamports > MAX_ENTRY_LAMPORTS:
        reasons.append("entry_size_exceeds_per_order_cap")
    for field, value in (("available_cash_lamports", available_cash_lamports),
                         ("pending_reservations_lamports", pending_reservations_lamports)):
        if value is not None and (type(value) is not int or value < 0):
            reasons.append(field + "_requires_native_nonnegative_integer")
    cash_checked = type(available_cash_lamports) is int and available_cash_lamports >= 0
    if (cash_checked and type(size_lamports) is int
            and size_lamports >= 0 and size_lamports > available_cash_lamports):
        reasons.append("entry_size_exceeds_known_available_cash")
    pending_checked = (cash_checked and type(pending_reservations_lamports) is int
                       and pending_reservations_lamports >= 0)
    spendable = None
    if pending_checked:
        spendable = max(0, available_cash_lamports - pending_reservations_lamports)
        if pending_reservations_lamports > available_cash_lamports:
            reasons.append("pending_reservations_exceed_known_available_cash")
        elif type(size_lamports) is int and size_lamports >= 0 and size_lamports > spendable:
            reasons.append("entry_size_exceeds_cash_after_pending_reservations")
    return {
        "size_constraints_satisfied": not reasons,
        "reasons": reasons,
        "max_entry_lamports": MAX_ENTRY_LAMPORTS,
        "cap_scope": "per_order_entry",
        "cap_lift_requires_explicit_operator_change": True,
        "cumulative_position_cap_lamports": None,
        "add_semantics_approved": False,
        "live_orders_authorized": False,
        "full_operating_contract_approved": False,
        "selected_bankroll_lamports": None,
        "maximum_drawdown": None,
        "credits_remaining": None,
        "cash_bound_checked": cash_checked,
        "pending_reservations_accounted_for": pending_checked,
        "spendable_cash_lamports": spendable,
        "source_contract_ref": SOURCE_CONTRACT_REF,
        "source_contract_sha256": SOURCE_CONTRACT_SHA256,
    }
