# Task E — sizes.tsv Report

**Date:** 2026-07-31
**Commit after Task E:** (pending)

## What sizes.tsv is

`D:\tmp\toolout\sizes.tsv` is the log created by `_cap_and_log` in
`supervisor/mcp/server.py` (commit c870a02). Every tool result that passes
through the MCP server's `handle_message` path is logged with: timestamp,
tool name, pre-cap chars, post-cap chars, and (if capped) the path to the
full output file.

## Contents

The file contains **2 entries**, both from the synthetic test run during
Task 1 (commit c870a02):

| # | Tool | Pre-cap chars | Post-cap chars | Capped? | Full output path |
|---|---|---|---|---|---|
| 1 | `test_tool` | 17 | 17 | no | — |
| 2 | `test_big` | 20,011 | 7,281 | yes | `D:\tmp\toolout\1785509608_d9be79a1.txt` |

## Why only 2 entries

The `_cap_and_log` function lives in `supervisor/mcp/server.py` and only
fires when the MCP server processes a `tools/call` request. This session's
work was performed via Hermes's own built-in tools (terminal, read_file,
patch, search_files) — those tool results pass through Hermes's internal
dispatch, not the MCP server's `handle_message`. No hermes_supervisor MCP
tool calls were made this session.

## Ten largest tool results by pre-cap size

There are only 2 entries, so the "ten largest" is the complete dataset:

| Rank | Tool | Pre-cap | Post-cap | Reduction |
|---|---|---|---|---|
| 1 | `test_big` | 20,011 chars | 7,281 chars | 64% (elided middle) |
| 2 | `test_tool` | 17 chars | 17 chars | 0% (under cap) |

## Assessment: was the tool-output cap the right fix?

**The cap is correctly implemented** — the test proves it: a 20k-char
input is capped to ~7k with visible elision, the full output is preserved
to disk, and the log records the sizes.

**But the cap has not yet been exercised by real traffic.** The context
growth that pushed the prior session to 105k tokens was driven by Hermes's
own built-in tool results (terminal output, file reads), not by MCP
server tool results. The `_cap_and_log` function caps the MCP path; the
Hermes-internal tool path has no such cap.

**Conclusion:** The cap is a correct fix for the MCP path, but the
context growth is NOT coming from the MCP path. The growth is in Hermes's
own tool results (large terminal outputs, large file reads). The cap
should be extended to the Hermes-internal tool dispatch path, or the
largest tool results should be manually managed (e.g., piping to files
and reading only summaries).

**Evidence for this conclusion:** This session's largest context
consumers were:
- File reads (phase_b_preflight.py: 16k chars, runner.py: 9k chars, etc.)
- Terminal outputs (constitution grep: ~8k chars, ci_gate output: ~500 chars)
- Search results (constitution text: ~12k chars)

None of these passed through `_cap_and_log`. The cap is necessary but
insufficient — the real growth vector is Hermes's built-in tools, which
are uncapped.

---

*Inspection-verified against `D:\tmp\toolout\sizes.tsv` (2 entries) and
the file listing in `D:\tmp\toolout\` (1 .txt file).*
