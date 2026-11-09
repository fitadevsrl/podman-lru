# Prometheus metrics

Expose a small `/metrics` endpoint (opt-in, `PODMAN_LRU_METRICS_ADDR`) with:
images total bytes, budget, number of known images, evictions total, bytes freed
total, last enforce timestamp, and a gauge for "over budget but nothing
evictable". Publish the port in the systemd unit when set.
