# Anthropic Rate Limits — Tier 2

| Model | Requests/min | Input Tokens/min | Output Tokens/min |
|-------|-------------|-----------------|-------------------|
| Claude Opus Active | 1,000 | 450K (excl. cache reads) | 90K |
| Claude Sonnet Active | 1,000 | 450K (excl. cache reads) | 90K |
| Claude Haiku Active | 1,000 | 450K (excl. cache reads) | 90K |

## Additional
- Batch requests: 1,000/min across all models
- Web search tool uses: 30/sec across all models
- Files API storage: 500 GB

## Budget Implications for Bot

The bot shares the Anthropic API budget with OpenClaw (Apollo).

Key constraints:
- **1,000 RPM for Opus** — OpenClaw heartbeats (~2/hr), operator interactions, plus any Opus supervisory calls from the bot
- **450K input tokens/min** — Cache reads are excluded, so prompt caching helps a lot
- **90K output tokens/min** — Less likely bottleneck

### Design Rules
1. The daemon handles ALL latency-sensitive decisions (no LLM calls in hot path)
2. Opus supervisory calls are bounded and infrequent (candidate adjudication, daily analysis)
3. If Opus times out or is rate-limited, daemon continues on state-machine logic (NO_TRADE, not crash)
4. OpenClaw's own usage (chat, heartbeats) takes priority over bot supervisory calls
5. Bot should budget ~10% of RPM for supervisory use, leaving 90% for OpenClaw operations
