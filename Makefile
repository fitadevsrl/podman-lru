# Common entry points. `make help` lists them.

NAME     := podman-lru
VERSION  := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
IMAGE    ?= docker.io/fitadevsrl/$(NAME)
ENGINE   ?= $(shell command -v podman 2>/dev/null || command -v docker)
TARGET   := x86_64-unknown-linux-musl
PODMAN_TEST_IMAGE ?= quay.io/podman/stable:v4.9.4

.PHONY: help build test lint image rpm integration clean

help: ## Show this help
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | column -t -s "$$(printf '\t')"

build: ## Build the static release binary
	cargo build --release --locked --target $(TARGET)

test: ## Run unit tests
	cargo test --locked

lint: ## Run clippy and rustfmt checks
	cargo clippy --all-targets --locked -- -D warnings
	cargo fmt --check

image: ## Build the container image ($(IMAGE):$(VERSION) and :latest)
	$(ENGINE) build -f Containerfile -t $(IMAGE):$(VERSION) -t $(IMAGE):latest .

rpm: ## Build the noarch RPM inside a Rocky Linux 8 container, output in dist/
	rm -rf dist/rpmbuild
	mkdir -p dist/rpmbuild/SOURCES
	git archive --format=tar.gz --prefix=$(NAME)-$(VERSION)/ -o dist/rpmbuild/SOURCES/$(NAME)-$(VERSION).tar.gz HEAD
	$(ENGINE) run --rm -v "$(CURDIR)/dist/rpmbuild:/root/rpmbuild:Z" -v "$(CURDIR)/packaging:/packaging:ro,Z" \
	    docker.io/library/rockylinux:8 sh -ec '\
	        dnf -q install -y rpm-build systemd-rpm-macros rpmlint >/dev/null && \
	        rpmbuild -ba /packaging/$(NAME).spec && \
	        rpmlint /root/rpmbuild/RPMS/noarch/*.rpm || true'
	cp dist/rpmbuild/RPMS/noarch/*.rpm dist/
	ls -1 dist/*.rpm

integration: build ## Run the end-to-end test against podman 4.9 in a container
	PODMAN_TEST_IMAGE=$(PODMAN_TEST_IMAGE) tests/integration.sh target/$(TARGET)/release/$(NAME)

clean: ## Remove build output
	cargo clean
	rm -rf dist
