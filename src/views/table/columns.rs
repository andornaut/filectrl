use ratatui::prelude::Constraint;
use smart_default::SmartDefault;

pub const NAME_MIN_LEN: u16 = 39;
pub const MODE_LEN: u16 = 10;
pub const MODIFIED_LEN: u16 = 12;
pub const SIZE_LEN: u16 = 7;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum SortDirection {
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
pub(super) enum SortColumn {
    #[default]
    Name,
    Modified,
    Size,
}

#[derive(SmartDefault)]
pub(super) struct Columns {
    #[default(NAME_MIN_LEN)]
    name_width: u16,
    sort_column: SortColumn,
    sort_direction: SortDirection,
}

impl Columns {
    pub(super) fn constraints(&mut self, width: u16) -> Vec<Constraint> {
        let (constraints, name_width) = calculate_constraints(width);
        self.name_width = name_width;
        constraints
    }

    pub(super) fn name_width(&self) -> u16 {
        self.name_width
    }

    pub(super) fn sort_column(&self) -> &SortColumn {
        &self.sort_column
    }

    pub(super) fn sort_column_for_click(&mut self, x: u16) -> Option<SortColumn> {
        if x <= self.name_width {
            Some(SortColumn::Name)
        } else if x <= self.name_width + MODIFIED_LEN {
            Some(SortColumn::Modified)
        } else if x <= self.name_width + MODIFIED_LEN + SIZE_LEN {
            Some(SortColumn::Size)
        } else {
            None
        }
    }

    pub(super) fn sort_direction(&self) -> &SortDirection {
        &self.sort_direction
    }

    pub(super) fn sort_by(&mut self, column: SortColumn) {
        if self.sort_column == column {
            self.sort_direction.toggle();
        } else {
            self.sort_column = column;
        }
    }
}

fn calculate_constraints(width: u16) -> (Vec<Constraint>, u16) {
    let mut constraints = Vec::new();
    let mut name_column_width = width;
    let mut len = NAME_MIN_LEN;
    if width > len {
        name_column_width = width - MODIFIED_LEN - 1; // 1 for the cell padding
        constraints.push(Constraint::Length(MODIFIED_LEN));
    }
    len += MODIFIED_LEN + 1 + SIZE_LEN + 1;
    if width > len {
        name_column_width -= SIZE_LEN + 1;
        constraints.push(Constraint::Length(SIZE_LEN));
    }
    len += MODE_LEN + 1;
    if width > len {
        name_column_width -= MODE_LEN + 1;
        constraints.push(Constraint::Length(MODE_LEN));
    }
    constraints.insert(0, Constraint::Length(name_column_width));
    (constraints, name_column_width)
}
