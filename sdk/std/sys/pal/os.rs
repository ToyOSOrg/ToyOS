use crate::ffi::{OsStr, OsString};
use crate::marker::PhantomData;
use crate::path::{self, PathBuf};
use crate::{fmt, io};

pub fn getcwd() -> io::Result<PathBuf> {
    // `getcwd` returns the length the path *needs*, so a return that exceeds
    // the buffer means nothing was written and names the size to retry with.
    // The fixed buffer alone was a silent-truncation bug: any cwd past 256
    // bytes came back as a shorter, valid-looking path to a *different*
    // directory, and every path derived from it pointed somewhere wrong.
    let mut buf = [0u8; 256];
    let n = toyos_abi::syscall::getcwd(&mut buf);
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::Other, "getcwd failed"));
    }
    if n <= buf.len() {
        return path_from_utf8(&buf[..n]);
    }

    // Too long for the stack buffer: allocate exactly what it asked for. The
    // kernel bounds cwd, so this retries once rather than looping.
    let mut heap: crate::vec::Vec<u8> = crate::vec::Vec::new();
    heap.resize(n, 0);
    let got = toyos_abi::syscall::getcwd(&mut heap);
    if got == 0 || got > heap.len() {
        return Err(io::Error::new(io::ErrorKind::Other, "getcwd failed"));
    }
    path_from_utf8(&heap[..got])
}

fn path_from_utf8(bytes: &[u8]) -> io::Result<PathBuf> {
    let s = core::str::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "invalid utf-8 in cwd"))?;
    Ok(PathBuf::from(s))
}

pub fn chdir(p: &path::Path) -> io::Result<()> {
    crate::sys::fs::chdir(p)
}

pub struct SplitPaths<'a> {
    iter: crate::vec::IntoIter<PathBuf>,
    _marker: PhantomData<&'a ()>,
}

pub fn split_paths(unparsed: &OsStr) -> SplitPaths<'_> {
    let s = unparsed.as_encoded_bytes();
    let paths: crate::vec::Vec<PathBuf> = if s.is_empty() {
        crate::vec::Vec::new()
    } else {
        s.split(|&b| b == b':')
            .map(|p| PathBuf::from(unsafe { OsStr::from_encoded_bytes_unchecked(p) }))
            .collect()
    };
    SplitPaths { iter: paths.into_iter(), _marker: PhantomData }
}

impl<'a> Iterator for SplitPaths<'a> {
    type Item = PathBuf;
    fn next(&mut self) -> Option<PathBuf> {
        self.iter.next()
    }
}

#[derive(Debug)]
pub struct JoinPathsError;

pub fn join_paths<I, T>(paths: I) -> Result<OsString, JoinPathsError>
where
    I: Iterator<Item = T>,
    T: AsRef<OsStr>,
{
    let mut joined = OsString::new();
    for (i, path) in paths.enumerate() {
        if i > 0 {
            joined.push(":");
        }
        let p = path.as_ref();
        if p.as_encoded_bytes().contains(&b':') {
            return Err(JoinPathsError);
        }
        joined.push(p);
    }
    Ok(joined)
}

impl fmt::Display for JoinPathsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "path contains colon separator".fmt(f)
    }
}

impl crate::error::Error for JoinPathsError {}

pub fn current_exe() -> io::Result<PathBuf> {
    let args: crate::vec::Vec<_> = crate::env::args().collect();
    if let Some(arg0) = args.first() {
        Ok(PathBuf::from(arg0))
    } else {
        Err(io::Error::new(io::ErrorKind::NotFound, "no argv[0]"))
    }
}

pub fn temp_dir() -> PathBuf {
    PathBuf::from("/tmp")
}

pub fn home_dir() -> Option<PathBuf> {
    crate::env::var_os("HOME").filter(|s| !s.is_empty()).map(PathBuf::from)
}

pub fn exit(code: i32) -> ! {
    toyos_abi::syscall::exit(code)
}

pub fn getpid() -> u32 {
    toyos_abi::syscall::getpid().0
}
