---
status: open
kind: track
opened: 2026-09-03
---

# Three devices are still on the identity domain

xHCI, NVMe, virtio-net and virtio-console each hold an address space of their
own now, mapping that driver's pools and nothing else. virtio-gpu, virtio-sound
and the Intel HDA controller do not, and each still reaches every byte of RAM.
They are two different problems wearing one heading.

**virtio-gpu is a different shape from the four that moved.** Its command pool
is an ordinary `DmaPool` and would take `alloc_in` unchanged, but the two
framebuffers and the cursor are raw `pmm::alloc_contiguous` pages wrapped in a
`Region` whose `Arc<Pages>` the compositor also holds, and `set_resolution`
allocates a second pair while the first is still live and swaps them. The device
address has to be taken at `attach_backing` and given back after
`destroy_resource` and the scanout swap, while the pages stay alive as long as
any userland holder maps them — a lifetime that is not a pool's. And
`Profile::VirtioGpu` is driven by one registered test, `gpu_set_resolution`, and
it is Nightly, so the display's DMA path would move with a single nightly arm
behind it on the path that is also the owner's own desktop.

**virtio-sound and HDA are blocked on the instrument, not on the code.** Both
moves are three lines each, the same three the four that moved took. What is
missing is a verdict: `cargo test --test toyos-build -- --audio-gate 30` on the
moved shape aborted at iteration 24 of 30 with its own instrument broken —

    [gate A] FAILED on iteration 24: audio_tone_load.smp1 instrument broken: the
    capture came back at 394.2 Hz for a 440 Hz tone — the device consumed the
    buffers at -10.4% of the rate soundd generated them for

— taken at 1-minute host load 22.6 with two other agents' suites on the machine,
against a sample `tests/audio-baseline.toml` records at load 1.2-2.1 with no
concurrent agents. The first 23 iterations were clean at 440.0 Hz with six
scattered mid-signal underruns, all on `smp=1`, all load-coincident. That is a
run that decided nothing in either direction, and a DMA path change under the
mixer may not land on nothing: it needs the gate on a quiet machine, or the T14.

The other half of the same question is whether translation *costs* the audio
path anything at all. QEMU consults its IOTLB per access and misses walk the
tables; on a loaded host that is real work in the device model's completion
path, and the -10.4% consumption rate above is exactly the shape a slower device
model would produce. Nothing here separates that from the load, and nothing in
this harness can — it is the same measurement the 2x cost bar in
`issues/kernel/the-iommu-stops-at-translation.md` is waiting on hardware for.

`iommu_domain_isolation` already takes any of the three the moment they move: it
reads every moved function's context entry out of the tables the unit walks and
asserts the domains are disjoint, with no new host-side machinery. What each
still owes is the behavioural arm — that device aimed at another driver's pool —
which for virtio-gpu means a scanout or cursor backing pointed somewhere it does
not own.
