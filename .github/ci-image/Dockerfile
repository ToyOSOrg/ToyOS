# The hosted guest jobs' container: `debian:sid` carrying the eight packages
# every shard's `deps` step installed per job — 52-59 s on the critical path
# of all thirteen jobs, three times a day (run 31896922288). Published by
# `.github/workflows/ci-image.yml` when this file or the declared QEMU moves;
# consumed pinned by digest, the way `.github/runner/Dockerfile` is on the
# T14, so a rebuild cannot change what a verdict was measured with. The build
# refuses an image whose QEMU is not the declared instrument — an image is
# only allowed to be faster than apt, never to move the instrument.
FROM debian:sid

ARG DEBIAN_FRONTEND=noninteractive

COPY qemu-version /toyos-qemu-version
RUN apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
        jq \
        qemu-system-x86 \
        xz-utils \
        zstd \
        > /dev/null \
    && want=$(grep -v '^#' /toyos-qemu-version | tr -d '[:space:]') \
    && qemu-system-x86_64 --version | head -1 | grep -F "version $want" \
    && apt-get clean \
    && find /var/lib/apt/lists -mindepth 1 -delete
