"""Outcome-blind boundary primitives; no source reads or outcome selection."""
from copy import deepcopy
import hashlib
import json


def _validate_json(value):
    """Reject Python-only shapes before JSON can silently coerce their identity."""
    if type(value) is dict:
        for key, child in value.items():
            if type(key) is not str:
                raise ValueError("JSON object keys must be strings")
            _validate_json(child)
    elif type(value) is list:
        for child in value:
            _validate_json(child)
    elif value is not None and type(value) not in (str, bool, int, float):
        raise ValueError("invalid JSON value type")


def canonical_sha256(document):
    """SHA256 of UTF-8, sorted-key, compact JSON (no NaN)."""
    _validate_json(document)
    return hashlib.sha256(json.dumps(document, sort_keys=True, separators=(",", ":"),
                                     ensure_ascii=False, allow_nan=False).encode("utf-8")).hexdigest()


class EvaluationBoundary:
    """Immutable snapshot checked against a separately trusted hash pin."""

    def __init__(self, document, *, expected_sha256):
        self._document = deepcopy(document)
        self._sha256 = canonical_sha256(self._document)
        if self.sha256 != expected_sha256:
            raise ValueError("evaluation policy hash mismatch")

        self._validate()

    @property
    def sha256(self):
        return self._sha256

    @property
    def document(self):
        return deepcopy(self._document)

    def _validate(self):
        d = self._document
        allowed = {"DEVELOPMENT", "SEALED_ECONOMICS", "CHALLENGE", "QUARANTINED"}
        required = {"policy_version", "reservation_status", "selected_at_utc_ms",
                    "source_assignments", "exposed_sources", "entity_assignments",
                    "windows", "embargo_ms", "timeless_reference_ids"}
        if type(d) is not dict or not required <= d.keys() or not _identifier(d["policy_version"]):
            raise ValueError("incomplete policy")
        for field in ("source_assignments", "entity_assignments"):
            if type(d[field]) is not dict or any(not _identifier(k) or not _identifier(v)
                                               or v not in allowed for k, v in d[field].items()):
                raise ValueError("invalid assignment")
        for field in ("exposed_sources", "timeless_reference_ids", "materialized_protected_ids"):
            if field in d and (type(d[field]) is not list or any(not _identifier(v) for v in d[field])):
                raise ValueError(f"invalid identifier list: {field}")
        if type(d["windows"]) is not list:
            raise ValueError("windows must be a list")
        for field in ("future_window_status", "exposure_status", "ancestry_status"):
            if field in d and not _identifier(d[field]):
                raise ValueError(f"invalid policy metadata: {field}")
        if "scope" in d:
            scope = d["scope"]
            if (type(scope) is not dict or not _identifier(scope.get("chain"))
                    or type(scope.get("venues")) is not list or not scope["venues"]
                    or any(not _identifier(v) for v in scope["venues"])):
                raise ValueError("invalid scope")
        if "policy_rules" in d and (type(d["policy_rules"]) is not dict
                or any(not _identifier(k) or not _identifier(v) for k, v in d["policy_rules"].items())):
            raise ValueError("invalid policy rules")
        if "policy_sha256" in d and not _hash(d["policy_sha256"]):
            raise ValueError("invalid policy digest")
        for source in d["exposed_sources"]:
            if d["source_assignments"].get(source) != "DEVELOPMENT":
                raise ValueError("exposed source cannot become protected")
        status = d["reservation_status"]
        if not _identifier(status) or status not in {"RESERVED", "POLICY_FROZEN_RESERVATION_PENDING"}:
            raise ValueError("unknown reservation status")
        selected = d["selected_at_utc_ms"]
        if status == "RESERVED" and (not _utc_ms(selected) or not d["windows"]):
            raise ValueError("reservation needs selection time and forward window")
        if status != "RESERVED" and (selected is not None or d["windows"] or d.get("materialized_protected_ids")
                or any(split in {"SEALED_ECONOMICS", "CHALLENGE"}
                       for field in ("source_assignments", "entity_assignments")
                       for split in d[field].values())):
            raise ValueError("pending reservation cannot claim protected assignments or selected windows")
        for index, w in enumerate(d["windows"]):
            if (type(w) is not dict or not _identifier(w.get("source_id"))
                    or not _identifier(w.get("split")) or w["split"] not in {"SEALED_ECONOMICS", "CHALLENGE"}
                    or not _utc_ms(w.get("start_utc_ms")) or not _utc_ms(w.get("end_utc_ms"))
                    or not selected < w["start_utc_ms"] < w["end_utc_ms"]):
                raise ValueError("invalid future window or late selection")
            collection = w.get("collection_started_at_utc_ms")
            if not _utc_ms(collection) or selected >= collection:
                raise ValueError("selection must precede documented collection start")
            if w["source_id"] in d["exposed_sources"]:
                raise ValueError("exposed source cannot become protected")
            for prior in d["windows"][:index]:
                if (w["source_id"] == prior["source_id"]
                        and w["start_utc_ms"] < prior["end_utc_ms"]
                        and prior["start_utc_ms"] < w["end_utc_ms"]):
                    raise ValueError("overlapping reservation windows")
        if type(d["embargo_ms"]) is not dict or set(d["embargo_ms"]) != {"label_horizon", "episode", "feature", "retrieval"}:
            raise ValueError("incomplete embargo dependencies")
        if any(v is not None and not _utc_ms(v) for v in d["embargo_ms"].values()):
            raise ValueError("invalid embargo")

    def assign(self, *, source_id, entity_ids, event_at_utc_ms, available=True):
        """Use only frozen IDs and absolute UTC; unregistered/conflicted stays Q."""
        if (available is not True or not _utc_ms(event_at_utc_ms) or not _identifier(source_id)
                or type(entity_ids) not in (list, tuple) or any(not _identifier(e) for e in entity_ids)):
            return "QUARANTINED"
        d = self._document
        restrictions = []
        if source_id in d["source_assignments"]:
            restrictions.append(d["source_assignments"][source_id])
        restrictions.extend(w["split"] for w in d["windows"]
                            if w["source_id"] == source_id
                            and w["start_utc_ms"] <= event_at_utc_ms < w["end_utc_ms"])
        if not restrictions:
            return "QUARANTINED"
        restrictions.extend(d["entity_assignments"][e] for e in entity_ids if e in d["entity_assignments"])
        return _resolve(restrictions)

    def inherit(self, parent_splits):
        """Caller must supply the COMPLETE transitive dependency closure.

        Missing/unknown parents and cross-partition contexts fail closed. This
        helper cannot discover dependencies absent from the caller's manifest.
        """
        if type(parent_splits) is not dict or any(not _identifier(k) or not _identifier(v)
                                                for k, v in parent_splits.items()):
            return "QUARANTINED"
        restrictions = []
        for parent_id, split in parent_splits.items():
            if split == "TIMELESS_REFERENCE" and parent_id in self._document["timeless_reference_ids"]:
                continue
            restrictions.append(split)
        return _resolve(restrictions)

    def assert_before_fit(self, *, stage, started_at_utc_ms, parent_splits):
        """Mandatory fit/export precondition, not training or live authorization."""
        if not _identifier(stage) or stage not in {"teacher_selection", "feature_fit", "cluster_fit", "execution_calibration",
                         "curation", "retrieval_fit", "cpt", "sft", "retention"}:
            raise ValueError("unknown fitting stage")
        if self._document["reservation_status"] != "RESERVED":
            raise ValueError("reservation pending: fitting blocked")
        if not _utc_ms(started_at_utc_ms) or self._document["selected_at_utc_ms"] >= started_at_utc_ms:
            raise ValueError("reservation must be selected before fitting")
        if self.inherit(parent_splits) != "DEVELOPMENT":
            raise ValueError("protected, unknown or conflicting parent in fitting closure")

    def bind_artifact(self, *, stage, started_at_utc_ms, parent_splits,
                      artifact_sha256, config_sha256):
        """Bind measured artifact/config hashes to the checked fit closure.

        Persist this receipt in immutable custody; a hash is not a signature.
        """
        self.assert_before_fit(stage=stage, started_at_utc_ms=started_at_utc_ms,
                               parent_splits=parent_splits)
        receipt = {"policy_sha256": self.sha256, "stage": stage,
                   "started_at_utc_ms": started_at_utc_ms,
                   "parents_sha256": canonical_sha256(parent_splits),
                   "artifact_sha256": artifact_sha256, "config_sha256": config_sha256}
        self.verify_artifact(receipt, artifact_sha256=artifact_sha256, config_sha256=config_sha256)
        return receipt

    def verify_artifact(self, receipt, *, artifact_sha256, config_sha256):
        if type(receipt) is not dict:
            raise ValueError("invalid artifact receipt")
        for key, actual in (("policy_sha256", self.sha256), ("artifact_sha256", artifact_sha256),
                            ("config_sha256", config_sha256)):
            if (not isinstance(actual, str) or len(actual) != 64
                    or any(c not in "0123456789abcdef" for c in actual)
                    or receipt.get(key) != actual):
                raise ValueError(f"immutable artifact hash mismatch: {key}")

    def training_interval_allowed(self, *, start_utc_ms, label_end_utc_ms):
        """Training must end strictly before the earliest window minus dependencies.

        Source-independent by design: cross-source dependencies cannot bypass the
        global time exclusion. Not a substitute for entity/parent anti-joins.
        """
        d = self._document
        if (d["reservation_status"] != "RESERVED" or not _utc_ms(start_utc_ms)
                or not _utc_ms(label_end_utc_ms) or label_end_utc_ms < start_utc_ms
                or any(v is None for v in d["embargo_ms"].values())):
            return False
        embargo = max(d["embargo_ms"].values())
        return label_end_utc_ms < min(w["start_utc_ms"] for w in d["windows"]) - embargo


def _identifier(value):
    return type(value) is str and bool(value.strip())


def _hash(value):
    return type(value) is str and len(value) == 64 and all(c in "0123456789abcdef" for c in value)


def _utc_ms(value):
    return type(value) is int and value >= 0


def _resolve(restrictions):
    values = set(restrictions)
    if len(values) == 1 and values <= {"DEVELOPMENT", "SEALED_ECONOMICS", "CHALLENGE"}:
        return next(iter(values))
    return "QUARANTINED"
