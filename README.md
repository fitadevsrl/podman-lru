# Podman LRU cleanup policy

The purpose of this tool is to run as a daemon on a build host and remove the least recently used images or layers.

The build host mostly builds images, it doesn't run containers, or very rarely.
So an image or layer must be considered used when it's built and when an image depending on it is built.

The build host will have a maximum disk size allocated to images.

## docuum

docuum would have been the perfect solution if it was compatible with podman.

Unfortunately, it's made for docker and not podman.
While podman has a compatibility layer supporting the docker socket API, it has been reported it's very brittle.
Furthermore, docker stores when a layer was used, podman doesn't.
Thus, when docuum starts, it knows when the layers were used, podman doesn't.

But we can still make this work as our use case is quite simple.
Indeed, the images we build are never used after the build (only third-party bases are reused) and we don't run containers there (except for
tools like pandoc).
Also, whenever a build host goes idle, we delete all images.
So when this new tool starts, simply consider all present images/layers were used right now; most likely there won't be any.

The tool should run inside a container.
It will be fed by `podman events` running outside the container.
I envision podman events piped into a socket mounted in the tool container.
Maybe you have a better idea.
It must be installed using a RPM.
Podman on build host has version 4.9.4-rhel.
Build host is RHEL 8.
