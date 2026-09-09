"""Additive prospective routing reservation, not capture/admission/fit authority.

Only caller-supplied metadata is examined; no provider, outcome or media reads.
An external trusted digest pin is required; hashes do not authenticate custody.
"""
from copy import deepcopy

from north_star.dependencies import parent_closure
from north_star.splits import canonical_sha256


_UNSET = object()


def _utc_ms(value):
    return type(value) is int and value >= 0


class ProspectiveReservation:
    """Pinned snapshot separate from the still-pending EvaluationBoundary."""

    def __init__(self, document, *, expected_sha256, base_boundary):
        self._document = deepcopy(document)
        self._sha256 = canonical_sha256(self._document)
        if self._sha256 != expected_sha256:
            raise ValueError("reservation hash mismatch")
        self._base = base_boundary
        self._validate()

    def _validate(self):
        d = self._document
        constants = {
            "policy_version": "north_star_prospective_reservation_v3",
            "reservation_status": "PROSPECTIVE_COLLECTION_RESERVED_FIT_BLOCKED",
            "base_policy_sha256": self._base.sha256,
            "materialized_protected_ids": [],
            "dependency_horizons_status": "UNKNOWN",
            "collector_custody_status": "DEPLOYMENT_PENDING",
        }
        if (type(d) is not dict or any(d.get(k) != v for k, v in constants.items())
                or d.get("collection_authorized") is not False
                or d.get("fit_authorized") is not False):
            raise ValueError("invalid reservation policy or authority")
        w = d.get("window")
        if (type(w) is not dict or set(w) != {"start_utc_ms", "end_utc_ms", "split", "source_scope"}
                or w["split"] != "SEALED_ECONOMICS"
                or w["source_scope"] != "ALL_FUTURE_SUPPORTED_CAPTURE_SOURCES"
                or not all(_utc_ms(t) for t in (d.get("selected_at_utc_ms"),
                                               w["start_utc_ms"], w["end_utc_ms"]))
                or not d["selected_at_utc_ms"] < w["start_utc_ms"]
                or w["end_utc_ms"] - w["start_utc_ms"] != 7 * 24 * 60 * 60 * 1000):
            raise ValueError("reservation requires one exact future source-neutral week")

    def assign(self, *, source_id, entity_ids, event_at_utc_ms=None,
               available_at_utc_ms=None, captured_at_utc_ms=None,
               collection_started_at_utc_ms=None, supported_capture=None,
               previously_exposed=None, interval_start_utc_ms=_UNSET,
               interval_end_utc_ms=_UNSET):
        """Reserve metadata for protected custody; never certify pristine data.

        Unknown support/exposure/clocks fail closed. Exact source/provider rights
        and spending approvals must be checked separately before acquisition.
        Explicit intervals have inclusive last-dependency bounds: touching the
        exclusive window end quarantines. Omission means a point record only.
        """
        if (type(source_id) is not str or not source_id.strip()
                or type(entity_ids) not in (tuple, list)
                or any(type(e) is not str or not e.strip() for e in entity_ids)
                or supported_capture is not True or previously_exposed is not False):
            return "QUARANTINED"
        times = (event_at_utc_ms, available_at_utc_ms, captured_at_utc_ms,
                 collection_started_at_utc_ms)
        if not all(_utc_ms(t) for t in times):
            return "QUARANTINED"
        if (not event_at_utc_ms <= available_at_utc_ms <= captured_at_utc_ms
                or not self._document["selected_at_utc_ms"] < collection_started_at_utc_ms <= captured_at_utc_ms):
            return "QUARANTINED"
        w = self._document["window"]
        if not all(w["start_utc_ms"] <= t < w["end_utc_ms"] for t in times[:3]):
            return "QUARANTINED"
        if interval_start_utc_ms is not _UNSET or interval_end_utc_ms is not _UNSET:
            if (not _utc_ms(interval_start_utc_ms) or not _utc_ms(interval_end_utc_ms)
                    or not w["start_utc_ms"] <= interval_start_utc_ms <= event_at_utc_ms
                    <= interval_end_utc_ms < w["end_utc_ms"]):
                return "QUARANTINED"
        base = self._base.document
        if source_id in base["exposed_sources"]:
            return "QUARANTINED"
        restrictions = {"reservation": "SEALED_ECONOMICS"}
        if source_id in base["source_assignments"]:
            restrictions["source"] = base["source_assignments"][source_id]
        for index, entity in enumerate(entity_ids):
            if entity in base["entity_assignments"]:
                restrictions[f"entity:{index}"] = base["entity_assignments"][entity]
        for index, window in enumerate(base["windows"]):
            if (window["source_id"] == source_id
                    and window["start_utc_ms"] <= event_at_utc_ms < window["end_utc_ms"]):
                restrictions[f"window:{index}"] = window["split"]
        return self._base.inherit(restrictions)

    def inherit(self, *, root, graph, node_splits):
        """Resolve the root AND full declared transitive ancestry, not direct parents.

        Exact known exposed identities quarantine regardless of caller labels;
        known source/entity assignments must agree with each declared split.
        Other node IDs may identify artifacts, not sources: absence from the
        base source registry does not establish unknown source metadata or prove
        pristine capture. This helper does not discover producer metadata,
        source aliases, per-record entity bindings, clocks or undeclared edges.
        Production callers must independently verify those bindings and complete
        lineage, and supply routed splits (including quarantine) without relabeling.
        This is restriction propagation only, never source/data admission.
        """
        if type(node_splits) is not dict:
            return "QUARANTINED"
        try:
            parents = parent_closure(root, graph)
        except (ValueError, TypeError):
            return "QUARANTINED"
        base = self._base.document
        exposed = set(base["exposed_sources"])
        restrictions = {}
        for node in (root, *parents):
            if node in exposed:
                return "QUARANTINED"
            split = node_splits.get(node)
            for field in ("source_assignments", "entity_assignments"):
                if node in base[field] and split != base[field][node]:
                    return "QUARANTINED"
            restrictions[node] = split
        return self._base.inherit(restrictions)

    def assert_before_fit(self, *, stage, started_at_utc_ms, parent_splits):
        self._base.assert_before_fit(stage=stage, started_at_utc_ms=started_at_utc_ms,
                                     parent_splits=parent_splits)
        raise ValueError("dependency horizons unknown; collector custody deployment pending: fitting blocked")

    def training_interval_allowed(self, *, start_utc_ms, label_end_utc_ms):
        # This additive policy never resolves the base freeze or dependency gates.
        return False

    @property
    def sha256(self):
        return self._sha256

    @property
    def document(self):
        return deepcopy(self._document)
