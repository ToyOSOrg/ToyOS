# The hosted guest jobs' container: the dated `sid` image carrying the eight
# packages every shard's `deps` step installed per job — 52-59 s on the critical
# path of all thirteen jobs, three times a day (run 31896922288). Published by
# `.github/workflows/ci-image.yml` when this file or the declared QEMU moves;
# consumed pinned by digest and never by tag, so a rebuild cannot change what a
# verdict was measured with. The build
# refuses an image whose QEMU is not the declared instrument — an image is
# only allowed to be faster than apt, never to move the instrument.
#
# The base is the date `.github/apt-snapshot` declares, for the reason that
# file gives: a rolling base carries packages the dated archive cannot
# downgrade back to, and apt refuses the whole install.
FROM debian:sid-20260824@sha256:c1acdb109bacb5adf0f2078892ac177ee2e2ce6f88e96c5b743998b5489d36a9

ARG DEBIAN_FRONTEND=noninteractive

COPY qemu-version /toyos-qemu-version
COPY apt-snapshot /toyos-apt-snapshot
# The dated archive the workflows install from: an image built a day later
# must not carry a different instrument. `http`, because this base carries no
# ca-certificates and fetching them from sid-as-it-stands is the one unpinned
# apt the pin exists to remove; what apt trusts is the archive's signature.
RUN stamp=$(grep -v '^#' /toyos-apt-snapshot | tr -d '[:space:]') \
    && echo "deb http://snapshot.debian.org/archive/debian/$stamp sid main" \
        > /etc/apt/sources.list \
    && rm -f /etc/apt/sources.list.d/debian.sources \
        /etc/apt/sources.list.d/debian.list \
    && apt-get -o Acquire::Check-Valid-Until=false update -qq \
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
