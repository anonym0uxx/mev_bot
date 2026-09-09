# Declared dependency-closure primitive

Stage 1/2 integration groundwork, not complete source ancestry certification.

Independent review found budget checks occurred after scanning the parent list. A failing no-iteration regression preceded the fix: check node/edge budget before parent scanning or set allocation. Final dependency suite: 12 passed.

Implemented iterative bounded DFS over declared exact parent IDs. Missing parents, cycles, duplicates and malformed IDs reject; node/edge budgets bound traversal. Returns deterministic unique sorted transitive parents. Includes alternatives and portfolio dependencies when declared.

TDD RED: five failures with missing module. GREEN: five passed. Expanded malformed/diamond/deep-chain/inherited-protected-alternative fixtures: **11 passed** via `python -m pytest tools/data-pipeline/tests/north_star/test_dependencies.py -q --tb=short`.

The tested integration passes the complete declared closure into the existing split inheritance helper; a held-out rival ancestor quarantines a context despite its selected token being developmental. No actual dataset parents or held-out boundaries were invented. Production edge discovery, source completeness and independent ancestry audit remain required. Test data is synthetic software fixtures only.
