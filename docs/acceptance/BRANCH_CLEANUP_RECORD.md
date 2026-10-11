# Branch cleanup record (`anonym0uxx/mev_bot`)

Snapshot: remote heads fetched without pruning into `refs/remotes/snap/*` on 2026-10-10 PT. Integration head:
`task/integ-combined` = `905acd2d`. `main` = `9ba157bf`. Main is 194 commits behind integration and has 0
main-only commits. The independent snapshot taken at `905acd2d` is **confirmed** on every point below.

## Remote branches

| Branch | Tip | Relation | Proposed disposition |
|---|---|---|---|
| `main` | `9ba157bf` | ancestor of integration | Align with integration through one `--no-ff` merge PR (ancestry preserved). Rollback SHA = `9ba157bf`. |
| `task/integ-combined` | `905acd2d` + docs commits | accepted engineering head | Merge source for the PR. Retire only after the merge is on `main` and nothing depends on it. |
| `task/docs-record-merge` | `160f2e41` | contained in `main` | Delete remote branch after PR review |
| `task/dropped-print-detection` | `67c5330e` | contained in `main` | Delete remote branch after PR review |
| `task/main-consolidation` | `3a16f5ae` | contained in `main` | Delete remote branch after PR review |
| `task/p6-management-slice` | `959cee8c` | contained in `main`; = `rollback/pre-consolidation-959cee8c` | Delete remote branch after PR review (the tag keeps the tip) |
| `task/p6-price-limit-threading` | `6209624c` | contained in `main` | Delete remote branch after PR review |
| `task/training-code-of-record` | `4a8d2cc2` | contained in `main` | Delete remote branch after PR review |
| `task/durable-missing-history` | `c5abbfc0` | contained in integration (69 commits not in `main`); backs PR #13 | Keep until the integration PR merges. Then propose closing PR #13 as superseded and deleting the branch. Do **not** merge PR #13 separately. |
| `task/continuation-record` | `7e725f2e` | 1 commit outside `main` and integration (`docs/CONTINUATION_missing_history.md`, historical) | Archive tag `archive/continuation-record-7e725f2e`, then delete the branch after PR review. Not current operating instructions. |
| `task/north-star-build` | `7d64b50a` (2026-09-09) | 17 commits outside `main` and integration; `tools/data-pipeline/north_star`, data-pipeline src/tests | Archive tag `archive/north-star-build-7d64b50a`, intact, then delete the branch after PR review. Not merged into the runtime. Known active dependency: none found in the running training/evaluation, serving scripts, cron jobs or systemd units. |

Existing tags kept unchanged: `rollback/pre-consolidation-959cee8c`, `tested/resource-budget-5ef32954`.

## Local state (training host clone `/home/alon/build/mev_bot_gh`)

Remote ancestry does not make a local worktree disposable. Nothing local is removed by this record.

- **Main checkout** `/home/alon/build/mev_bot_gh` (on `task/p6-management-slice`) has 2 uncommitted source edits in
  `tools/stream-capture-rs/src/{sender.rs,ws.rs}` (a diff copy was saved off-repo). The 3,859 tracked files that
  show as deleted are build artifacts and old paths under `bench/target`, `tools/stream-capture-rs` and
  `tools/social-ingest-rs`. **Keep untouched**; any clean-up needs an explicit decision.
- **Local branches with commits on no remote:**
  - `task/integ-trial` (1 merge commit)
  - `task/slice-m1acc` (2 commits: operator re-arm interface + tests). That re-arm interface is a variant not in
    integration, which has a different re-arm entry point (`a2e328cd`).
  Keep both. Pushing or archiving them is a separate decision.
- **Worktrees** (all clean): `mev_bot_consol` (durable-missing-history), `/tmp/rb_wt` (detached at `959cee8c`), and
  `wt_{cashback,comb,conc,fees,integ,m1acc,m2,m2reg,mayhem,mgmtrender,offset,plim,serve,shadow,wire}` under
  `/training/mh_build`. Keep all. `wt_serve` is referenced by a serving analysis script (fee config fixture path).
  `wt_comb` is the integration worktree.
- No stashes. No branch, worktree, process, cron job or systemd unit used by the running Q3C training/evaluation
  or its restore scripts references any branch listed for deletion.

## Deletion plan (executed only after operator review of the PR)
1. Push archive tags `archive/continuation-record-7e725f2e` and `archive/north-star-build-7d64b50a`. Verify each
   with `git ls-remote --tags origin` (the tag must resolve to the exact tip).
2. Delete remote branches: `task/docs-record-merge`, `task/dropped-print-detection`, `task/main-consolidation`,
   `task/p6-management-slice`, `task/p6-price-limit-threading`, `task/training-code-of-record`,
   `task/continuation-record`, `task/north-star-build`. Before each deletion, re-verify that the tip is contained in
   `main` or in its archive tag.
3. After the integration PR merges: close PR #13 as superseded, then delete `task/durable-missing-history`. Delete
   `task/integ-combined` once nothing active depends on it.
4. No `git clean`, no stash removal, no forced worktree removal, no history rewrite. Local branches are deleted only
   with `git branch -d` (which refuses unmerged work).
