//! A cursor over untrusted bytes: every read either yields the field or [`Refused::Truncated`].

use crate::{Address, Refused};

pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    pub(crate) const fn rest(&self) -> &'a [u8] {
        self.rest
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Refused> {
        let (head, tail) = self.rest.split_at_checked(n).ok_or(Refused::Truncated)?;
        self.rest = tail;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], Refused> {
        let (head, tail) = self.rest.split_first_chunk::<N>().ok_or(Refused::Truncated)?;
        self.rest = tail;
        Ok(*head)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Refused> {
        let [b] = self.array()?;
        Ok(b)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, Refused> {
        self.array().map(u16::from_le_bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Refused> {
        self.array().map(u32::from_le_bytes)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Refused> {
        self.array().map(u64::from_le_bytes)
    }

    pub(crate) fn address(&mut self) -> Result<Address, Refused> {
        self.array().map(Address)
    }
}
