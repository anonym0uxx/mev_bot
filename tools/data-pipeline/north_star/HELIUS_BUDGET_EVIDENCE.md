# Helius collection budget — account statement versus public pricing

Operator states current Business account allowance is **150,000,000 credits/month**. Preserve that account-specific statement. Remaining credits, billing-cycle boundary, other consumers, add-ons and autoscaling status have not been verified.

Public provider documentation retrieved 2026-09-09 PT:
- https://www.helius.dev/docs/billing/credits
- https://www.helius.dev/docs/billing/plans

Public standard Business pricing currently lists 100M/month, not the operator's 150M account allowance. This is a discrepancy to verify against the actual account; it does not invalidate a legacy/custom/add-on allocation and is not permission to change plans.

LaserStream gRPC is documented as **2 credits per 0.1 MB uncompressed streamed data**. Local compressed raw/event bytes are NOT billable-byte measurements. Never infer billed credits from zstd/Parquet sizes, or assume two connections cost exactly twice as much regardless of filters/traffic.

No overage, autoscaling enablement, upgrade or credit purchase is authorized. Do not commit seven days of continuous paid capture merely because an evaluation calendar window is reserved. Allocation/routing and collection authorization are separate. Before extending beyond current bounded capture, obtain remaining-account usage and a bounded job estimate from provider-compatible metering. Use existing same-stream capture rather than duplicate subscriptions solely for split creation.

This file is planning evidence, not proof of live account usage or a budget enforcement deployment.
