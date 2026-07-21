# llama.cpp server launch — GLM-5.2 on 3× RTX 6000 (reference)

The supervisor talks to a standard `llama-server`. Launch it so the **control channel is
grammar-constrained** and long-context milestone work fits. Tune to your GGUF quant and VRAM.

```bat
llama-server ^
  --model  C:\models\glm-5.2\glm-5.2-Q5_K_M.gguf ^
  --alias  glm-5.2 ^
  --host   127.0.0.1 --port 8080 ^
  --ctx-size 32768 ^
  --n-gpu-layers 999 ^
  --tensor-split 1,1,1 ^                REM spread across the 3× RTX 6000
  --parallel 2 ^                        REM one interactive build slot + one background research slot
  --cont-batching ^
  --flash-attn ^
  --ctx-checkpoints 32 ^                REM resumable long generations
  --jinja                              REM enable chat template / tool parsing
```

Notes:
- **Constrained decoding** is requested per-call by the supervisor via `response_format.json_schema`
  (and `json_schema`); no server flag needed beyond a build that includes the grammar sampler (all
  current builds do). This is what makes GLM's control output mechanically valid.
- **Determinism for control turns**: the client sends a fixed `seed` and low temperature; keep
  server-side sampling defaults from overriding (the client sets them explicitly).
- **Slots**: `--parallel 2` lets the build loop and research loop run without contending; raise if
  you dedicate more inference to research. Keep one GPU's worth of headroom for long-context M-work.
- **Quant choice**: higher-quality quant (Q5_K_M / Q6_K) materially improves hard-task success rate
  and is worth the VRAM on this hardware; the reinforcement engine's best-of-N partly compensates for
  lower quants but cannot fully replace model quality.
- The trading bot's hot path must NOT depend on these GPUs (constitution §58) — they serve the
  builder/researcher only.
```
