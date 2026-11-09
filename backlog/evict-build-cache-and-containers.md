# Evict stopped containers and build cache too

Only images are managed today. Stopped containers pin their image and hold
their own writable layer; `--mount=type=cache` build caches grow outside images.
Consider: remove exited containers older than a threshold before evicting images
(`DELETE /libpod/containers/{id}`), and report or prune build cache volumes.
Needs a clear policy on what "used" means for a container.
