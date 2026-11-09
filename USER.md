# podman-lru user guide

podman-lru keeps the image storage of a podman build host under a byte budget by
removing the least recently used images and intermediate layers. It runs as a
container, follows podman events through the libpod API socket, and deletes
images through the same socket.

## Installation on RHEL 8

Requirements: podman 4.9 or later, systemd, and access to the registry that hosts
the podman-lru image (Docker Hub by default).

```sh
sudo dnf install ./podman-lru-0.1.0-1.el8.noarch.rpm
sudoedit /etc/sysconfig/podman-lru        # set PODMAN_LRU_MAX_SIZE at least
sudo systemctl enable --now podman-lru
sudo journalctl -fu podman-lru
```

The RPM installs:

| Path | Purpose |
|---|---|
| `/usr/lib/systemd/system/podman-lru.service` | pulls the image and runs the container |
| `/etc/sysconfig/podman-lru` | configuration, kept on upgrade |
| `/var/lib/podman-lru/` | state: last-used timestamps |
| `/usr/share/doc/podman-lru/` | this guide and the README |

Installing the RPM enables `podman.socket`, the systemd unit that exposes the
libpod API on `/run/podman/podman.sock`. The service mounts that socket and the
state directory into the container and starts the container with `--sdnotify=conmon`,
so systemd reports it active once the container runs.

### Using a registry proxy

The image reference is `${PODMAN_LRU_REGISTRY}/${PODMAN_LRU_IMAGE}`. To pull
through a company proxy or mirror, change only the registry part:

```sh
PODMAN_LRU_REGISTRY=registry.example.com/dockerhub-proxy
PODMAN_LRU_IMAGE=fitadev/podman-lru:0.1.0
```

Then `sudo systemctl restart podman-lru`. The unit runs `podman pull` before each
start, so a new tag is picked up on restart; a failed pull does not prevent the
start when the image is already present.

## Configuration

All settings live in `/etc/sysconfig/podman-lru`. The file is read by systemd for
the image location and passed as environment to the container for the rest.
Restart the service after editing it.

| Variable | Default | Meaning |
|---|---|---|
| `PODMAN_LRU_REGISTRY` | `docker.io` | registry host (and optional path) serving the image |
| `PODMAN_LRU_IMAGE` | `fitadev/podman-lru:<version>` | image path and tag below the registry |
| `PODMAN_LRU_MAX_SIZE` | `50GB` | byte budget for all images together, see units below |
| `PODMAN_LRU_INTERVAL` | `60s` | period of the safety-net pass (re-list images, enforce budget) |
| `PODMAN_LRU_KEEP` | unset | regular expression; an image with any name matching it is never evicted |
| `PODMAN_LRU_DRY_RUN` | `false` | `true` logs what would be evicted and deletes nothing |
| `RUST_LOG` | `info` | log level: `error`, `warn`, `info`, `debug`, `trace` |

Sizes accept decimal units (`KB`, `MB`, `GB`, `TB`, powers of 1000), binary units
(`KiB`, `MiB`, `GiB`, `TiB`, powers of 1024), a bare letter (`K`, `M`, `G`, `T`,
binary) or a plain number of bytes. Durations accept `30s`, `5m`, `1h`.

The same settings are available as command line flags (`podman-lru --help`), for
running the binary outside the packaged service.

## What counts as used

The build host builds images and rarely runs containers, so "used" means:

- an image is built, pulled, committed, imported, loaded, tagged, untagged,
  pushed, saved or mounted;
- a container is created or started from an image.

Each of these marks the image **and every ancestor** in its parent chain as used
at that moment. Intermediate layers that `podman build --layers` caches are
images with a parent link, so building an image refreshes its whole cache chain.
A base image stays "fresh" as long as anything built on it is.

When podman-lru starts, every image already present is considered used right
now. On later restarts, timestamps come back from the state file and podman
events missed while the service was down are replayed from the last event seen.

## What gets evicted

Every time the state changes, and on every `PODMAN_LRU_INTERVAL`, podman-lru
asks podman for the images disk usage (`podman system df`, shared layers counted
once). While it exceeds `PODMAN_LRU_MAX_SIZE`, it removes one image at a time:

1. candidates are images with no container, no child image and no name matching
   `PODMAN_LRU_KEEP`;
2. the candidate with the oldest last use goes first; ties go to the one that
   frees the most bytes;
3. an image podman refuses to delete (a container appeared meanwhile) is skipped
   for that pass.

Parents are never deleted before their children: a child is at least as recently
used as its parents, and an image with children is not a candidate. When nothing
can be removed and storage is still over budget, podman-lru logs a warning and
waits for the next change.

Only images are managed. Containers, volumes and the podman build cache for
`--mount=type=cache` are untouched.

## Operations

- **Logs**: `journalctl -u podman-lru`. Each eviction logs the image, the bytes
  freed, its last use and the total before removal. `RUST_LOG=debug` adds the
  per-event detail.
- **Dry run**: set `PODMAN_LRU_DRY_RUN=true` and restart. The service logs
  `dry run: would evict` lines in eviction order, simulating the freed bytes, and
  deletes nothing.
- **State file**: `/var/lib/podman-lru/state.json` maps image IDs to unix
  timestamps. Deleting it is safe: every present image is then treated as just
  used.
- **Stopping**: `systemctl stop podman-lru` saves the state and exits. Nothing
  is evicted while the service is down.
- **The podman-lru image itself** runs a container, so it is never a candidate.

## Running outside the RPM

```sh
podman run -d --name podman-lru \
    -v /run/podman/podman.sock:/run/podman/podman.sock \
    -v podman-lru-state:/state \
    -e PODMAN_LRU_MAX_SIZE=50GB \
    docker.io/fitadev/podman-lru:latest
```

Or build and run the binary directly: `cargo build --release` then
`podman-lru --max-size 50GB --socket /run/podman/podman.sock --state ./state.json`.

## Building

| Command | Result |
|---|---|
| `make build` | static binary in `target/x86_64-unknown-linux-musl/release/` |
| `make test` / `make lint` | unit tests; clippy and rustfmt |
| `make image` | container image `docker.io/fitadev/podman-lru:<version>` and `:latest` |
| `make rpm` | noarch RPM in `dist/`, built inside a Rocky Linux 8 container |
| `make integration` | end-to-end test against podman 4.9.4 running in a container |

`make rpm` packages the committed tree (`git archive HEAD`). Pass
`rpmbuild --define 'image_ns myorg'` through the spec to change the default
Docker Hub namespace.
