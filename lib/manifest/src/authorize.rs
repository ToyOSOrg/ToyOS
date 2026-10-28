//! What a row may do to the machine's disks beyond its own partitions, which
//! only the signed image grants: `authorize = [...]` in its row. The
//! supervisor mints each at every start of the row, one action on one target
//! that the block service resolves each time it is used, and puts it in the
//! row's namespace under [`Authorization::name`]. The build refuses one that
//! changes a disk to a login authority or a row a login can start, and no
//! `[apps]` row can spell one.

/// One authorization a row may declare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authorization {
    /// Every disk's table, read: a listing and each disk's description.
    Tables,
    /// Designations of the machine's one DATA partition, resolved each time
    /// one is asked for, for as long as the row runs.
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
