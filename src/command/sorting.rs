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

impl SortColumn {}
