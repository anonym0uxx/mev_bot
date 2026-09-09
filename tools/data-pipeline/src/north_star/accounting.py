"""Small, deterministic FIFO reference ledger; no execution or data admission.

Inputs are explicitly ordered, confirmed actions, not inferred from balance deltas.
One instance is one chain/accounting scope; wallets stay separate. Mint identities
must already be canonical. No venue/finality/ownership evidence is inferred here.

Trade lamports are observed consideration INCLUDING embedded venue fees. The
transaction fee is the independently known additional nonrecoverable cash expense
(including any separately paid priority fee/tip), charged once and expensed at
transaction time, never duplicated in lot basis. Per-sale PnL excludes this separate
expense; ledger realized PnL includes it. No implicit slippage or recoverable rent.
Missing consideration/fee components must be quarantined by the caller, not zeroed.

Cumulative-floor allocation assigns integer residuals deterministically. Original
lot quantity intervals survive partial sales/transfers so splitting cannot lose or
reassign cost. Python integers do not overflow; input native amounts are u64.
This is not a persistence layer, portfolio valuation, tax report, or admission gate.
"""

from dataclasses import dataclass, replace

U64_MAX = 2**64 - 1


def _native(value, name, *, positive=False):
    if type(value) is not int or not (int(positive) <= value <= U64_MAX):
        raise ValueError(f"{name} must be an exact {'positive ' if positive else ''}u64 integer")


def _identity(value, name):
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be nonempty text")


@dataclass(frozen=True)
class Action:
    action_id: str
    kind: str
    wallet: str
    mint: str
    decimals: int
    raw_quantity: int
    lamports: int | None
    destination: str | None = None
    owner_evidence: str | None = None
    unknown_cost_reason: str | None = None


@dataclass(frozen=True)
class Transaction:
    tx_id: str
    sequence: int
    status: str
    fee_lamports: int
    actions: tuple[Action, ...]


