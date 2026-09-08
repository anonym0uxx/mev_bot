
## Attempt #18 — 2026-08-31 20:51 PT — FURTHEST YET (cleared optimizer step 1)
- git SHA: d2101118db6aa48317c82783cf192065f1d971aa; DS md5: f6f9c20b17e1633da9b0f8f157264349
- Effective cfg (dup-key caveat: last-key-wins): NVMe param offload buf=12x430M, max_live=6e7, reuse=6e7, prefetch=5e7, ulimit -n 1048576
- Result: baseline eval 6.3968 OK -> step 1 loss=6.706 grad_norm=405.2 finite, all 3 ranks OK (796s/it) -> DIED step 2 swap-in
- Exception: AssertionError: param 846 already assigned swap buffer id 4 (partitioned_param_swapper, all ranks) = upstream PartitionedParamSwapper state bug
- Peak GPU ~46GB/rank observed. dxg residency ceiling ~57-59GB is HOST-EMPIRICAL (this box), not platform law.

## Attempt #19 — pending — controlled change per governance
- Fix dup keys; max_live=1e9 (DS default-scale), reuse_distance=0, prefetch_bucket_size=0 (eliminate cache/prefetch interaction), bufcount=12 unchanged
- If swapper still fails => abandon NVMe param offload; fallback offload_param=cpu + offload_optimizer=nvme (headroom calc first)

## Attempt #19 — 2026-08-31 21:15 PT — LAUNCH STABLE (CANONICAL)
- git SHA: e5cc0279ca2b2bbe3b95c63a6eafb290ff2c7aa4; DS md5: 42759da12532273c35db9a4fb397a21e
- Config: ZeRO-3 BF16, NVMe param offload buf=12x430M pin=false, NVMe optimizer buf=4, max_live=1e9, reuse_distance=0, prefetch_bucket=0, sub_group=3e7, reduce_bucket=5e7, ulimit -n 1048576, .wslconfig memory=210GB
- Env: torch=2.9.1+cu128 deepspeed=0.18.2 transformers=5.16.1 accelerate=1.11.0 cuda=12.8
- Result: eval 6.3968 OK -> step1 6.706/405.2 -> step2 6.628/349.9 LR warmup engaged. Swapper bug eliminated by reuse=0+prefetch=0.
- Throughput ~830s/step, 161 steps ~37h/epoch. GPU ~49GB/rank steady.
- GOVERNANCE: config FROZEN. No further DS knob changes. This file+SHA is the canonical training config.

## Attempt #19 POST-MORTEM + #20 — 2026-09-01T06:22 PT
- #19 death: 2026-09-01 02:23:58 PT, guest OOM-killer shot rank pid 5547 (anon-rss 49.0GB + shmem 31.7GB, total-vm 602GB) during FIRST BEST-VAL CHECKPOINT SAVE at step 23 (val loss 0.4101, ppl 1.507). Launch zone fully cleared; failure stage = checkpoint-save full-model gather (54GB params gathered to rank0 RAM on top of eval residency).
- Authorized fix (Alon, 2026-09-01): stage3_gather_16bit_weights_on_model_save true -> false. Checkpoints save as per-rank ZeRO shards; consolidate offline via zero_to_fp32.py. No effect on math/LR/data/schedule.
- #20 config md5: 98a497c432886fa1f561c31deea2326d (sole delta from canonical a695cea = gather flag)

## #24 — STABILITY HARDENING (grounded, non-regressive; #23 running untouched)
Investigation: training math is already textbook-stable (beta2=0.95, eps=1e-8 + FP32 master
states, grad-clip 1.0, BF16, grad-ckpt use_reentrant=False). All instability is INFRA.
Grounded in: ZeRO-Infinity (arXiv:2104.07857), ZeRO/ZeRO-Offload (2020), Mixed Precision
Training (Micikevicius ICLR'18), Adam eps-trap (arXiv:2510.26788), Diehl "Going Deep on
DeepSpeed" + NVIDIA NeMo perf guide.
 1. launch_cpt_detached.sh: purge stale NVMe offload *.swp (orphaned swap files from dead
    runs bloat /training/offload to 812GB vs ~300GB steady-state); free-space preflight
    gate (refuse if /training <1000G free) to prevent disk-exhaustion at next ckpt save.
 2. cpt_watchdog.sh: add disk_free_GB + offload_MB trend — catch volume exhaustion BEFORE a
    save dies (the #19/#22 failure family, shifted RAM->disk).
No training-math, LR, data, or optimizer change. No regression path.
