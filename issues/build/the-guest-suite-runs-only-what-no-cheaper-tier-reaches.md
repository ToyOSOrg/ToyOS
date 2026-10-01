---
status: open
kind: track
opened: 2026-10-01
---

# The guest suite runs only what no cheaper tier reaches

The ladder is the review's (`.claude/agents/reviewer.md`, **Guest tests**). Each name below is a
guest test the first cut deleted whose behaviour no cheaper tier holds yet, with the tier that is
to hold it and the behaviour it guarded. Stages are independent and run in parallel; a stage's
pull request deletes the lines it meets, and the stage once it is empty. A row brings back from
`main` before the cut whatever actuator, feature or harness its arm needs; an item names what is
more than an actuator. Owner: the orchestrator, which dispatches each stage.

## Stage A: boot, the loader and updates

Host: `toyos-rootimage`, `toyos-update`, `toyos-gpt`. Metal: the T14's boot records. Exit: each
name is a `#[test]` in the named crate or a `METAL` row, and reverting the behaviour reds it.

- host `root_candidate_malformed`: ROOT bytes the signed header does not cover are refused by name.
- host `root_named_but_absent`: a cmdline naming a partition that is not there is refused.
- host `root_chunk_refused`: an unreadable ROOT chunk is refused by name, never retried forever.
- host `root_candidate_overlaps`: an overlapping ROOT partition is refused.
- host `root_named_twice_on_the_boot_disk`, `root_named_twice`: a twin ROOT is never read.
- host `update_refusals_boot_the_other_slot`: every refused slot is refused by name and the other boots.
- host `update_grant_refuses_a_stray_partition`: init grants only the idle slot's partition.
- host `update_floor_is_the_images_own`: the anti-rollback floor is the key's and the image's.
- host `update_refused_pass_credits_no_image`: a refused pass credits nothing.
- host `screen_gop_firmware_mode`: the loader picks the largest mode both panel and firmware offer.
- host `boot_partition_identity`: the boot partition is found by its unique GUID.
- host `volume_from_another_disk`: DATA on a disk other than the boot disk is refused.
- host `broken_data_volume_is_absent`, `data_candidate_with_bad_geometry_is_absent`: a bad DATA
  candidate is absent, not formatted.
- host `block_duplicate_id`: two disks with one identity are refused.
- metal `root_from_memory`: ROOT mounts from the loader's image with no storage command before init.
- metal `root_withheld_refused`: a handoff with no ROOT image is refused by name; brings back
  `loader-withholds-root` in the loader as well as the kernel.
- metal `kernel_args_layout_refused`: a loader of another `KernelArgs` layout is refused first;
  brings back `loader-writes-no-layout` in the loader as well as the kernel.
- metal `boot_from_power_on`: the power-on spans are the loader's counts at the kernel's rate.
- metal `shipped_config_boots`: the root `system.toml` boots to ready.
- metal `diskless_boot`, `internal_disk_boot`: a boot with no NVMe, and one off the internal disk.
- metal `update_boots_the_new_kernel`: an update over ssh is the kernel the next boot runs.
- metal `update_falls_back_from_a_dying_kernel`, `update_hang_kills_an_unproven_image`: a slot
  that dies or hangs falls back, its death in the next boot's `/log`.
- metal `foreign_disk_untouched`: a disk the system was not given comes back byte for byte;
  brings back `src/fingerprint.rs`.

## Stage B: the panic path and the end of a machine

Metal: the blackbox page and `loader.log` the next boot reads off the T14. Host: the panel
console's renderer. Exit: as stage A.

- metal `panic_reboots`, `panic_before_peripherals_reboots`: a panic ends the boot inside its
  bound, from `percpu::init_bsp` on.
