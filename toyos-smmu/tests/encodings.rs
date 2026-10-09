//! Every encoding against IHI 0070 H.a's own field positions: each expected
//! value is built from `(msb, lsb, value)` triples as the specification's
//! diagrams and field headings give them, and compared whole, so a field in
//! the wrong place and a stray bit are both a difference.

use toyos_phys::Phys;
use toyos_smmu::config::{Cd, Ste};
use toyos_smmu::queue::{event, Attempt, Code, Command, Event, Signal};
use toyos_smmu::table::{entry, next, plan, Access, Entry, Leaf, INPUT_BITS, MAIR0};
use toyos_smmu::unit::{self, probe, CommandError, Lacks, Unit};

/// `N` doublewords holding `value` in bits `[msb:lsb]` for every triple, and
/// zero everywhere else. A field may cross a doubleword.
fn layout<const N: usize>(fields: &[(u32, u32, u64)]) -> [u64; N] {
    let mut words = [0u64; N];
    for &(msb, lsb, value) in fields {
        let width = msb - lsb + 1;
        assert!(width == 64 || value >> width == 0, "{value:#x} does not fit [{msb}:{lsb}]");
        for bit in 0..width {
            if value >> bit & 1 != 0 {
                let at = lsb + bit;
                assert_eq!(words[(at / 64) as usize] >> (at % 64) & 1, 0, "two fields claim bit {at}");
                words[(at / 64) as usize] |= 1 << (at % 64);
            }
        }
    }
    words
}

fn word(fields: &[(u32, u32, u64)]) -> u64 {
    layout::<1>(fields)[0]
}

/// The identification registers of a unit that lacks nothing: `S1P` [1],
/// `TTF` [3:2] VMSAv8-64 alone, `COHACC` [4], `ASID16` [12], little-endian
/// tables, stall and terminate; 16 StreamID bits, queues of 2^19 and 2^8;
/// `GRAN4K` [4] and a 44-bit output.
const IDR0: &[(u32, u32, u64)] = &[(1, 1, 1), (3, 2, 0b10), (4, 4, 1), (12, 12, 1), (22, 21, 0b10)];
const IDR1: &[(u32, u32, u64)] = &[(5, 0, 16), (20, 16, 8), (25, 21, 19)];
const IDR5: &[(u32, u32, u64)] = &[(2, 0, 0b100), (4, 4, 1)];

/// The fields of `base` with those of `change` in place of the same bits.
fn with(base: &[(u32, u32, u64)], change: &[(u32, u32, u64)]) -> u32 {
    let kept: Vec<_> = base.iter().filter(|b| !change.iter().any(|c| c.0 == b.0)).chain(change).copied().collect();
    word(&kept) as u32
}

fn idr(idr0: &[(u32, u32, u64)], idr1: &[(u32, u32, u64)], idr5: &[(u32, u32, u64)]) -> Result<Unit, Lacks> {
    probe(with(IDR0, idr0), with(IDR1, idr1), with(IDR5, idr5), false)
}

fn whole() -> Unit {
    idr(&[], &[], &[]).expect("a unit that lacks nothing")
}

/// `STALL_MODEL` [25:24] `0b01`: every fault terminates, and none can stall.
fn terminating() -> Unit {
    idr(&[(25, 24, 0b01)], &[], &[]).expect("a unit that terminates every fault")
}

// --- §6.3.1, §6.3.2, §6.3.6: the identification registers ------------------

#[test]
fn a_units_sizes_are_read_from_their_own_fields() {
    let unit = whole();
    assert_eq!((unit.stream_bits, unit.command_queue_log2, unit.event_queue_log2), (16, 19, 8));
    // Each field moved alone moves its own answer and no other.
    let other = idr(&[], &[(5, 0, 32), (20, 16, 19), (25, 21, 0)], &[]).expect("a unit");
    assert_eq!((other.stream_bits, other.command_queue_log2, other.event_queue_log2), (32, 0, 19));
}

/// `IDR5.OAS` [2:0]: 32, 36, 40, 42, 44, 48, 52 and 56 bits. The unit is
/// given a stream table that ends at its last byte and none a byte further.
#[test]
fn a_units_output_size_is_idr5s_and_bounds_what_it_is_given() {
    for (oas, bits) in [32u32, 36, 40, 42, 44, 48, 52, 56].into_iter().enumerate() {
        let unit = idr(&[], &[], &[(2, 0, oas as u64)]).expect("a unit");
        let end = 1u64 << bits.min(48);
        assert!(unit.stream_table(Phys::new(end - 64).unwrap(), 0).is_some(), "OAS {oas:#b}");
        // Past 48 bits there is no address to refuse.
        if let Some(past) = Phys::new(end) {
            assert_eq!(unit.stream_table(past, 0), None, "OAS {oas:#b}");
        }
    }
}