@dataclass(frozen=True)
class Lot:
    origin_id: tuple[str, str]
    origin_order: tuple[int, int]
    original_quantity: int
    original_cost_lamports: int | None
    start: int
    stop: int
    unknown_cost_reason: str | None

    @property
    def raw_quantity(self):
        return self.stop - self.start

    @property
    def cost_lamports(self):
        if self.original_cost_lamports is None:
            return None
        return (self.original_cost_lamports * self.stop // self.original_quantity
                - self.original_cost_lamports * self.start // self.original_quantity)


@dataclass(frozen=True)
class Sale:
    tx_id: str
    action_id: str
    wallet: str
    mint: str
    origin_id: tuple[str, str]
    raw_quantity: int
    cost_lamports: int | None
    proceeds_lamports: int
    unknown_cost_reason: str | None

    @property
    def pnl_lamports(self):
        if self.cost_lamports is None:
            return None
        return self.proceeds_lamports - self.cost_lamports


@dataclass(frozen=True)
class Receipt:
    transaction: Transaction
    fills: tuple[Action, ...]
    sales: tuple[Sale, ...]
    transferred_cost_lamports: int | None
    cash_delta_lamports: int


class Ledger:
    """Atomic in-memory transactions with strict reject-on-duplicate semantics.

    Sequence is caller-proven relative total order, not guessed wall time. Same
    sequence is rejected even for distinct transactions. OPEN is only legal before
    any activity for that wallet/mint, including before a position returned flat.
    Unknown-basis fills are retained but whole-ledger realized PnL becomes None.
    """

    def __init__(self):
        self._lots = {}
        self._decimals = {}
        self._seen_transactions = set()
        self._last_sequence = -1
        self._cash_delta = 0
        self._realized = 0

    @property
    def cash_delta_lamports(self):
        return self._cash_delta

    @property
    def realized_pnl_lamports(self):
        return self._realized

    def lots(self, wallet, mint):
        """Immutable active lot intervals in original acquisition FIFO order."""
        return tuple(self._lots.get((wallet, mint), ()))

    def quantity(self, wallet, mint):
        return sum(lot.raw_quantity for lot in self.lots(wallet, mint))

    def inventory_cost(self, wallet, mint):
        return _known_sum(lot.cost_lamports for lot in self.lots(wallet, mint))

    def apply(self, tx):
        self._validate_transaction(tx)
        # Copy lists, not immutable lots. A failed action publishes no partial state.
        lots = {key: list(value) for key, value in self._lots.items()}
        decimals = dict(self._decimals)
        fills, sales, transferred = [], [], []
        cash_delta = -tx.fee_lamports
        realized = None if self._realized is None else self._realized - tx.fee_lamports
        for index, action in enumerate(tx.actions):
            self._validate_action(action)
            key = (action.wallet, action.mint)
            if action.mint in decimals and decimals[action.mint] != action.decimals:
                raise ValueError("conflicting mint decimals")
            decimals[action.mint] = action.decimals
            if action.kind == "OPEN" and key in lots:
                raise ValueError("opening inventory must precede all account/mint activity")
            inventory = lots.setdefault(key, [])
            if action.kind in {"BUY", "OPEN", "TRANSFER_IN"}:
                inventory.append(Lot((tx.tx_id, action.action_id), (tx.sequence, index),
                                     action.raw_quantity, action.lamports, 0,
                                     action.raw_quantity, action.unknown_cost_reason))
                if action.kind == "BUY":
                    fills.append(action)
                    cash_delta -= action.lamports
                continue
            consumed = _consume(inventory, action.raw_quantity)
            if action.kind == "SELL":
                fills.append(action)
                cash_delta += action.lamports
                revenues = allocate_lamports(action.lamports,
                                             tuple(lot.raw_quantity for lot in consumed))
                for lot, revenue in zip(consumed, revenues):
                    sale = Sale(tx.tx_id, action.action_id, action.wallet, action.mint,
                                lot.origin_id, lot.raw_quantity, lot.cost_lamports,
                                revenue, lot.unknown_cost_reason)
                    sales.append(sale)
                    realized = _known_sum((realized, sale.pnl_lamports))
            else:
                transferred.extend(lot.cost_lamports for lot in consumed)
                if action.kind == "TRANSFER":
                    destination = lots.setdefault((action.destination, action.mint), [])
                    destination.extend(consumed)
                    destination.sort(key=lambda lot: (lot.origin_order, lot.start))
        self._lots = lots
        self._decimals = decimals
        self._last_sequence = tx.sequence
        self._seen_transactions.add(tx.tx_id)
        self._cash_delta += cash_delta
        self._realized = realized
        return Receipt(tx, tuple(fills), tuple(sales), _known_sum(transferred), cash_delta)

    def _validate_transaction(self, tx):
        if not isinstance(tx, Transaction):
            raise ValueError("Transaction required")
        _identity(tx.tx_id, "transaction identity")
        if tx.tx_id in self._seen_transactions:
            raise ValueError("duplicate transaction identity")
        if type(tx.sequence) is not int or tx.sequence < 0 or tx.sequence <= self._last_sequence:
            raise ValueError("transaction order must be strictly increasing nonnegative integers")
        if tx.status not in ("confirmed", "failed"):
            raise ValueError("only confirmed success or confirmed failed status is supported")
        _native(tx.fee_lamports, "fee_lamports")
        if type(tx.actions) is not tuple or any(not isinstance(a, Action) for a in tx.actions):
            raise ValueError("actions must be an immutable tuple of Action")
        if tx.status == "failed" and tx.actions:
            raise ValueError("failed transaction must be fee-only; no fabricated fill or transfer")
        identities = set()
        for action in tx.actions:
            _identity(action.action_id, "action identity")
            if action.action_id in identities:
                raise ValueError("duplicate action identity within transaction")
            identities.add(action.action_id)

    @staticmethod
    def _validate_action(action):
        for name in ("action_id", "wallet", "mint"):
            _identity(getattr(action, name), name)
        if action.kind not in ("BUY", "SELL", "OPEN", "TRANSFER", "TRANSFER_IN", "TRANSFER_OUT"):
            raise ValueError("unsupported action kind")
        _native(action.raw_quantity, "raw_quantity", positive=True)
        if type(action.decimals) is not int or not 0 <= action.decimals <= 255:
            raise ValueError("decimals must be an exact u8 integer")
        if action.lamports is None:
            if action.kind not in ("OPEN", "TRANSFER_IN"):
                raise ValueError("unknown consideration is not a confirmed trade")
            _identity(action.unknown_cost_reason, "unknown cost reason")
        else:
            _native(action.lamports, "lamports")
            if action.unknown_cost_reason is not None:
                raise ValueError("known cost cannot have unknown cost reason")
        if action.kind in ("TRANSFER", "TRANSFER_OUT") and action.lamports != 0:
            raise ValueError("transfer is not sale consideration")
        if action.kind == "TRANSFER":
            _identity(action.destination, "destination")
            _identity(action.owner_evidence, "verified same-owner evidence reference")
            if action.destination == action.wallet:
                raise ValueError("source and destination must differ")
        elif action.destination is not None or action.owner_evidence is not None:
            raise ValueError("destination/ownership evidence only applies to internal transfers")


def _known_sum(values):
    total = 0
    for value in values:
        if value is None:
            return None
        total += value
    return total


def _consume(inventory, quantity):
    if sum(lot.raw_quantity for lot in inventory) < quantity:
        raise ValueError("insufficient inventory; missing history requires explicit opening lot")
    consumed = []
    remaining = []
    for lot in inventory:
        take = min(quantity, lot.raw_quantity)
        if take:
            consumed.append(replace(lot, stop=lot.start + take))
            quantity -= take
        if take < lot.raw_quantity:
            remaining.append(replace(lot, start=lot.start + take))
    inventory[:] = remaining
    return consumed


def allocate_lamports(total_lamports, weights):
    """Allocate by cumulative floor; differences telescope to the exact total."""
    weights = tuple(weights)
    if type(total_lamports) is not int or not weights:
        raise ValueError("integer total and nonempty weights required")
    if any(type(weight) is not int or weight <= 0 for weight in weights):
        raise ValueError("weights must be positive integers")
    denominator = sum(weights)
    result = []
    cumulative = previous = 0
    for weight in weights:
        cumulative += weight
        allocated = total_lamports * cumulative // denominator
        result.append(allocated - previous)
        previous = allocated
    return tuple(result)
