#!/usr/bin/env python
"""transcribe_vod.py — Twitch/YouTube VOD -> timestamped transcript (GPU).

Adds the nvidia pip-wheel DLL dirs to the search path (needed for CTranslate2/CUDA
on Windows), then runs faster-whisper large-v3 with VAD to skip silence/banter.
Output: timestamped segments to <input>.segments.json + plain text to <input>.txt.

Usage:
  python transcribe_vod.py <input_audio> [--model large-v3] [--beam 5]
"""
import os, sys, time, json

# --- DLL path fix for CTranslate2/CUDA on Windows (must run before import) ---
# ctranslate2 bundles its own cuDNN but loads cuBLAS/cudart via LoadLibrary, which
# honors PATH (standard search order) but NOT reliably AddDllDirectory. So we both
# prepend the nvidia pip-wheel dirs to PATH and register them.
def _nvidia_bin_dirs():
    dirs = []
    for candidate in [os.path.join(sys.prefix, "Lib", "site-packages"),
                      os.path.join(sys.prefix, "lib", "site-packages")]:
        nvidia_root = os.path.join(candidate, "nvidia")
        if os.path.isdir(nvidia_root):
            for sub in os.listdir(nvidia_root):
                d = os.path.join(nvidia_root, sub, "bin")
                if os.path.isdir(d):
                    dirs.append(d)
    return dirs

_nvidia_dirs = _nvidia_bin_dirs()
os.environ["PATH"] = os.pathsep.join(_nvidia_dirs) + os.pathsep + os.environ.get("PATH", "")
for d in _nvidia_dirs:
    try:
        os.add_dll_directory(d)
    except OSError:
        pass

from faster_whisper import WhisperModel  # noqa: E402


def main():
    inp = sys.argv[1]
    model_name = "large-v3"
    if "--model" in sys.argv:
        model_name = sys.argv[sys.argv.index("--model") + 1]

    print(f"[transcribe] loading {model_name} on CUDA float16...", flush=True)
    model = WhisperModel(model_name, device="cuda", compute_type="float16")

    t0 = time.time()
    segments, info = model.transcribe(
        inp, beam_size=5, language="en", vad_filter=True,
        vad_parameters=dict(min_silence_duration_ms=500),
    )
    segs = [{"start": round(s.start, 2), "end": round(s.end, 2), "text": s.text.strip()}
            for s in segments]
    audio_sec = info.duration
    dt = time.time() - t0

    out_json = inp.rsplit(".", 1)[0] + ".segments.json"
    out_txt = inp.rsplit(".", 1)[0] + ".txt"
    with open(out_json, "w", encoding="utf-8") as f:
        json.dump({"model": model_name, "audio_seconds": audio_sec,
                   "transcribe_seconds": round(dt, 1), "segments": segs}, f, indent=1)
    with open(out_txt, "w", encoding="utf-8") as f:
        for s in segs:
            f.write(f"[{s['start']:.1f}-{s['end']:.1f}] {s['text']}\n")

    nwords = sum(len(s["text"].split()) for s in segs)
    print(f"[transcribe] DONE: {audio_sec:.0f}s audio -> {len(segs)} segments, "
          f"{nwords} words in {dt:.0f}s ({audio_sec/dt:.1f}x realtime)", flush=True)
    print(f"[transcribe] wrote {out_json} and {out_txt}", flush=True)


if __name__ == "__main__":
    main()