/// DEN 0049 Table 14: the IORT's `COHACC override` stands for `IDR0.COHACC`
/// [4] where it is set, and a unit neither calls coherent is refused.
#[test]
fn a_unit_that_is_not_coherent_is_refused_unless_the_iort_says_it_is() {
    assert_eq!(idr(&[(4, 4, 0)], &[], &[]), Err(Lacks::CoherentAccess));
    let (idr0, idr1, idr5) = (with(IDR0, &[(4, 4, 0)]), with(IDR1, &[]), with(IDR5, &[]));
    assert_eq!(probe(idr0, idr1, idr5, true), Ok(whole()));
    // The override adds nothing to a unit that says so itself.
    assert_eq!(probe(with(IDR0, &[]), idr1, idr5, true), Ok(whole()));
}

#[test]
fn a_unit_that_lacks_what_the_configuration_needs_is_refused_by_name() {
    assert_eq!(idr(&[(1, 1, 0)], &[], &[]), Err(Lacks::Stage1));
    // `TTF` `0b01` is VMSAv8-32 LPAE alone; `0b11` has both.
    assert_eq!(idr(&[(3, 2, 0b01)], &[], &[]), Err(Lacks::Aarch64Tables));
    assert!(idr(&[(3, 2, 0b11)], &[], &[]).is_ok());
    assert_eq!(idr(&[(22, 21, 0b11)], &[], &[]), Err(Lacks::LittleEndianTables));
    assert!(idr(&[(22, 21, 0b00)], &[], &[]).is_ok());
    assert_eq!(idr(&[(25, 24, 0b10)], &[], &[]), Err(Lacks::Termination(0b10)));
    assert_eq!(idr(&[(25, 24, 0b11)], &[], &[]), Err(Lacks::Termination(0b11)));
    assert_eq!(idr(&[], &[], &[(4, 4, 0)]), Err(Lacks::Granule4K));
    assert_eq!(idr(&[], &[(30, 30, 1)], &[]), Err(Lacks::FixedStructures));
    assert_eq!(idr(&[], &[(29, 29, 1)], &[]), Err(Lacks::FixedStructures));
}

#[test]
fn a_reserved_size_is_refused_and_the_largest_defined_one_is_not() {
    assert_eq!(idr(&[], &[(5, 0, 33)], &[]), Err(Lacks::Size(33)));
    assert_eq!(idr(&[], &[(25, 21, 20)], &[]), Err(Lacks::Size(20)));
    assert_eq!(idr(&[], &[(20, 16, 20)], &[]), Err(Lacks::Size(20)));
    assert!(idr(&[], &[(5, 0, 32), (25, 21, 19), (20, 16, 19)], &[]).is_ok());
    // Every value three registers can hold is an answer.
    for bits in [0u32, u32::MAX, 0x5555_5555, 0xaaaa_aaaa] {
        for coherent in [false, true] {
            let _ = probe(bits, bits, bits, coherent);
            let _ = probe(with(IDR0, &[]), bits, bits, coherent);
        }
    }
}

#[test]
fn an_asid_is_no_wider_than_the_units() {
    let narrow = idr(&[(12, 12, 0)], &[], &[]).expect("a unit of 8-bit ASIDs");
    assert_eq!(narrow.asid(0xff).map(|asid| asid.get()), Some(0xff));
    assert_eq!(narrow.asid(0x100), None);
    assert_eq!(whole().asid(0xffff).map(|asid| asid.get()), Some(0xffff));
}

// --- §5.2, §5.4: the stream table entry and the context descriptor ---------

#[test]
fn an_aborting_entry_is_valid_and_configured_to_pass_nothing() {
    // V [0] set; Config [3:1] `0b000`, "report abort to device, no event recorded".
    assert_eq!(Ste::ABORT.words(), layout(&[(0, 0, 1), (3, 1, 0b000)]));
}

