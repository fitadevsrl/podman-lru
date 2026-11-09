# Build stage: the Alpine toolchain targets musl, so the result is a static binary.
FROM docker.io/library/rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked \
    && mkdir -p /out/state \
    && cp target/release/podman-lru /out/podman-lru

# Runtime stage: nothing but the binary and the state directory.
FROM scratch
COPY --from=build /out/podman-lru /podman-lru
COPY --from=build /out/state /state
VOLUME ["/state"]
# The podman socket is owned by root on the host; run as root to open it.
USER 0
ENTRYPOINT ["/podman-lru"]
