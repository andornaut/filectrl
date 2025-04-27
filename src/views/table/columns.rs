use ratatui::prelude::Constraint;

const NAME_MIN_LEN: u16 = 39; // Below this width, we don't show any other columns
const MODE_LEN: u16 = 10;
const MODIFIED_LEN: u16 = 12;
const SIZE_LEN: u16 = 7;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

impl SortDirection {
    pub fn toggle(&mut self) {
        *self = match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        };
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum SortColumn {
    #[default]
    Name,
    Modified,
    Size,
}

#[derive(Default)]
pub(super) struct Columns {
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
    let mut constraints = Vec::with_capacity(4); // Pre-allocate for potential max columns
    let mut name_column_width = width;

    // Add columns in order of priority based on available width
    let mut min_width = NAME_MIN_LEN;

    // Add Modified column if there's enough space
    if width > min_width {
        constraints.push(Constraint::Length(MODIFIED_LEN));
        name_column_width = width - MODIFIED_LEN - 1; // 1 for the cell padding
        min_width += MODIFIED_LEN + 1;

        // Add Size column if there's enough space
        if width > min_width + SIZE_LEN + 1 {
            constraints.push(Constraint::Length(SIZE_LEN));
            name_column_width -= SIZE_LEN + 1;
            min_width += SIZE_LEN + 1;

            // Add Mode column if there's enough space
            if width > min_width + MODE_LEN + 1 {
                constraints.push(Constraint::Length(MODE_LEN));
                name_column_width -= MODE_LEN + 1;
            }
        }
    }

    // Name column is always first
    constraints.insert(0, Constraint::Length(name_column_width));
    (constraints, name_column_width)
}