#[test]
fn a_translating_entry_is_stage_1_alone_through_one_context_descriptor() {
    let context = Phys::<6>::new(0xfff_ffff_ffc0).expect("64-byte aligned, inside the unit's 44 bits");
    let fields = [
        (0, 0, 1),                       // V
        (3, 1, 0b101),                   // Config: stage 1 translate, stage 2 bypass
        (5, 4, 0),                       // S1Fmt, ignored with S1CDMax zero
        (55, 6, 0xfff_ffff_ffc0 >> 6),   // S1ContextPtr
        (63, 59, 0),                     // S1CDMax: one CD, substreams disabled
        (67, 66, 0b01),                  // S1CIR
        (69, 68, 0b01),                  // S1COR
        (71, 70, 0b11),                  // S1CSH
        (93, 92, 0),                     // EATS
        (95, 94, 0),                     // STRW: NS-EL1
    ];
    let mut stalling = fields.to_vec();
    stalling.push((91, 91, 1)); // S1STALLD, which a unit that can stall must be told
    assert_eq!(Ste::stage1(context, &whole()).map(|ste| ste.words()), Some(layout(&stalling)));
    // And is ILLEGAL to set on one that cannot.
    assert_eq!(Ste::stage1(context, &terminating()).map(|ste| ste.words()), Some(layout(&fields)));
}

#[test]
fn a_context_descriptor_walks_ttb0_alone_under_its_own_asid() {
    let root = Phys::<12>::new(0xfff_ffff_f000).expect("a page inside the unit's 44 bits");
    let unit = whole();
    let asid = unit.asid(0xbeef).expect("a 16-bit ASID");
    assert_eq!(
        Cd::new(root, asid, &unit).map(|cd| cd.words()),
        Some(layout(&[
            (5, 0, 16),                       // T0SZ: 48 bits
            (7, 6, 0b00),                     // TG0: 4KB
            (9, 8, 0b01),                     // IR0
            (11, 10, 0b01),                   // OR0
            (13, 12, 0b11),                   // SH0
            (14, 14, 0),                      // EPD0
            (15, 15, 0),                      // ENDI
            (30, 30, 1),                      // EPD1
            (31, 31, 1),                      // V
            (34, 32, 0b100),                  // IPS: 44 bits, the unit's OAS
            (35, 35, 0),                      // AFFD
            (41, 41, 1),                      // AA64
            (44, 44, 0),                      // S
            (45, 45, 1),                      // R
            (46, 46, 1),                      // A
            (47, 47, 1),                      // ASET
            (63, 48, 0xbeef),                 // ASID
            (119, 68, 0xfff_ffff_f000 >> 4),  // TTB0: address bits [55:4]
            (223, 192, 0x0044_ff04),          // MAIR0: the loader's MAIR, attributes 0 to 3
        ]))
    );
}

#[test]
fn a_context_descriptors_ips_is_the_units_output_size_and_no_more_than_48_bits() {
    let root = Phys::<12>::new(0x1000).unwrap();
    // `TCR_ELx.PS`'s encodings for 32, 36, 40, 42, 44 and 48 bits; 52 and 56 are past these tables.
    for (oas, ips) in [0b000u64, 0b001, 0b010, 0b011, 0b100, 0b101, 0b101, 0b101].into_iter().enumerate() {
        let unit = idr(&[], &[], &[(2, 0, oas as u64)]).expect("a unit");
        let cd = Cd::new(root, unit.asid(1).unwrap(), &unit).expect("a root in the first 4 GiB");
        assert_eq!(cd.words()[0] >> 32 & 0b111, ips, "OAS {oas:#b}");
    }
}

// --- §6.3: the registers the driver writes ---------------------------------

/// §6.2.1, "Registers in Page 0", and §6.2.2, "Registers in Page 1" —
/// "Offsets are relative to the base of Page 1, 0x10000" — row by row. Each
/// register's own description states the same offset in its "Accessible at
/// offset ... from SMMUv3_PAGE_0" (§6.3.1, .2, .6, .9, .10, .11, .12, .15,
/// .17, .18, .19, .20, .24, .25, .26, .27, .28, .29) or "from SMMUv3_PAGE_1"
/// (§6.3.130, §6.3.131).
#[test]
fn the_registers_sit_where_the_register_overview_has_them() {
    let page0: [(usize, usize); 18] = [
        (0x0000, unit::IDR0),
        (0x0004, unit::IDR1),
        (0x0014, unit::IDR5),
        (0x0020, unit::CR0),
        (0x0024, unit::CR0ACK),
        (0x0028, unit::CR1),
        (0x002C, unit::CR2),
        (0x0044, unit::GBPA),
        (0x0050, unit::IRQ_CTRL),
        (0x0054, unit::IRQ_CTRLACK),
        (0x0060, unit::GERROR),
        (0x0064, unit::GERRORN),
        (0x0080, unit::STRTAB_BASE),
        (0x0088, unit::STRTAB_BASE_CFG),
        (0x0090, unit::CMDQ_BASE),
        (0x0098, unit::CMDQ_PROD),
        (0x009C, unit::CMDQ_CONS),
        (0x00A0, unit::EVENTQ_BASE),
    ];
    for (specified, declared) in page0 {
        assert_eq!(declared, specified);
    }
    let page1: [(usize, usize); 2] = [(0x00A8, unit::EVENTQ_PROD), (0x00AC, unit::EVENTQ_CONS)];
    for (specified, declared) in page1 {
        assert_eq!(declared, 0x1_0000 + specified);
    }
}

