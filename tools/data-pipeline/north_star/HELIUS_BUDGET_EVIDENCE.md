# Helius collection budget — account statement versus public pricing

Operator states current Business account allowance is **150,000,000 credits/month**. The operator subsequently reports **1,700,000 credits used**. If that is total usage in the same billing cycle against the stated allowance, the calculated remaining balance is **148,300,000 credits**, with approximately **1.13% used**. This is operator-reported usage plus conditional arithmetic, not an independently queried account balance or a LaserStream-only consumption rate. Billing-cycle boundary, whether usage covers all consumers, add-ons and autoscaling status remain unverified. Preserve the account-specific allowance rather than substituting public standard pricing.

Public provider documentation retrieved 2026-09-09 PT:
- https://www.helius.dev/docs/billing/credits
- https://www.helius.dev/docs/billing/plans

Public standard Business pricing currently lists 100M/month, not the operator's 150M account allowance. This is a discrepancy to verify against the actual account; it does not invalidate a legacy/custom/add-on allocation and is not permission to change plans.

LaserStream gRPC is documented as **2 credits per 0.1 MB uncompressed streamed data**. Local compressed raw/event bytes are NOT billable-byte measurements. Never infer billed credits from zstd/Parquet sizes, or assume two connections cost exactly twice as much regardless of filters/traffic.

No overage, autoscaling enablement, upgrade or credit purchase is authorized. Do not commit seven days of continuous paid capture merely because an evaluation calendar window is reserved. Allocation/routing and collection authorization are separate. Before extending beyond current bounded capture, obtain remaining-account usage and a bounded job estimate from provider-compatible metering. Use existing same-stream capture rather than duplicate subscriptions solely for split creation.

This file is planning evidence, not proof of live account usage or a budget enforcement deployment.