- metal `blackbox_panic_chain`, `blackbox_early_panic_sealed` (and
  `blackbox_early_panic_sealed_muted`, the T14's own shape), `blackbox_fault_sealed`: the report
  is sealed on the page and the next pass reads it.
- metal `panic_outlives_the_deadline`: a panic crosses the deadline's reset as a panic report.
- metal `hang_bounded_by_the_stick`: a hanging image costs one boot and never traps the machine.
- metal `double_fault_stack`, `syscall_panic_halts`, `syscall_fault_halts`,
  `heap_over_ceiling_halts`, `klogd_fault_halts`: each fatal path halts with its own report.
- metal `reentry_names_the_first_panic`, `double_panic_names_the_fault`,
  `nested_fault_is_recursive`: a fault inside the report names the first.
- metal `pre_idle_wedge_speaks`: a boot stopped in phase 3 says where.
- metal `panic_halts_the_others_first`: no other CPU's record follows the fatal line.
- metal `watchdog_resets`, `job_deadline_reboots`: the TCO and the job deadline end the machine.
- metal `quiesce_refuses_a_second_shutdown`: the machine has one shutdown, and a second caller is
  refused while the first holds it.
- metal `screen_pager_keys`: PageUp pages a halted report.
- host `screen_paged_scrollback`: a report longer than the panel pages with no input.
- host `screen_early_panel`: the panel repaints after each committed record.
- host `screen_log_absent`: a `/log` that did not mount is said on the panel.
- host `screen_blocked_dump`: Ctrl+Alt+D's summary tells the three states apart.

## Stage C: CPUs, memory, scheduling and the kernel log

Metal: test kernels armed on the T14. Host: `toyos-sched` and its sim, `kernel-loom`,
`toyos-wallclock`. Exit: as stage A; an unrepresentable name is a type the kernel builds against
and a compile-fail case.

- metal `fpu_isolation`, `gsbase_locked`, first: x86-64 FPU isolation runs nowhere until this
  lands. One process's FPU state and GS base never reach another; brings back the
  `fpu-save-nothing` and `user-writable-gsbase` features, their negative controls.
- metal `control_regs_negative`: an AP left without the declared control registers is caught,
  under `no-ap-control-regs`.
- metal `smp_failed_ap_leaves_no_hole`: an AP that never starts leaves no hole in the roster.
- metal `va_exhaustion`, `heap_ceiling_bounds`: an exhausted address space and heap refuse by name.
- metal `idle_stack_guard`: the idle stack's guard page catches an overflow.
- metal `dump_left_pending_is_owed`, `blocked_dump`: a dump asked in a pass that may not serve it
  is served later, and names each blocked thread.
- metal `syscall_window_nmi_controls`: an NMI at the syscall entry runs on its IST stack; brings
  back `arch/x86_64/nmi_gate.rs` and the entry's hold.
- metal `handler_post_without_a_pass`: a claim vector's watch post lands before any pass.
- metal `tls_rebase_window`: a thread's TLS block is never reachable before its rebase.
- metal `kernel_heartbeat`: the kernel's heartbeat record arrives on its period; brings back
  `kernel/src/heartbeat.rs`.
- metal `wall_clock_utc`: the wall clock reads the RTC as UTC.
- metal `launcher_refusals`, `spawn_cwd`: a spawn's refusals and working directory.
- host `sched_check_build`: the scheduler's `check` instruments hold under the sim.
- host `log_conservation_smp2`, `log_nested_emit`, `log_reserve_window`,
  `log_reserve_window_negative`: every record is read or counted lost, nested emits keep one order.
- host `wall_clock_rtc_dead`, `wall_clock_rtc_unstable`, `wall_clock_no_century`,
  `wall_clock_century_register`: each RTC shape decodes or is refused.
- unrepresentable `lock_across_switch_halts`: a lock guard cannot be held across a switch.
- unrepresentable `hash_seed_precedes_every_map`: no kernel map is built before its seed.

## Stage D: storage and filesystems

Host: `toyos-fat32`, `toyos-fat32-check`, `toyos-blockring`, `bcachefs`. Metal: the T14's stick
and NVMe. Exit: as stage A.

- host `fat_backing_revoked`: unlinked clusters are reissued and the volume stays whole.
- host `fsync_failed_commit`: an fsync keeps refusing while the device refuses its flush.
- host `redirty_mid_flush`: a page redirtied mid-flush reaches the disk.
- host `fs_rename_durable`, `fs_dirs_durable`: renames and directories survive a reboot.
- host `file_mtime_undated`: a file written with no wall clock is stamped as undated.
- host `blockd_serves_partitions`, `blockd_survives_its_death`, `blockd_serves_nothing`: blockd's
  protocol, its restart, and its refusals with no controller.
- host `nvme_large_device`, `nvme_wide_sector`: a device past 2 TiB and an 8 KiB namespace.
- host `fsd_two_data`: two DATA partitions are refused by name.
- metal `fsd_restart`, `fsd_end_at_mount`, `fsd_claim_held`: a file server ended under its
  clients is restarted, or refused while its claim is held.
- metal `home_overwrite_reads_back`, `apps_and_home_are_one_filesystem`, `layout_fresh_boot`:
  `/home`'s bytes and layout on the disk.
- metal `pkg_install_gbae`: `pkg install` lands a package that runs.
- metal `partition_claim`, `partition_claim_gives_up`: a partition claimed as a device, and its
  refusals.
- metal `esp_filesystem`, `toybox_cp_volume`: the ESP mounted and written.
- metal `log_flush_retry`, `kernel_log_file`, `log_partition_layout`, `log_partition_identity`:
  `/log` on the stick, its retries and its layout.
- metal `file_mtime_survives_a_reboot`: a file's mtime survives a reboot.

## Stage E: USB and xHCI

Host: `toyos-xhci` and its sim. Metal: the T14's controller and stick. Exit: as stage A.

- host `xhci_many_devices`, `xhci_second_controller`, `xhci_two_controllers`: devices across one
  and two controllers enumerate and deliver.
- host `xhci_msi_only`, `xhci_no_interrupt`: a controller with MSI only, and with no interrupt.
- host `xhci_slot_exhaustion`, `xhci_scan_hands_over_a_free_slot`: slots run out by name and are
  handed back.
- host `xhci_deaf_registers`, `xhci_slow_connect`, `xhci_portsc_rw1c`, `xhci_flap`: registers that
  never answer, a slow connect, PORTSC's write-one-to-clear, a replug inside the debounce.
- host `xhci_full_speed_device`, `xhci_superspeed_ports`: device speeds bind to their ports.
- host `usb_storage_shapes`, `usb_refused_disk_first`, `usb_pool_exhausted`: disk sizes, sector
  sizes and counts the driver serves or refuses.
- host `usb_short_read`, `usb_storage_write_error`, `usb_flush_optional`: a short read, a refused
  write and a missing cache flush.
- metal `usb_storage_gate`: the stick is read and written byte for byte.
- metal `late_storage_connect`: a disk that connects after the boot scan is bound, and `/boot` and
  `/log` mount off it. Nothing is plugged: `xhci-slow-storage-connect` reports the first root-hub
  port empty until the scan has run, and a row whose stick is on another port is red, not vacuous,
  because its boot then logs no `usb-storage: 0 device(s)`.

## Stage F: input and the i8042

Host: `toyos-ps2`, `toyos-keymap`. Metal: the T14's controller. Exit: as stage A.

- host `input_claim_absent`, `keyboard_claim_close_spares_stdin`: a claim with no device, and a
  close that leaves stdin armed.
- host `i8042_no_spurious_wake`, `i8042_undecoded_bytes`: drains wake only on an event, and
  undecodable bytes are counted.
- host `metal_sim_input`: a PS/2 packet's motion reaches a pointer client scaled and in order; the
  scaling moves out of `kernel/src/mouse.rs` beside `toyos-ps2`'s decode, and the ISR's hand-off to
  the client is kernel wiring no host test reaches.
- host `i8042_absent`, `i8042_quarantine`, `i8042_budget_expiry`, `i8042_fadt_denial`,
  `i8042_kbd_echo`: the probe's absent, quarantined, expired, denied and echoing controller.
- host `swiss_german_layout`, `locale_detect`, `locale_detect_unrecognized`,
  `desktop_locale_detect`: layouts and the locale wizard.
- metal `i8042_health`: the T14's controller reports healthy.

## Stage G: the desktop, graphics and applications

Host: `toyos-desktop`, the console's grid. Metal: the T14's desktop. Exit: as stage A.

- host `metal_sim_window_caps`, `metal_sim_ipc_hostile_peer`, `metal_sim_hostile_clipboard`: the
  compositor refuses a hostile client.
- host `metal_sim_compositor_stall`, `metal_sim_client_death`: a stalled or dying client never
  stops the desktop.
- host `metal_sim_pointer_churn`, `metal_sim_window_drag`: pointer sources bind and a drag moves a
  window.
- host `desktop_typing_damage`: typing damages only the cells it changes.
- host `screen_console_clear`: the console's model clears.
- metal `metal_sim_compositor`: the compositor runs on the T14's GOP scanout.
- metal `doom_frames`: doom renders a demo to a known digest.
- metal `cxx_runtime`: a C++ program links libc++ and runs.
- metal `toolkit_iced`, `toolkit_window_wake`, `toolkit_winit_loop`, `toolkit_winit_pace`: iced
  and winit run unmodified on the desktop.

## Stage H: the network and remote services

Host: `toyos-net-tcp`, `toyos-dns`, `toyos-mdns`, `toyos-swap`, `toyos-inspect`,
`toyos-logstream`. Metal: the cable. Exit: as stage C.

- host `lan_mdns_answer`, `lan_no_lease`: netd answers for its name, and announces with no lease.
- host `netd_connection_caps`, `netd_listener_forgery`, `netd_hostile_peer`: netd refuses forged
  flags, hostile peers and excess connections.
- host `netd_slow_reader`, `netd_refused_pipes`, `netd_held_open`: a slow, refused or silent
  client costs only itself.
- host `netd_udp_refused`, `netd_udp_any_address`: UDP datagrams too large are refused by name;
  `0.0.0.0` receives.
- host `dns_resolve`, `netd_lookup_let_go`: lookups resolve, and abandoned ones are let go.
- host `inspect_reads_its_owners`: `inspect`'s selectors read every owner.
- host `log_stream_stalled_reader`: a reader that never reads stalls neither the file nor another.
- host `swap_netd`, `swap_refusals`, `swap_not_inherited`: a wrong digest, a stranger's key and an
  undeclared program are refused a swap.
- host `sshd_key_auth`: sshd refuses a key not authorized.
- metal `https_tls13` (and `https_tls13_e1000e`, the same fetch on the 82574): ureq and rustls
  fetch over TLS 1.3 on the I219.
- metal `sshd_files`: sftp moves files byte for byte.
- metal `swap_crash_rolls_back`: a replacement that dies under probation is rolled back.
- unrepresentable `netd_seeds_its_stack`: the stack cannot be built without the kernel's seed.

## Stage I: program lines in `/log`

Metal: the stick's `/log`. Exit: as stage A.

- `log_program_line`, `log_program_forgery`, `log_carrier_forgery`: a program's line is filed under
  its pipe's name, and forged heads are refused.
- `log_after_a_refused_stop`, `log_resume_meets_its_flush`: lines after a refused stop and a
  resumed flush reach `/log`.
- `log_program_line_after_its_records`, `log_program_flood`: a flood loses nothing uncounted and
  keeps the owner's lines.

## Stage J: devices, PCI and DMA isolation

Metal: the T14's VT-d. Host: `toyos-pci`, `toyos-pcid`, `toyos-hda`. Exit: as stage A.

- metal `iommu_discovery`, `iommu_context_absent`, `iommu_empty_domain`,
  `iommu_interrupt_remapping`, `iommu_domain_isolation`: the unit is found, and every DMA and
  message outside a domain faults.
- metal `iommu_gpu_scanout_swap`, `iommu_gpu_foreign_backing`, `iommu_hda_foreign_bdl`,
  `iommu_sound_foreign_dma`, `userdev_dma_fault`: a device aimed outside its grant faults and the
  machine runs on.
- metal `userdev_residue_is_its_own`: a claim reads only its own grant's residue.
- metal `blockd_dma_outside_the_lent`, `blockd_lends_within_its_bound`: a userland driver's DMA
  stays inside what it was lent.
- metal `swap_quiets_the_function`, `swap_keeps_what_nothing_reset`, `swap_fault_tells_its_holder`,
  `swap_resets_the_function`: a released function is stopped, kept, faulted or reset before its
  next holder.
- host `pci_function_is_exclusive`, `bar_placement_is_proven`, `pci_claim_caps_truncated`: one
  holder per function, BARs moved only once the machine says so, a truncated capability list.
- host `swap_refused_device_fails`, `swap_moved_device_fails`: a function whose window was lost or
  moved fails the swap by name.
- host `hda_two_live_refused`: two live HDA links are refused by name.
- host `virtio_used_ring`: a used-ring element is refused for a head outside the table, a head
  with no chain and a length past its chain (`Virtqueue::parse_used`, pure once it leaves the
  kernel crate).
- metal `query_pci_agreement`: the kernel's PCI enumeration equals the T14's Ubuntu `lspci` of the
  same machine, function for function: an oracle the kernel did not write.

## Stage K: the shared boot's own judges

Metal, and `userland/libc`'s host tests. Exit: each judge reads on the T14 what its guest judge read.

- metal `disk_backtrace`, `fault_gates`, `debug_trap`, `dlopen_dedup`, `abuse_elf_loader`,
  `exit_wait_storm`: the record beside the exit code — the backtrace's names, the fault's
  attribution, the loader's refusal reason, the wait accounting.
- host `90_stdio_buffering`: stdout and stderr interleave as the case expects.

## Stage L: the harness's own guard

Host. Exit: a host test fails when the harness's death leaves its QEMU running.

- host `guest_dies_with_its_harness`: a `SIGKILL`ed harness takes its QEMU with it.
