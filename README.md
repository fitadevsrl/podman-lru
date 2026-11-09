# podman-lru

Least-recently-used eviction of podman images for build hosts.

podman-lru runs as a container next to podman, follows `podman events` through
the libpod API socket, remembers when each image and cached layer was last used,
and removes the least recently used ones whenever image storage exceeds a byte
budget. It targets podman 4.9 on RHEL 8 and ships as a noarch RPM that pulls and
runs the container image.

See [USER.md](USER.md) for installation, configuration and behaviour, and the
[backlog](backlog/) for pending work.

## Why not docuum

[docuum](https://github.com/stepchowfun/docuum) does exactly this for Docker but
not for podman: podman's Docker-compatible socket is reported brittle, and podman
does not record when a layer was last used, so docuum would start blind.

Our case is simpler than the general one. The build host builds images and
rarely runs containers (tools such as pandoc). Images we build are not used after
the build; only third-party bases are reused. Hosts that go idle drop all their
images. So podman-lru treats every image present at startup as used right now,
counts builds, pulls and container creations as uses of the image and its whole
parent chain, and evicts from the leaves inward.

## Design in short

- **Events, not polling**: `GET /libpod/events?stream=true` pushes one JSON line per
  event over a long-lived response. Image lists are refreshed only after an event
  that may have changed them, on reconnect and on a periodic safety net.
- **Layers are images**: with `podman build --layers`, cached steps are dangling
  images linked by `ParentId`. Using an image touches its ancestors, and an image
  with children is never a candidate, so parents outlive children.
- **Budget from podman**: `GET /libpod/system/df` gives the images total with shared
  layers counted once; eviction stops as soon as it fits.
- **Host side is thin**: a systemd unit, a sysconfig file and a state directory.
  The daemon is a static Rust binary in a `FROM scratch` image.