#[test]
fn the_control_values_hold_each_field_where_its_register_has_it() {
    assert_eq!(u64::from(unit::CR0_SMMUEN | unit::CR0_EVENTQEN | unit::CR0_CMDQEN), word(&[(0, 0, 1), (2, 2, 1), (3, 3, 1)]));
    // TABLE_SH, TABLE_OC, TABLE_IC, QUEUE_SH, QUEUE_OC, QUEUE_IC.
    assert_eq!(
        u64::from(unit::CR1_WRITE_BACK),
        word(&[(11, 10, 0b11), (9, 8, 0b01), (7, 6, 0b01), (5, 4, 0b11), (3, 2, 0b01), (1, 0, 0b01)])
    );
    // E2H clear, RECINVSID, PTM.
    assert_eq!(u64::from(unit::CR2_RECORD_PRIVATE), word(&[(0, 0, 0), (1, 1, 1), (2, 2, 1)]));
    assert_eq!(u64::from(unit::GBPA_ABORT | unit::GBPA_UPDATE), word(&[(20, 20, 1), (31, 31, 1)]));
    assert_eq!(u64::from(unit::IRQ_GERROR | unit::IRQ_EVENTQ), word(&[(0, 0, 1), (2, 2, 1)]));
    assert_eq!(
        u64::from(unit::GERROR_CMDQ | unit::GERROR_EVENTQ_ABORT | unit::GERROR_SERVICE_FAILURE),
        word(&[(0, 0, 1), (2, 2, 1), (8, 8, 1)])
    );
}

#[test]
fn an_error_is_active_while_the_two_registers_differ_in_its_bit() {
    assert_eq!(unit::active_errors(0, 0), 0);
    assert_eq!(unit::active_errors(0b101, 0b100), 0b001);
    // Acknowledged by writing the same value, whichever way the bit last toggled.
    assert_eq!(unit::active_errors(0b101, 0b101), 0);
    assert_eq!(unit::active_errors(0b000, 0b001), 0b001);
}

#[test]
fn a_command_error_is_the_cons_registers_err_field_alone() {
    let cons = |err: u64| word(&[(30, 24, err), (19, 0, 0xf_ffff), (31, 31, 1)]) as u32;
    assert_eq!(unit::command_error(cons(0)), CommandError::None);
    assert_eq!(unit::command_error(cons(1)), CommandError::Illegal);
    assert_eq!(unit::command_error(cons(2)), CommandError::Abort);
    assert_eq!(unit::command_error(cons(3)), CommandError::AtcInvalidation);
    assert_eq!(unit::command_error(cons(0x7f)), CommandError::Reserved(0x7f));
}

#[test]
fn a_stream_table_is_linear_inside_the_units_streams_and_aligned_to_its_size() {
    let unit = whole();
    // 2^8 entries of 64 bytes: 16 KiB.
    let table = Phys::<6>::new(0x4000_4000).unwrap();
    assert_eq!(
        unit.stream_table(table, 8),
        // RA [62], ADDR [55:6]; FMT [17:16] linear, SPLIT [10:6] unused, LOG2SIZE [5:0].
        Some((word(&[(62, 62, 1), (55, 6, 0x4000_4000 >> 6)]), word(&[(17, 16, 0b00), (5, 0, 8)]) as u32))
    );
    // A table larger than everything the unit reads: 2^27 entries are 8 GiB, on a unit of 32 output bits.
    let small = idr(&[], &[(5, 0, 32)], &[(2, 0, 0b000)]).unwrap();
    let zero = Phys::<6>::new(0).unwrap();
    assert!(small.stream_table(zero, 26).is_some());
    assert_eq!(small.stream_table(zero, 27), None);
    assert_eq!(unit.stream_table(Phys::new(0x4000_2000).unwrap(), 8), None, "8 KiB into a 16 KiB table's alignment");
    assert_eq!(unit.stream_table(Phys::new(0x4000_0040).unwrap(), 0), Some((1 << 62 | 0x4000_0040, 0)));
    // The unit takes 16 StreamID bits and no more.
    let big = Phys::<6>::new(0x8000_0000).unwrap();
    assert!(unit.stream_table(big, 16).is_some());
    assert_eq!(unit.stream_table(big, 17), None);
}

