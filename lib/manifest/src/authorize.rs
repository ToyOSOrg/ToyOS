//! What a row may do to the machine's disks beyond its own partitions, which
//! only the signed image grants: `authorize = [...]` in its row. The
//! supervisor mints each at every start of the row and puts it in the
//! row's services under [`Authorization::name`]. The build refuses one that
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
    /// Raw read/write access to DATA partitions, including existing volumes.
    /// The trusted formatter selects one; this is not a format-only or
    /// single-use grant. It grants neither the disk table nor other types.
    FormatData,
}

impl Authorization {
    pub const ALL: [Self; 3] = [Self::Tables, Self::DesignateData, Self::FormatData];

    /// How a row spells it.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Tables => "tables",
            Self::DesignateData => "designate:data",
            Self::FormatData => "format:data",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.word() == word)
    }

    /// The name it has among the row's services.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tables => "block-tables",
            Self::DesignateData => "block-designate-data",
            Self::FormatData => "block-format-data",
        }
    }

    /// Whether it changes a disk, rather than reading one.
    pub const fn changes(self) -> bool {
        matches!(self, Self::DesignateData | Self::FormatData)
    }
}
