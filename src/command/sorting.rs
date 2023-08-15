#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

impl SortDirection {
    pub fn toggle(&mut self) {
        match self {
            Self::Ascending => *self = Self::Descending,
            Self::Descending => *self = Self::Ascending,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SortColumn {
    #[default]
    Name,
    Modified,
    Size,
}

impl PartialEq<&str> for SortColumn {
    fn eq(&self, other: &&str) -> bool {
        let other = other.to_lowercase();
        match self {
            Self::Modified => "modified" == other,
            Self::Name => "name" == other,
            Self::Size => "size" == other,
        }
    }
}