#[test]
fn a_queue_is_inside_the_units_size_and_aligned_to_its_own() {
    let unit = whole();
    let at = Phys::<5>::new(0x4001_0000).unwrap();
    // RA or WA [62], ADDR [55:5], LOG2SIZE [4:0].
    let base = |log2: u64| word(&[(62, 62, 1), (55, 5, 0x4001_0000 >> 5), (4, 0, log2)]);
    assert_eq!(unit.command_queue(at, 12).map(|(register, _)| register), Some(base(12)));
    assert_eq!(unit.event_queue(at, 8).map(|(register, _)| register), Some(base(8)));
    // 2^13 commands of 16 bytes are 128 KiB, and 2^12 records of 32 as many: `at` is 64 KiB aligned.
    assert_eq!(unit.command_queue(at, 13), None);
    assert_eq!(unit.event_queue(at, 12), None, "past EVENTQS and misaligned both");
    let unit = idr(&[], &[(20, 16, 19)], &[]).unwrap();
    assert_eq!(unit.event_queue(at, 11).map(|(register, _)| register), Some(base(11)));
    assert_eq!(unit.event_queue(at, 12), None, "128 KiB of records at a 64 KiB alignment");
    // The unit's largest and one more.
    let low = Phys::<5>::new(0).unwrap();
    assert!(unit.command_queue(low, 19).is_some() && unit.event_queue(low, 19).is_some());
    let small = idr(&[], &[(25, 21, 4), (20, 16, 4)], &[]).unwrap();
    assert_eq!((small.command_queue(low, 5), small.event_queue(low, 5)), (None, None));
}

/// §6.3.24: the bits of `ADDR` above `IDR5.OAS` are RES0, and a unit need not
/// store them, so it would read its table or queue at the address without
/// them.
#[test]
fn a_table_or_queue_past_the_units_output_size_is_refused() {
    let unit = idr(&[], &[(20, 16, 19)], &[(2, 0, 0b000)]).expect("a unit of 32 output bits");
    let past = 1u64 << 32;
    assert_eq!(unit.stream_table(Phys::new(past).unwrap(), 0), None);
    assert_eq!(unit.command_queue(Phys::new(past).unwrap(), 0), None);
    assert_eq!(unit.event_queue(Phys::new(past).unwrap(), 0), None);
    // Each ends at the unit's last byte and is given.
    assert!(unit.stream_table(Phys::new(past - (64 << 8)).unwrap(), 8).is_some());
    assert!(unit.command_queue(Phys::new(past - (16 << 8)).unwrap(), 8).is_some());
    assert!(unit.event_queue(Phys::new(past - (32 << 8)).unwrap(), 8).is_some());
}

/// The same bound on what an entry, a descriptor and a table name: an
/// `S1ContextPtr`, a `TTB0`, a next table and a leaf's output address.
#[test]
fn a_descriptor_or_a_mapping_past_the_units_output_size_is_refused() {
    let unit = idr(&[], &[], &[(2, 0, 0b000)]).expect("a unit of 32 output bits");
    let asid = unit.asid(1).unwrap();
    let past = 1u64 << 32;
    assert_eq!(Ste::stage1(Phys::new(past).unwrap(), &unit), None);
    assert!(Ste::stage1(Phys::new(past - 64).unwrap(), &unit).is_some());
    assert_eq!(Cd::new(Phys::new(past).unwrap(), asid, &unit), None);
    assert!(Cd::new(Phys::new(past - 4096).unwrap(), asid, &unit).is_some());
    assert_eq!(next(Phys::new(past).unwrap(), &unit), None);
    assert!(next(Phys::new(past - 4096).unwrap(), &unit).is_some());
    let memory = |at: u64| plan(AT, Leaf::Memory(Phys::new(at).unwrap(), Access::ReadWrite), &unit);
    assert_eq!(memory(past), None);
    assert!(memory(past - (1 << 21)).is_some());
    let doorbell = |at: u64| plan(AT, Leaf::Doorbell(Phys::new(at).unwrap()), &unit);
    assert_eq!(doorbell(past), None);
    assert!(doorbell(past - 4096).is_some());
    // A unit of 44 bits takes what one of 32 does not.
    assert!(Ste::stage1(Phys::new(past).unwrap(), &whole()).is_some());
}

