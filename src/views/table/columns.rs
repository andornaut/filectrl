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

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    // --- calculate_constraints ---

    // Column count at each threshold boundary (strictly-greater comparisons)
    #[test_case(NAME_MIN_LEN,     1; "at or below min width: name only")]
    #[test_case(NAME_MIN_LEN + 1, 2; "one above min: modified added")]
    #[test_case(60,               2; "at size threshold: no size yet")]
    #[test_case(61,               3; "one above size threshold: size added")]
    #[test_case(71,               3; "at mode threshold: no mode yet")]
    #[test_case(72,               4; "one above mode threshold: mode added")]
    fn column_count_for_width(width: u16, expected_count: usize) {
        let (constraints, _) = calculate_constraints(width);
        assert_eq!(expected_count, constraints.len());
    }

    #[test]
    fn at_min_width_name_column_fills_the_full_width() {
        let (constraints, name_width) = calculate_constraints(NAME_MIN_LEN);
        assert_eq!(Constraint::Length(NAME_MIN_LEN), constraints[0]);
        assert_eq!(NAME_MIN_LEN, name_width);
    }

    #[test]
    fn modified_column_has_correct_length_and_name_shrinks() {
        let (constraints, name_width) = calculate_constraints(NAME_MIN_LEN + 1);
        assert_eq!(Constraint::Length(MODIFIED_LEN), constraints[1]);
        assert_eq!(NAME_MIN_LEN + 1 - MODIFIED_LEN - 1, name_width);
    }

    #[test]
    fn size_column_has_correct_length_and_name_shrinks() {
        let (constraints, name_width) = calculate_constraints(61);
        assert_eq!(Constraint::Length(SIZE_LEN), constraints[2]);
        assert_eq!(40, name_width);
    }

    #[test]
    fn mode_column_has_correct_length_and_name_shrinks() {
        let (constraints, name_width) = calculate_constraints(72);
        assert_eq!(Constraint::Length(MODE_LEN), constraints[3]);
        assert_eq!(40, name_width);
    }

    // --- sort_by ---

    #[test]
    fn clicking_same_column_twice_toggles_sort_direction() {
        let mut cols = Columns::default();
        assert_eq!(&SortDirection::Ascending, cols.sort_direction());
        cols.sort_by(SortColumn::Name);
        assert_eq!(&SortDirection::Descending, cols.sort_direction());
        cols.sort_by(SortColumn::Name);
        assert_eq!(&SortDirection::Ascending, cols.sort_direction());
    }

    #[test]
    fn switching_to_a_new_column_preserves_sort_direction() {
        // Direction is not reset when changing columns — it carries over
        let mut cols = Columns::default();
        cols.sort_by(SortColumn::Name); // toggle to Descending
        cols.sort_by(SortColumn::Modified); // switch column
        assert_eq!(&SortColumn::Modified, cols.sort_column());
        assert_eq!(&SortDirection::Descending, cols.sort_direction());
    }

    // --- sort_column_for_click ---

    #[test]
    fn click_within_name_width_maps_to_name_column() {
        let mut cols = Columns::default();
        cols.constraints(72); // name_width = 40
        assert_eq!(Some(SortColumn::Name), cols.sort_column_for_click(0));
        assert_eq!(Some(SortColumn::Name), cols.sort_column_for_click(40));
    }

    #[test]
    fn click_past_name_into_modified_column() {
        let mut cols = Columns::default();
        cols.constraints(72); // name_width = 40; modified ends at 40 + 12 = 52
        assert_eq!(Some(SortColumn::Modified), cols.sort_column_for_click(41));
        assert_eq!(Some(SortColumn::Modified), cols.sort_column_for_click(52));
    }

    #[test]
    fn click_past_modified_into_size_column() {
        let mut cols = Columns::default();
        cols.constraints(72); // size ends at 40 + 12 + 7 = 59
        assert_eq!(Some(SortColumn::Size), cols.sort_column_for_click(53));
        assert_eq!(Some(SortColumn::Size), cols.sort_column_for_click(59));
    }

    #[test]
    fn click_on_mode_column_returns_none_because_it_is_not_sortable() {
        let mut cols = Columns::default();
        cols.constraints(72); // mode starts at x = 60
        assert_eq!(None, cols.sort_column_for_click(60));
    }
}
