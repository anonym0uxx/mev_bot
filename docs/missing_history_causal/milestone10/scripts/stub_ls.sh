#!/bin/sh
# stub sidecar: replays captured wire lines [START,END) of $PQ_STUB_WIRE then idles so the daemon owns shutdown
sed -n "${PQ_STUB_START:-1},${PQ_STUB_END:-1000000}p" "$PQ_STUB_WIRE"
sleep ${PQ_STUB_LINGER:-3}