// --- §3.5.1: the queues' indexes --------------------------------------------

/// Against a count kept beside it: the command queue is full exactly at all
/// of its entries and consumed exactly at none, the event queue empty
/// exactly at none, and the slot the index that many steps on.
#[test]
fn a_queues_indexes_say_empty_full_and_slot_as_a_count_would() {
    let unit = idr(&[], &[(20, 16, 19)], &[]).unwrap();
    let low = Phys::<5>::new(0).unwrap();
    for log2size in 0..=4u8 {
        let (_, commands) = unit.command_queue(low, log2size).unwrap();
        let (_, events) = unit.event_queue(low, log2size).unwrap();
        let size = 1usize << log2size;
        // Start anywhere, the wrap flags and the bits above them included.
        for start in [0u32, 1 << log2size, (2 << log2size) - 1, 0x7ff0_0000 | 1 << log2size] {
            let (mut prod, mut cons) = (start, start);
            let (mut produced, mut consumed) = (0usize, 0usize);
            let first = commands.slot(start);
            // Fill, drain halfway, fill again, drain: three laps of the buffer.
            for step in 0..6 * size {
                let held = produced - consumed;
                assert_eq!(commands.is_empty(prod, cons), held == 0, "size {size} step {step}");
                assert_eq!(events.is_empty(prod, cons), held == 0, "size {size} step {step}");
                assert_eq!(commands.is_full(prod, cons), held == size, "size {size} step {step}");
                assert_eq!(commands.slot(prod), (first + produced) % size);
                assert_eq!(events.slot(cons), (first + consumed) % size);
                if held < size && (step / size).is_multiple_of(2) {
                    prod = commands.after(prod);
                    produced += 1;
                } else if held > 0 {
                    cons = events.after(cons, prod);
                    consumed += 1;
                }
            }
        }
    }
    // The wrap flag toggles as the index passes the top, and nothing above it is written back.
    let (_, commands) = unit.command_queue(low, 3).unwrap();
    assert_eq!(commands.after(0b0_111), 0b1_000);
    assert_eq!(commands.after(0b1_111), 0b0_000);
    assert_eq!(commands.after(0xffff_fff7), 0b1_000);
    // An index register is the unit's word: every bit set, it is still an index.
    assert_eq!(commands.after(u32::MAX), 0);
    assert_eq!(unit.command_queue(low, 19).unwrap().1.after(u32::MAX), 0);
    assert_eq!(unit.event_queue(low, 19).unwrap().1.after(0x7fff_ffff, 0), 0);
}

/// §7.4: records were lost while `SMMU_EVENTQ_PROD.OVFLG` [31] differs from
/// `SMMU_EVENTQ_CONS.OVACKFLG` [31], and the consumer acknowledges by
/// writing its flag to the value it read `OVFLG` with.
#[test]
fn consuming_an_event_acknowledges_an_overflow_and_never_raises_one() {
    let unit = idr(&[], &[(20, 16, 19)], &[]).unwrap();
    let (_, events) = unit.event_queue(Phys::new(0).unwrap(), 3).unwrap();
    const OVFLG: u32 = 1 << 31;
    for (prod, cons) in [(0b0_101, 0b0_011), (OVFLG | 0b0_101, OVFLG | 0b0_011)] {
        assert!(!events.overflowed(prod, cons));
        // The flag is written back as it was read: no overflow is forged.
        let written = events.after(cons, prod);
        assert_eq!(written, prod & OVFLG | 0b0_100);
        assert!(!events.overflowed(prod, written));
    }
    for (prod, cons) in [(OVFLG | 0b0_101, 0b0_011), (0b0_101, OVFLG | 0b0_011)] {
        assert!(events.overflowed(prod, cons));
        let written = events.after(cons, prod);
        assert_eq!(written, prod & OVFLG | 0b0_100);
        assert!(!events.overflowed(prod, written));
    }
    // The flags are no part of a position: a queue whose flags differ is as empty as its indexes say.
    assert!(events.is_empty(OVFLG | 0b1_010, 0b1_010));
    assert_eq!(events.slot(OVFLG | 0b1_010), 0b010);
}

// --- chapter 4: commands ----------------------------------------------------

