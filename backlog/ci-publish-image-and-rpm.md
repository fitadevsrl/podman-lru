# CI: publish the image and the RPM

Add a GitHub Actions workflow that, on a tag `v*`:

1. runs `make lint test`;
2. builds the container image with `Containerfile` and pushes it to Docker Hub as
   `fitadev/podman-lru:<version>` and `:latest` (confirm the Hub namespace first;
   Hub forbids dots, so `fita.dev` cannot be used as is);
3. builds the RPM with `make rpm` and attaches it to the GitHub release.

The RPM default image tag must match the pushed tag (the spec pins
`%{image_ns}/podman-lru:%{version}`).
