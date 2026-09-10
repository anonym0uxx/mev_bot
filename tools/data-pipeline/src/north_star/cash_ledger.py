"""Pure offline native-cash reference mechanics, never an economics admission gate.

Separate from accounting.Ledger's consideration delta and FIFO PnL. No bankroll,
RPC, classification inference, finality proof, valuation or execution lives here.

One instance has one caller-defined chain/accounting scope and strictly increasing
transaction sequence. Openings are checkpoints immediately before a wallet's
first transaction; later reorg/reclassification requires replay into a new ledger.
Evidence strings are mandatory locators, NOT independently verified evidence.

All inputs are exact u64 lamports (not SOL); signed cumulative flows use Python
integers. Endpoints are witnessed post-transaction native wallet balances. Do not
feed wrapped-native token balances or model fills as wallet native movements.
TRADE_* amounts exclude recoverable rent and include embedded venue costs. With
fee.overlap='overlap' exactly one named same-payer trade amount includes the entire
transaction fee: debit includes it, credit is net of it. Only that transaction fee
is backed out of trade consideration. Partial/multiple/unknown fee overlap is not
supported. Nonoverlap adds the fee debit separately, including explicit zero.

Unknown endpoints/movements never become income, external flow or trade profit.
Cash snapshots follow observed endpoints, with unexplained differences retained in
suspense and permanent unresolved reasons. A later offsetting delta cannot cure
an unresolved history. Unknown opening is never backsolved from a later endpoint.
No realized PnL field is offered; even reconciled synthetic mechanics always have
economics_eligible=False and training_eligible=False.
"""
from dataclasses import dataclass, replace

U64_MAX = 2**64 - 1
_DEBITS = ("EXTERNAL_WITHDRAWAL", "TRADE_DEBIT", "UNKNOWN_DEBIT", "RENT_DEPOSIT")
_FLOW_FIELDS = {
    "EXTERNAL_DEPOSIT": "external_flow_lamports",
    "EXTERNAL_WITHDRAWAL": "external_flow_lamports",
    "TRADE_DEBIT": "trade_consideration_lamports",
    "TRADE_CREDIT": "trade_consideration_lamports",
    "NONTRADING_INCOME": "nontrading_income_lamports",
}
_KINDS = tuple(_FLOW_FIELDS) + ("RENT_DEPOSIT", "RENT_REFUND", "UNKNOWN_DEBIT", "UNKNOWN_CREDIT")


def _native(value, name):
    if type(value) is not int or not 0 <= value <= U64_MAX:
        raise ValueError(f"{name} must be an exact nonnegative u64 integer")


def _identity(value, name):
    if type(value) is not str or not value.strip():
        raise ValueError(f"{name} must be nonempty text")


@dataclass(frozen=True)
class WalletCash:
    wallet: str
    opening_lamports: int | None
    cash_lamports: int | None
    opening_evidence: str
    unresolved_reasons: tuple[str, ...] = ()
    external_flow_lamports: int = 0
    trade_consideration_lamports: int = 0
    fee_expense_lamports: int = 0
    nontrading_income_lamports: int = 0
    recoverable_rent_lamports: int = 0
    suspense_lamports: int | None = 0

    @property
    def reconciled(self):
        return self.cash_lamports is not None and not self.unresolved_reasons

    @property
    def economics_eligible(self):
        return False

    @property
    def training_eligible(self):
        return False


@dataclass(frozen=True)
class Movement:
    action_id: str
    wallet: str
    kind: str
    lamports: int
    evidence: str
    unknown_reason: str | None = None
    rent_id: str | None = None


@dataclass(frozen=True)
class TransactionFee:
    lamports: int
    payer: str
    evidence: str
    overlap: str
    embedded_action_id: str | None = None


@dataclass(frozen=True)
class CashEndpoint:
    wallet: str
    lamports: int
    evidence: str


@dataclass(frozen=True)
class CashTransaction:
    tx_id: str
    sequence: int
    status: str
    actions: tuple[Movement, ...]
    fee: TransactionFee
    endpoints: tuple[CashEndpoint, ...]


@dataclass(frozen=True)
class Reconciliation:
    wallet: str
    expected_lamports: int | None
    observed_lamports: int
    unexplained_delta_lamports: int | None


@dataclass(frozen=True)
class CashReceipt:
    transaction: CashTransaction
    reconciliations: tuple[Reconciliation, ...]
    reconciled: bool

    @property
    def economics_eligible(self):
        return False

    @property
    def training_eligible(self):
        return False