#[test]
fn each_command_holds_its_opcode_and_parameters_where_its_diagram_has_them() {
    let asid = whole().asid(0xa5c3).unwrap();
    // CMD_CFGI_STE: StreamID [63:32], Leaf [64], SSec [10] clear.
    assert_eq!(Command::ForgetStream(0xdead_beef).words(), layout(&[(7, 0, 0x03), (63, 32, 0xdead_beef), (64, 64, 1)]));
    // CMD_CFGI_ALL: CMD_CFGI_STE_RANGE with Range [68:64] of 31.
    assert_eq!(Command::ForgetAll.words(), layout(&[(7, 0, 0x04), (68, 64, 31)]));
    // CMD_TLBI_NH_ASID: ASID [63:48], VMID [47:32].
    assert_eq!(Command::InvalidateAsid(asid).words(), layout(&[(7, 0, 0x11), (47, 32, 0), (63, 48, 0xa5c3)]));
    assert_eq!(Command::InvalidateAll.words(), layout(&[(7, 0, 0x30)]));
    // CMD_SYNC: CS [13:12]; MSIAddress [119:66] zero, so SIG_IRQ is the wired interrupt.
    assert_eq!(Command::Sync(Signal::None).words(), layout(&[(7, 0, 0x46), (13, 12, 0b00)]));
    assert_eq!(Command::Sync(Signal::Irq).words(), layout(&[(7, 0, 0x46), (13, 12, 0b01), (119, 66, 0)]));
}

// --- §7.3: event records ----------------------------------------------------

/// A record of `number` from `stream`, every field the four fault records
/// carry filled: InputAddr [191:128], RnW [99], InD [98] clear as a write's
/// is, PnU [97], S2 [103], CLASS [105:104], STAG [79:64], an IPA [247:204].
fn record(number: u8, stream: u32, address: u64, read: bool) -> [u64; 4] {
    layout(&[
        (7, 0, u64::from(number)),
        (11, 11, 1),
        (31, 12, 0xf_ffff),
        (63, 32, u64::from(stream)),
        (79, 64, 0xffff),
        (97, 97, 1),
        (98, 98, 0),
        (99, 99, u64::from(read)),
        (103, 103, 1),
        (105, 104, 0b10),
        (191, 128, address),
        (247, 204, 0xfff_ffff_ffff),
    ])
}

#[test]
fn a_translation_fault_names_its_stream_its_address_and_whether_it_wrote() {
    for (number, code) in [
        (0x10, Code::Translation),
        (0x11, Code::AddressSize),
        (0x12, Code::AccessFlag),
        (0x13, Code::Permission),
    ] {
        for read in [false, true] {
            assert_eq!(
                event(record(number, 0x0001_0008, 0xffff_8000_dead_b000, read)),
                Event {
                    stream: 0x0001_0008,
                    code,
                    attempt: Some(Attempt { address: 0xffff_8000_dead_b000, write: !read })
                }
            );
        }
    }
}

#[test]
fn every_event_number_is_its_own_code_and_only_a_fault_carries_an_attempt() {
    let named = [
        (0x01, Code::UnsupportedTransaction),
        (0x02, Code::BadStream),
        (0x03, Code::EntryFetch),
        (0x04, Code::BadEntry),
        (0x05, Code::AtsRequest),
        (0x06, Code::StreamDisabled),
        (0x07, Code::TranslatedForbidden),
        (0x08, Code::BadSubstream),
        (0x09, Code::ContextFetch),
        (0x0A, Code::BadContext),
        (0x0B, Code::WalkAbort),
        (0x10, Code::Translation),
        (0x11, Code::AddressSize),
        (0x12, Code::AccessFlag),
        (0x13, Code::Permission),
        (0x20, Code::TlbConflict),
        (0x21, Code::ConfigurationConflict),
        (0x24, Code::PageRequest),
        (0x25, Code::VmsFetch),
    ];
    for number in 0..=u8::MAX {
        let decoded = event(record(number, 7, 0x1000, false));
        let code = named.iter().find(|(n, _)| *n == number).map_or(Code::Other(number), |(_, code)| *code);
        assert_eq!((decoded.stream, decoded.code), (7, code), "event {number:#x}");
        assert_eq!(decoded.attempt.is_some(), (0x10..=0x13).contains(&number), "event {number:#x}");
    }
}

// --- the stage 1 tables (DDI 0487 M.d §D8.3.1) -----------------------------

/// A device address whose four table indexes are 0x1a5, 0x0f0, 0x00f and 0x155.
const AT: u64 = 0x1a5 << 39 | 0x0f0 << 30 | 0x00f << 21;

