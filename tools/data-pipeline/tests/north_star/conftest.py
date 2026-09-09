"""Make each North Star test target independently runnable from any cwd."""
from pathlib import Path
import sys

SOURCE_ROOT = str(Path(__file__).resolve().parents[2] / "src")
if SOURCE_ROOT not in sys.path:
    sys.path.insert(0, SOURCE_ROOT)
