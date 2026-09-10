# Megga bounded local ASR acquisition receipt

Status: **completed development-only ASR, with one quarantined timestamp overshoot**. No training admission, teacher-rationale generation, authenticated speaker attribution, UTC mapping, full-capture transcription, or Stage 3 closure.

## Canonical output
- External root: `D:/mev_bot-artifacts/north_star/development/megga_live_asr_v1`
- Final pointer: `ACQUISITION_RECEIPT.json`
- Completed run: `completed_sample/manifest.json`
- Manifest SHA-256: `754c9d9f2a3acca59ae36400ecf0346ee886bda99f24064d623532ee233e1ac7`
- Transcript: `completed_sample/segments.jsonl` and `completed_sample/transcript.txt`
- JSONL SHA-256: `7aedc8fbe99ad81ffef41f7b64fd277c9d807243b7c9acc8df26e5d661ac3748`
- Result: **900.0 seconds of cumulative decoded audio, 176 ASR segments, 1050 whitespace-delimited words**. Text is unedited automated original-audio transcription, not human-verified verbatim evidence or generated teaching prose.
- ASR completion, read from local UTC clock: `2026-09-10T01:41:32.362049+00:00`. This is processing time, NOT the speech epoch.
- Measured ASR elapsed: `46.15600000000268` seconds; VAD retained `384.736` seconds.

## Immutable source and duration mismatch
- Source: `D:/mev_bot-artifacts/narrative/twitch_live/megga_320254509148.mp4`
- Size before/after: `803908618` bytes; modification timestamp unchanged.
- SHA-256 before: `ef679c01c881909b47851ff9608556e7862eb46d5156ecfe155ec78b9f26a269`
- SHA-256 after: `ef679c01c881909b47851ff9608556e7862eb46d5156ecfe155ec78b9f26a269`
- ffprobe audio stream duration: **30546.283292 seconds**; video: **302.718267 seconds**; format: **30546.429292 seconds**. Video duration is not the audio bound. No AV synchronization/content-identity claim is made.
- Audio stream start PTS metadata: `0.146000` seconds; no authenticated UTC mapping.

## Timebase and quarantine
First extraction trimmed by source PTS and yielded only **895.498375 decoded seconds**, failing the exact 900-second sample check. That failed attempt and its immutable manifest remain at the root. A bounded 920-second decoded-frame probe found 42914 frames, 915.4986666666666 decoded seconds versus 920.0003123333335 source-PTS span, with 158 inter-frame discrepancies above 10 microseconds. `pts_diagnostic.json` records this evidence.

The successful derivative uses `asetpts=N/SR/TB,atrim=duration=900`: **14,400,000 mono PCM16 samples at 16 kHz**. It is the first 900 seconds of available decoded samples, not an exact 900-second source-PTS/wall-clock window. Gaps were not synthesized or filled. Segment time zero is the first decoded sample; source-clock continuity and UTC remain unverified.

A second attempt completed most decoding but correctly stopped on an out-of-window raw ASR estimate; its partial output is retained under `decoded_sample_retry/`. The completed acquisition preserves raw estimates without clamping: segment **175** reports **882.78–912.76 seconds**, outside the 900-second input, and is explicitly flagged `raw_asr_estimate_exceeds_audio_window_quarantined`. This is model timestamp overshoot, NOT additional audio transcribed. All segments remain ineligible; no semantic or timestamp-accuracy certification is implied by structural validation.

## Local execution and resource limits
- Inspected `tools/data-pipeline/src/transcribe_vod.py`; did not execute its unbounded/default-download-capable CLI or modify it. Used its existing faster-whisper CUDA API and NVIDIA DLL PATH workaround in external bounded wrappers.
- Python: `C:\Users\Alon\AppData\Local\hermes\hermes-agent\venv\Scripts\python.exe`
- Cached model: `C:\Users\Alon\.cache\huggingface\hub\models--Systran--faster-whisper-large-v3\snapshots\edaa852ec7e145841d8ffdb056a99866b5f0a478`; model files individually SHA-256 pinned in `model_files.json`.
- Versions: faster-whisper `1.2.1`, CTranslate2 `4.8.1`, PyAV `18.0.0`.
- GPU 0, float16, beam 5, VAD enabled; original-language transcription with language autodetection, no translation, initial prompt or hotwords.
- Offline environment flags and `local_files_only=True`; Python network socket audit guard active. No installs, model downloads, network requests, capture, trading, or commits were performed.
- Live GPU/RAM preflight plus sampled resource guard and 600-second worker deadline. Successful run minimum available RAM fraction: `0.883484` (required reserve 12%); maximum observed GPU 0 allocation: `5836` MiB. Sampling is not a claim of continuous OS reservation.

## Verification and limits
Independently recomputed hashes/sizes for all **13** files in the completed manifest inventory. JSONL count matches completion and validation receipts. Every row explicitly sets speaker, creator identity, mint, action, event UTC and UTC mapping to null. Every row sets training, gold, teacher-rationale and evaluation eligibility plus Stage 3 closure to false. `validation.json` validates structure/provenance, not human meaning or recognition correctness. Source hashes matched before/after all three attempts.

Only this receipt was created in the worktree. All scripts, derivatives, failed-attempt evidence, logs, model/tool hashes, resource samples and manifests are under the owned external root. Do not treat the root's initial failed `manifest.json` as the final pointer; use `ACQUISITION_RECEIPT.json` and `completed_sample/manifest.json`.
