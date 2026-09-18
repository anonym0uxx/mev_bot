"""Admission gate: policy rows must carry per-record evidence provenance.

Replaces the old blanket refusal. The requirement is now enforced rather than
deferred, so a policy row can never be admitted bare -- and a documented refusal
is still an accounted-for record rather than a silent gap.
"""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from release_data import task_bucket, POLICY_EVIDENCE_KEYS


def rec(task, evidence=None, policy_supervision=False):
    meta = {'task': task, 'policy_supervision': policy_supervision}
    if evidence is not None:
        meta['policy_evidence'] = evidence
    return {'meta': meta}


def full_evidence(**overrides):
    evidence = {k: {'status': 'derived', 'source': 'acquisition_v1 journal body'}
                for k in POLICY_EVIDENCE_KEYS}
    evidence.update(overrides)
    return evidence


class PolicyAdmissionTest(unittest.TestCase):
    def test_support_task_still_requires_no_policy_supervision(self):
        self.assertEqual(task_bucket(rec('bounded_movement_summary')), 'bounded_movement_summary')
        with self.assertRaises(ValueError):
            task_bucket(rec('bounded_movement_summary', policy_supervision=True))

    def test_unknown_task_is_refused(self):
        with self.assertRaises(ValueError):
            task_bucket(rec('teleport'))

    def test_policy_task_without_evidence_is_refused(self):
        with self.assertRaises(ValueError) as ctx:
            task_bucket(rec('next_action_decision'))
        self.assertIn('no per-record policy_evidence', str(ctx.exception))

    def test_policy_task_missing_a_required_evidence_key_is_refused(self):
        evidence = full_evidence()
        del evidence['economic']
        with self.assertRaises(ValueError) as ctx:
            task_bucket(rec('next_action_decision', evidence))
        self.assertIn('economic', str(ctx.exception))

    def test_policy_task_with_invalid_status_is_refused(self):
        with self.assertRaises(ValueError) as ctx:
            task_bucket(rec('next_action_decision',
                            full_evidence(economic={'status': 'probably', 'source': 'x'})))
        self.assertIn('valid status', str(ctx.exception))

    def test_policy_task_evidence_without_source_is_refused(self):
        with self.assertRaises(ValueError) as ctx:
            task_bucket(rec('next_action_decision',
                            full_evidence(causal={'status': 'present'})))
        self.assertIn('cites no source', str(ctx.exception))

    def test_documented_refusal_needs_a_reason(self):
        with self.assertRaises(ValueError) as ctx:
            task_bucket(rec('next_action_decision',
                            full_evidence(economic={'status': 'refused', 'source': 'journal'})))
        self.assertIn('refusal without a reason', str(ctx.exception))

    def test_complete_evidence_is_admitted(self):
        evidence = full_evidence(
            action={'status': 'present', 'source': 'labels.sqlite signature+wallet+mint'},
            economic={'status': 'derived', 'source': 'journal getTransaction preBalances/postBalances'},
            causal={'status': 'present', 'source': 'strict pre-event cutoff'})
        self.assertEqual(task_bucket(rec('next_action_decision', evidence)),
                         'next_action_decision')

    def test_a_documented_refusal_is_still_admissible(self):
        """A refused derivation that names its reason is accounted for, not silent."""
        evidence = full_evidence(
            economic={'status': 'refused', 'source': 'journal getTransaction',
                      'reason': 'no pump.fun instruction in transaction'})
        self.assertEqual(task_bucket(rec('next_action_decision', evidence)),
                         'next_action_decision')


if __name__ == '__main__':
    unittest.main()