#[test]
fn memory_is_a_level_2_block_only_its_own_context_and_an_unprivileged_device_reach() {
    let block = Phys::<21>::new(0xfff_ffe0_0000).unwrap();
    let fields = |read_only: u64| {
        word(&[
            (0, 0, 1),                        // valid
            (1, 1, 0),                        // a block
            (4, 2, 1),                        // AttrIndx: MAIR0's attribute 1
            (6, 6, 1),                        // AP[1]: unprivileged access
            (7, 7, read_only),                // AP[2]
            (9, 8, 0b11),                     // SH: inner shareable
            (10, 10, 1),                      // AF
            (11, 11, 1),                      // nG
            (47, 21, 0xfff_ffe0_0000 >> 21),  // the output address
            (53, 53, 1),                      // PXN
            (54, 54, 1),                      // UXN
        ])
    };
    let unit = whole();
    let path = plan(AT, Leaf::Memory(block, Access::ReadWrite), &unit).expect("a 2 MiB aligned address");
    assert_eq!((path.indices(), path.descriptor), (&[0x1a5, 0x0f0, 0x00f][..], fields(0)));
    let path = plan(AT, Leaf::Memory(block, Access::Read), &unit).expect("a 2 MiB aligned address");
    assert_eq!((path.indices(), path.descriptor), (&[0x1a5, 0x0f0, 0x00f][..], fields(1)));
}

#[test]
fn the_doorbell_is_one_level_3_page_of_device_memory() {
    let page = Phys::<12>::new(0x0809_0000).unwrap();
    let path = plan(AT | 0x155 << 12, Leaf::Doorbell(page), &whole()).expect("a 4 KiB aligned address");
    assert_eq!(path.indices(), [0x1a5, 0x0f0, 0x00f, 0x155]);
    assert_eq!(
        path.descriptor,
        word(&[
            (0, 0, 1),                   // valid
            (1, 1, 1),                   // a page
            (4, 2, 0),                   // AttrIndx: MAIR0's attribute 0, Device-nGnRE
            (6, 6, 1),                   // AP[1]
            (7, 7, 0),                   // AP[2]: written
            (10, 10, 1),                 // AF
            (11, 11, 1),                 // nG
            (47, 12, 0x0809_0000 >> 12), // the page alone
            (53, 53, 1),
            (54, 54, 1),
        ])
    );
    // The attributes the two leaves' AttrIndx name: 0 is Device-nGnRE and 1 Normal write-back.
    assert_eq!(MAIR0.to_le_bytes()[..2], [0x04, 0xff]);
}

#[test]
fn an_address_past_the_input_or_off_the_leafs_alignment_has_no_plan() {
    let unit = whole();
    let block = Leaf::Memory(Phys::new(0x20_0000).unwrap(), Access::ReadWrite);
    let page = Leaf::Doorbell(Phys::new(0x1000).unwrap());
    assert_eq!(plan(1 << INPUT_BITS, block, &unit), None);
    assert_eq!(plan(1 << INPUT_BITS, page, &unit), None);
    assert_eq!(plan(!0x1f_ffff, block, &unit), None);
    assert!(plan((1 << INPUT_BITS) - (1 << 21), block, &unit).is_some());
    // 4 KiB aligned is enough for the page and not for the block.
    assert_eq!(plan(AT | 0x1000, block, &unit), None);
    assert!(plan(AT | 0x1000, page, &unit).is_some());
    assert_eq!(plan(AT | 0x800, page, &unit), None);
}

#[test]
fn a_descriptor_read_back_is_a_table_only_above_level_3() {
    let unit = whole();
    let table = Phys::<12>::new(0xfff_ffff_f000).unwrap();
    // Valid [0], table [1], the next table's address [47:12], and no hierarchical restriction.
    let descriptor = next(table, &unit).expect("a table inside the unit's 44 bits");
    assert_eq!(descriptor, word(&[(0, 0, 1), (1, 1, 1), (47, 12, 0xfff_ffff_f000 >> 12)]));
    for level in 0..3 {
        assert_eq!(entry(descriptor, level), Entry::Table(table));
        assert_eq!(entry(0, level), Entry::Invalid);
        // Bit [0] alone decides validity: a stale address under it is nothing.
        assert_eq!(entry(descriptor & !1, level), Entry::Invalid);
    }
    let block = plan(AT, Leaf::Memory(Phys::new(0x4020_0000).unwrap(), Access::ReadWrite), &unit).unwrap();
    assert_eq!(entry(block.descriptor, 2), Entry::Mapped);
    // A level 3 page sets bit [1] as a table descriptor does: it is no table.
    let page = plan(AT, Leaf::Doorbell(Phys::new(0x0809_0000).unwrap()), &unit).unwrap();
    assert_eq!(entry(page.descriptor, 3), Entry::Mapped);
    assert_eq!(entry(0, 3), Entry::Invalid);
}
