//! What a row may do to the machine's disks beyond its own partitions, which
//! only the signed image grants: `authorize = [...]` in its row. The
//! supervisor mints each at the row's start, scoped to one action and target
//! in the state it finds then, and puts it in the row's namespace under
//! [`Authorization::name`]. No login, and no `[apps]` row, ever holds one.

/// One authorization a row may declare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authorization {
    /// Every disk's table, read: a listing and each disk's description.
    Tables,
    /// One designation of the machine's one DATA partition, bound to its
    /// disk's state when the row starts.
    DesignateData,
}

impl Authorization {
    pub const ALL: [Self; 2] = [Self::Tables, Self::DesignateData];

    /// How a row spells it.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Tables => "tables",
            Self::DesignateData => "designate:data",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.word() == word)
    }

    /// The name it has in the row's namespace.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tables => "block-tables",
            Self::DesignateData => "block-designate-data",
        }
    }

    /// Whether it changes a disk, rather than reading one.
    pub const fn changes(self) -> bool {
        matches!(self, Self::DesignateData)
    }
}
