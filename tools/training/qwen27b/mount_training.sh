#!/usr/bin/env bash
# Idempotent attach-side mount for the dedicated training volume.
# Mounts by LABEL=training (or TRAINING_UUID=<uuid> if exported) — NEVER by /dev/sdX.
# Safe to re-run; exits 0 if already mounted correctly.
set -euo pipefail

TARGET=/training
if [ -n "${TRAINING_UUID:-}" ]; then
    SRC="UUID=${TRAINING_UUID}"
    DEV=$(blkid -U "${TRAINING_UUID}" || true)
else
    SRC="LABEL=training"
    DEV=$(blkid -L training || true)
fi

if [ -z "${DEV}" ]; then
    echo "ERROR: no block device with ${SRC} found." >&2
    echo "Is the VHDX attached? Windows (elevated):" >&2
    echo "  wsl --mount D:\\wsl\\training_ext4.vhdx --vhd --bare" >&2
    exit 1
fi

if findmnt -n "${TARGET}" >/dev/null 2>&1; then
    CUR=$(findmnt -n -o SOURCE "${TARGET}")
    if [ "${CUR}" = "${DEV}" ]; then
        echo "OK: ${TARGET} already mounted from ${DEV}"
    else
        echo "ERROR: ${TARGET} mounted from unexpected source ${CUR} (expected ${DEV})" >&2
        exit 1
    fi
else
    mkdir -p "${TARGET}"
    mount "${SRC}" "${TARGET}"
    echo "Mounted ${SRC} (${DEV}) at ${TARGET}"
fi

mkdir -p "${TARGET}/offload" "${TARGET}/runs"
chown -R alon:alon "${TARGET}"
findmnt -n -o TARGET,SOURCE,FSTYPE,SIZE,AVAIL "${TARGET}"