def _copy_transaction(tx):
    """Copy the validated record graph; leaves are exact immutable primitives."""
    return replace(tx, actions=tuple(replace(action) for action in tx.actions),
                   fee=replace(tx.fee),
                   endpoints=tuple(replace(endpoint) for endpoint in tx.endpoints))


def _copy_receipt(receipt):
    """Detach every record, including nested transaction witnesses and comparisons."""
    return replace(receipt, transaction=_copy_transaction(receipt.transaction),
                   reconciliations=tuple(replace(row) for row in receipt.reconciliations))


class CashLedger:
    """Small in-memory reference, not persistent or thread-safe portfolio state.

    Public inputs/outputs never share record objects with retained state. Frozen
    dataclasses are only an edit guard: isolation comes from copying each record
    at ingress/egress, sharing only validated immutable primitive leaves. This is
    for single-threaded trusted callers, not protection against private-state
    access, monkeypatching, or concurrent mutation during validation/copying.
    """
    def __init__(self):
        self._wallets = {}
        self._receipts = ()
        self._rent = {}
        self._seen_transactions = set()
        self._last_sequence = -1

    @property
    def receipts(self):
        """Detached accepted witnesses and comparisons, fresh on every read."""
        return tuple(_copy_receipt(receipt) for receipt in self._receipts)

    def open_wallet(self, wallet, lamports, *, evidence, unknown_reason=None):
        _identity(wallet, "wallet")
        _identity(evidence, "opening evidence")
        if wallet in self._wallets:
            raise ValueError("opening must precede all wallet activity")
        if lamports is None:
            _identity(unknown_reason, "unknown opening reason")
        else:
            _native(lamports, "opening lamports")
            if unknown_reason is not None:
                raise ValueError("known opening cannot have unknown reason")
        reasons = () if unknown_reason is None else (unknown_reason,)
        self._wallets[wallet] = WalletCash(wallet, lamports, lamports, evidence, reasons,
                                           suspense_lamports=None if lamports is None else 0)

    def wallet(self, wallet):
        """Detached snapshot; frozen fields discourage edits, not enforce isolation."""
        return replace(self._wallets[wallet])

    def apply(self, tx):
        """Post atomically or raise ValueError; unexplained valid endpoints post to suspense.

        Failed transactions are stricter: only a fee-only debit with known prior
        cash and exact endpoint agreement is accepted. No attempted actions post.
        """
        self._validate(tx)
        tx = _copy_transaction(tx)
        wallets = dict(self._wallets)
        rent = dict(self._rent)
        delta = {endpoint.wallet: 0 for endpoint in tx.endpoints}
        for action in tx.actions:
            state = wallets[action.wallet]
            signed = action.lamports * (-1 if action.kind in _DEBITS else 1)
            if action.kind.startswith("UNKNOWN_"):
                suspense = None if state.suspense_lamports is None else state.suspense_lamports + signed
                state = replace(state, suspense_lamports=suspense,
                                unresolved_reasons=state.unresolved_reasons + (action.unknown_reason,))
            elif action.kind.startswith("RENT_"):
                key = (action.wallet, action.rent_id)
                rent[key] = rent.get(key, 0) - signed
                if rent[key] < 0:
                    raise ValueError("rent refund exceeds witnessed same-wallet deposit")
                state = replace(state, recoverable_rent_lamports=state.recoverable_rent_lamports - signed)
            else:
                field = _FLOW_FIELDS[action.kind]
                consideration = signed
                if tx.fee.overlap == "overlap" and action.action_id == tx.fee.embedded_action_id:
                    consideration += tx.fee.lamports
                state = replace(state, **{field: getattr(state, field) + consideration})
            wallets[action.wallet] = state
            delta[action.wallet] += signed
        payer = wallets[tx.fee.payer]
        wallets[tx.fee.payer] = replace(payer, fee_expense_lamports=
                                       payer.fee_expense_lamports + tx.fee.lamports)
        if tx.fee.overlap == "nonoverlap":
            delta[tx.fee.payer] -= tx.fee.lamports
        reconciliations = []
        for endpoint in tx.endpoints:
            state = wallets[endpoint.wallet]
            expected = None if state.cash_lamports is None else state.cash_lamports + delta[endpoint.wallet]
            if expected is not None:
                _native(expected, "derived cash endpoint")
            residual = None if expected is None else endpoint.lamports - expected
            if tx.status == "failed" and residual != 0:
                raise ValueError("failed transaction requires exactly witnessed fee debit")
            reconciliations.append(Reconciliation(endpoint.wallet, expected,
                                                   endpoint.lamports, residual))
            if residual is None or residual != 0:
                suspense = (None if residual is None or state.suspense_lamports is None
                            else state.suspense_lamports + residual)
                state = replace(state, suspense_lamports=suspense,
                                unresolved_reasons=state.unresolved_reasons +
                                (f"{tx.tx_id}: unexplained endpoint difference {residual}",))
            wallets[endpoint.wallet] = replace(state, cash_lamports=endpoint.lamports)
        receipt = CashReceipt(tx, tuple(reconciliations), all(
            wallets[row.wallet].reconciled for row in reconciliations))
        result = _copy_receipt(receipt)
        self._wallets = wallets
        self._rent = rent
        self._receipts += (receipt,)
        self._seen_transactions.add(tx.tx_id)
        self._last_sequence = tx.sequence
        return result

    def _validate(self, tx):
        if type(tx) is not CashTransaction:
            raise ValueError("expected CashTransaction")
        _identity(tx.tx_id, "transaction identity")
        _native(tx.sequence, "sequence")
        if tx.tx_id in self._seen_transactions or tx.sequence <= self._last_sequence:
            raise ValueError("duplicate transaction or nonincreasing sequence; replay required")
        if type(tx.status) is not str or tx.status not in ("confirmed", "finalized", "failed"):
            raise ValueError("unsupported transaction status")
        if type(tx.actions) is not tuple or type(tx.endpoints) is not tuple or not tx.endpoints:
            raise ValueError("immutable actions and nonempty endpoints required")
        if type(tx.fee) is not TransactionFee:
            raise ValueError("explicit transaction fee witness required, including zero")
        fee = tx.fee
        _native(fee.lamports, "fee lamports")
        _identity(fee.payer, "fee payer")
        _identity(fee.evidence, "fee evidence")
        if type(fee.overlap) is not str or fee.overlap not in ("overlap", "nonoverlap"):
            raise ValueError("fee overlap/nonoverlap must be explicit")
        endpoints = set()
        for endpoint in tx.endpoints:
            if type(endpoint) is not CashEndpoint:
                raise ValueError("expected CashEndpoint")
            _identity(endpoint.wallet, "endpoint wallet")
            _identity(endpoint.evidence, "endpoint evidence")
            _native(endpoint.lamports, "endpoint lamports")
            if endpoint.wallet in endpoints or endpoint.wallet not in self._wallets:
                raise ValueError("duplicate or unopened endpoint wallet")
            endpoints.add(endpoint.wallet)
        if fee.payer not in endpoints:
            raise ValueError("fee payer endpoint is required")
        actions = {}

        for action in tx.actions:
            if type(action) is not Movement:
                raise ValueError("expected Movement")
            _identity(action.action_id, "action identity")
            _identity(action.wallet, "movement wallet")
            _identity(action.evidence, "movement evidence")
            _native(action.lamports, "movement lamports")
            if action.action_id in actions or action.wallet not in endpoints:
                raise ValueError("duplicate action identity or missing wallet endpoint")
            if type(action.kind) is not str or action.kind not in _KINDS:
                raise ValueError("unsupported explicit movement classification")
            if action.kind.startswith("UNKNOWN_"):
                _identity(action.unknown_reason, "unknown movement reason")
            elif action.unknown_reason is not None:
                raise ValueError("known category cannot have unknown reason")
            if action.kind.startswith("RENT_"):
                _identity(action.rent_id, "rent account identity")
            elif action.rent_id is not None:
                raise ValueError("rent identity on nonrent movement")
            actions[action.action_id] = action
        if fee.overlap == "nonoverlap":
            if fee.embedded_action_id is not None:
                raise ValueError("nonoverlap cannot reference an embedded fee")
        else:
            _identity(fee.embedded_action_id, "embedded fee action")
            action = actions.get(fee.embedded_action_id)
            if (action is None or action.wallet != fee.payer or
                    action.kind not in ("TRADE_DEBIT", "TRADE_CREDIT")):
                raise ValueError("overlap requires exactly one same-payer trade witness")
            if action.kind == "TRADE_DEBIT" and action.lamports < fee.lamports:
                raise ValueError("embedded buy fee exceeds witnessed gross debit")
            if action.kind == "TRADE_CREDIT":
                _native(action.lamports + fee.lamports, "gross sale consideration")
        if tx.status == "failed" and (tx.actions or fee.overlap != "nonoverlap" or endpoints != {fee.payer}):
            raise ValueError("failed transaction admits fee payer debit only, no actions")
