//! An SFTP version 3 server over one in-memory directory
//! (draft-ietf-secsh-filexfer-02): what the scripted driver answers the
//! `sftp` subsystem with, so a recording's replies come again on any host.
//! Its files have no time; every other request is `OP_UNSUPPORTED`.

use std::collections::BTreeMap;

const INIT: u8 = 1;
const VERSION: u8 = 2;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const WRITE: u8 = 6;
const LSTAT: u8 = 7;
const FSTAT: u8 = 8;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;

const OK: u32 = 0;
const EOF: u32 = 1;
const NO_SUCH_FILE: u32 = 2;
const OP_UNSUPPORTED: u32 = 8;

const ATTR_SIZE: u32 = 1;
const ATTR_PERMISSIONS: u32 = 4;

enum Open {
    File(String),
    Dir { listed: bool },
}

#[derive(Default)]
pub struct Sftp {
    files: BTreeMap<String, Vec<u8>>,
    handles: BTreeMap<u32, Open>,
    next: u32,
    buf: Vec<u8>,
}

struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn u32(&mut self) -> u32 {
        let (v, rest) = self.0.split_first_chunk::<4>().unwrap();
        self.0 = rest;
        u32::from_be_bytes(*v)
    }
    fn u64(&mut self) -> u64 {
        (u64::from(self.u32()) << 32) | u64::from(self.u32())
    }
    fn string(&mut self) -> &'a [u8] {
        let len = self.u32() as usize;
        let (s, rest) = self.0.split_at(len);
        self.0 = rest;
        s
    }
}

fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn frame(kind: u8, body: Vec<u8>) -> Vec<u8> {
    let mut out = ((body.len() + 1) as u32).to_be_bytes().to_vec();
    out.push(kind);
    out.extend(body);
    out
}

fn attrs(out: &mut Vec<u8>, file: Option<&[u8]>) {
    out.extend_from_slice(&(ATTR_SIZE | ATTR_PERMISSIONS).to_be_bytes());
    out.extend_from_slice(&(file.map_or(0, |f| f.len()) as u64).to_be_bytes());
    out.extend_from_slice(&(if file.is_some() { 0o100644u32 } else { 0o040755 }).to_be_bytes());
}

fn status(id: u32, code: u32) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    body.extend_from_slice(&code.to_be_bytes());
    string(&mut body, b"");
    string(&mut body, b"");
    frame(STATUS, body)
}

impl Sftp {
    /// The replies to every whole request in `data`, the rest kept.
    pub fn input(&mut self, data: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some(len) = self.buf.first_chunk::<4>().map(|l| u32::from_be_bytes(*l) as usize) {
            if self.buf.len() < 4 + len {
                break;
            }
            let packet: Vec<u8> = self.buf.drain(..4 + len).skip(4).collect();
            out.extend(self.request(&packet));
        }
        out
    }

    fn request(&mut self, packet: &[u8]) -> Vec<u8> {
        let (kind, mut f) = (packet[0], Fields(&packet[1..]));
        if kind == INIT {
            return frame(VERSION, 3u32.to_be_bytes().to_vec());
        }
        let id = f.u32();
        let mut body = id.to_be_bytes().to_vec();
        let path = |name: &[u8]| String::from_utf8_lossy(name).trim_start_matches('/').to_string();
        match kind {
            REALPATH => {
                body.extend_from_slice(&1u32.to_be_bytes());
                string(&mut body, b"/");
                string(&mut body, b"/");
                body.extend_from_slice(&0u32.to_be_bytes());
                frame(NAME, body)
            }
            STAT | LSTAT => match path(f.string()) {
                p if p.is_empty() || p == "." => {
                    attrs(&mut body, None);
                    frame(ATTRS, body)
                }
                p => match self.files.get(&p) {
                    Some(file) => {
                        attrs(&mut body, Some(file));
                        frame(ATTRS, body)
                    }
                    None => status(id, NO_SUCH_FILE),
                },
            },
            OPEN => {
                let name = path(f.string());
                let flags = f.u32();
                if flags & 0x8 != 0 {
                    self.files.insert(name.clone(), Vec::new());
                } else if !self.files.contains_key(&name) {
                    return status(id, NO_SUCH_FILE);
                }
                self.handle(body, Open::File(name))
            }
            OPENDIR => self.handle(body, Open::Dir { listed: false }),
            CLOSE => {
                let handle = f.string();
                self.handles.remove(&u32::from_be_bytes(handle.try_into().unwrap()));
                status(id, OK)
            }
            READ | WRITE | FSTAT | READDIR => {
                let handle = u32::from_be_bytes(f.string().try_into().unwrap());
                match (kind, self.handles.get_mut(&handle).unwrap()) {
                    (READ, Open::File(name)) => {
                        let (offset, len) = (f.u64() as usize, f.u32() as usize);
                        let file = &self.files[name.as_str()];
                        if offset >= file.len() {
                            return status(id, EOF);
                        }
                        string(&mut body, &file[offset..(offset + len).min(file.len())]);
                        frame(DATA, body)
                    }
                    (WRITE, Open::File(name)) => {
                        let offset = f.u64() as usize;
                        let data = f.string();
                        let file = self.files.get_mut(name.as_str()).unwrap();
                        file.resize(file.len().max(offset + data.len()), 0);
                        file[offset..offset + data.len()].copy_from_slice(data);
                        status(id, OK)
                    }
                    (FSTAT, Open::File(name)) => {
                        attrs(&mut body, Some(&self.files[name.as_str()]));
                        frame(ATTRS, body)
                    }
                    (READDIR, Open::Dir { listed }) if !*listed => {
                        *listed = true;
                        body.extend_from_slice(&(self.files.len() as u32).to_be_bytes());
                        for (name, file) in &self.files {
                            string(&mut body, name.as_bytes());
                            string(&mut body, format!("-rw-r--r-- 1 toyos toyos {:>8} {name}", file.len()).as_bytes());
                            attrs(&mut body, Some(file));
                        }
                        frame(NAME, body)
                    }
                    (READDIR, Open::Dir { .. }) => status(id, EOF),
                    _ => status(id, OP_UNSUPPORTED),
                }
            }
            _ => status(id, OP_UNSUPPORTED),
        }
    }

    fn handle(&mut self, mut body: Vec<u8>, open: Open) -> Vec<u8> {
        let handle = self.next;
        self.next += 1;
        self.handles.insert(handle, open);
        string(&mut body, &handle.to_be_bytes());
        frame(HANDLE, body)
    }
}
