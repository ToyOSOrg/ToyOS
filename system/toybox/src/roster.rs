//! The roster `ps` prints, as the kernel answers it: the machine's header,
//! then one entry per live thread, or nothing to a capability carrying no
//! `ROSTER`.

use toyos_abi::syscall::{SYSINFO_ENTRY_SIZE as ENTRY, SYSINFO_HEADER_SIZE as HEADER};

/// One live thread, as the roster describes it.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub pid: u32,
    pub tid: u32,
    pub state: u8,
    pub is_thread: bool,
    pub memory: u64,
    pub cpu_ns: u64,
    pub name: String,
}

/// The machine's uptime, the CPU nanoseconds it has ever run, and every row of
/// `answer`, a roster the kernel wrote. `None` is a capability carrying no
/// `ROSTER`, which the kernel answers with nothing.
pub fn rows(answer: &[u8]) -> Option<(u64, u64, Vec<Row>)> {
    let (buf, n) = (answer, answer.len());
    if n < HEADER {
        return None;
    }
    let header = toyos_abi::syscall::SysinfoHeader::decode(buf[..HEADER].try_into().unwrap());
    let (uptime_ns, total_cpu_ns) = (header.uptime_ns, header.total_cpu_ns);
    let mut rows = Vec::new();
    let mut pos = HEADER;
    while pos + ENTRY <= n {
        let name_bytes = &buf[pos + 32..pos + 60];
        let name_len = name_bytes.iter().position(|&b| b == 0).unwrap_or(28);
        rows.push(Row {
            pid: u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()),
            tid: u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap()),
            state: buf[pos + 8],
            is_thread: buf[pos + 9] != 0,
            memory: u64::from_le_bytes(buf[pos + 16..pos + 24].try_into().unwrap()),
            cpu_ns: u64::from_le_bytes(buf[pos + 24..pos + 32].try_into().unwrap()),
            name: std::str::from_utf8(&name_bytes[..name_len]).unwrap_or("?").to_string(),
        });
        pos += ENTRY;
    }
    Some((uptime_ns, total_cpu_ns, rows))
}
