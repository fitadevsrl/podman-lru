#!/usr/bin/env bash
# End-to-end test: run podman 4.9 in a privileged container, build images with
# shared layers, drive podman-lru against the real libpod socket and check what
# gets evicted.
#
# Usage: tests/integration.sh path/to/podman-lru
# Env:   ENGINE (docker|podman, auto), PODMAN_TEST_IMAGE (quay.io/podman/stable:v4.9.4)
set -euo pipefail

BIN=$(realpath "${1:?usage: $0 path/to/podman-lru}")
ENGINE=${ENGINE:-$(command -v podman 2>/dev/null || command -v docker)}
PODMAN_TEST_IMAGE=${PODMAN_TEST_IMAGE:-quay.io/podman/stable:v4.9.4}
NAME=podman-lru-it-$$

log() { printf '\n==> %s\n' "$*"; }
fail() { printf '\nFAIL: %s\n' "$*" >&2; "$ENGINE" exec "$NAME" sh -c 'tail -n 50 /tmp/lru-*.log' >&2 || true; exit 1; }
cleanup() { "$ENGINE" rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

pe() { "$ENGINE" exec "$NAME" "$@"; }

# Wait until a log file contains a pattern (or fail after ~30s).
wait_log() {
    local file=$1 pattern=$2 i
    for i in $(seq 1 150); do
        if pe grep -q -- "$pattern" "$file" 2>/dev/null; then return 0; fi
        sleep 0.2
    done
    fail "timeout waiting for '$pattern' in $file"
}

# Start podman-lru in the background inside the podman container.
# $1 = log tag, remaining args = extra env assignments.
start_lru() {
    local tag=$1; shift
    pe sh -c "cd / && env PODMAN_LRU_STATE=/state/state.json RUST_LOG=debug $* \
        nohup podman-lru >/tmp/lru-$tag.log 2>&1 & echo \$! >/tmp/lru.pid"
}

stop_lru() {
    pe sh -c 'kill -TERM "$(cat /tmp/lru.pid)" 2>/dev/null || true
        for i in $(seq 1 50); do kill -0 "$(cat /tmp/lru.pid)" 2>/dev/null || exit 0; sleep 0.1; done
        echo "podman-lru did not exit on SIGTERM" >&2; exit 1'
}

image_present() { pe podman image exists "$1"; }

log "starting podman ($PODMAN_TEST_IMAGE) via $ENGINE"
"$ENGINE" run -d --init --privileged --name "$NAME" \
    -v "$BIN:/usr/local/bin/podman-lru:ro" \
    "$PODMAN_TEST_IMAGE" \
    sh -c 'mkdir -p /run/podman && podman system service --time=0 unix:///run/podman/podman.sock & exec sleep infinity' >/dev/null
for i in $(seq 1 60); do
    pe test -S /run/podman/podman.sock 2>/dev/null && break
    [ "$i" -eq 60 ] && fail "podman socket never appeared"
    sleep 0.5
done
pe podman info --format 'podman {{.Version.Version}}, graph driver {{.Store.GraphDriverName}}'
pe mkdir -p /state /ctx

log "building a shared base image (1 MiB layer)"
pe sh -c 'head -c 1048576 /dev/urandom >/ctx/common
    printf "FROM scratch\nCOPY common /common\n" >/ctx/Containerfile.base
    podman build -q --layers -t localhost/base:latest -f /ctx/Containerfile.base /ctx >/dev/null'

log "starting podman-lru with a huge budget to follow events"
start_lru A PODMAN_LRU_MAX_SIZE=1T
wait_log /tmp/lru-A.log "following podman events"
wait_log /tmp/lru-A.log "within budget"

log "building img1, img2, img3 (each: base <- intermediate <- image), one second apart"
for n in 1 2 3; do
    pe sh -c "head -c 1048576 /dev/urandom >/ctx/a$n; head -c 1048576 /dev/urandom >/ctx/b$n
        printf 'FROM localhost/base:latest\nCOPY a$n /a\nCOPY b$n /b\n' >/ctx/Containerfile.$n
        podman build -q --layers -t localhost/img$n:latest -f /ctx/Containerfile.$n /ctx >/dev/null"
    wait_log /tmp/lru-A.log "used image=localhost/img$n:latest"
    sleep 1.2
done

log "creating a container from img1 so it becomes the most recently used"
pe sh -c 'podman create --name touch1 localhost/img1:latest true >/dev/null && podman rm touch1 >/dev/null'
wait_log /tmp/lru-A.log 'container event'
wait_log /tmp/lru-A.log 'used image=localhost/img1:latest.*chain=3'
sleep 1.2
stop_lru
pe grep -q '"last_used"' /state/state.json || fail "state file not written"
pe sh -c 'podman images -a --format "{{.ID}} {{.Repository}}:{{.Tag}} {{.Size}}"'

log "dry run with budget 0: expected order img2, img3, img1 (base is a parent, never a candidate)"
start_lru B PODMAN_LRU_MAX_SIZE=0 PODMAN_LRU_DRY_RUN=true
wait_log /tmp/lru-B.log "no image is evictable"
stop_lru
order=$(pe sh -c 'grep -m3 "would evict" /tmp/lru-B.log | grep -o "image=localhost/img[0-9]" | tr -d "\n"')
[ "$order" = "image=localhost/img2image=localhost/img3image=localhost/img1" ] || fail "unexpected dry-run order: $order"
for n in 1 2 3; do image_present "localhost/img$n:latest" || fail "dry run deleted img$n"; done
total=$(pe sh -c 'grep -m1 "would evict" /tmp/lru-B.log | grep -o "total=[0-9]*" | cut -d= -f2')
[ "$total" -gt 4000000 ] || fail "implausible total $total"

log "budget = total - 1 byte: only img2 should go"
start_lru C PODMAN_LRU_MAX_SIZE=$((total - 1))
wait_log /tmp/lru-C.log "within budget"
stop_lru
image_present localhost/img2:latest && fail "img2 should have been evicted"
for n in 1 3; do image_present "localhost/img$n:latest" || fail "img$n should have survived"; done
image_present localhost/base:latest || fail "base should have survived"
pe grep -q 'evicted image=localhost/img2:latest' /tmp/lru-C.log || fail "eviction of img2 not logged"

log "budget 1 byte, keep=img1: everything but img1 and its ancestors goes"
start_lru D PODMAN_LRU_MAX_SIZE=1 PODMAN_LRU_KEEP=img1
wait_log /tmp/lru-D.log "no image is evictable"
stop_lru
image_present localhost/img1:latest || fail "img1 should be kept"
image_present localhost/base:latest || fail "base is an ancestor of img1 and should be kept"
image_present localhost/img3:latest && fail "img3 should have been evicted"
remaining=$(pe podman images -aq | wc -l)
# img1, its intermediate layer and base
[ "$remaining" -eq 3 ] || { pe podman images -a; fail "expected 3 images left, got $remaining"; }

log "budget 1 byte, no keep: storage empties, parents after children"
start_lru E PODMAN_LRU_MAX_SIZE=1
wait_log /tmp/lru-E.log "within budget"
stop_lru
remaining=$(pe podman images -aq | wc -l)
[ "$remaining" -eq 0 ] || { pe podman images -a; fail "expected no images left, got $remaining"; }

log "all checks passed"
