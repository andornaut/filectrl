use std::path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path};

use ratatui::buffer::CellWidth;
use ratatui::{
    Frame,
    crossterm::event::{MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::View;
use crate::{
    app::config::Config,
    command::{Command, handler::CommandHandler, result::CommandResult},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct BreadcrumbsView {
    breadcrumbs: Vec<String>,
    is_bookmarks: bool,
    is_searching: bool,
    area: Rect,
    positions: Vec<Vec<Position>>,
}

fn path_breadcrumbs(path: &Path) -> Vec<String> {
    let mut parts: Vec<_> = path
        .ancestors()
        .map(|p| {
            p.file_name()
                .map_or(String::new(), |n| n.to_string_lossy().into_owned())
        })
        .collect();
    parts.reverse();
    parts
}

impl BreadcrumbsView {
    fn display_breadcrumbs(&self) -> Vec<String> {
        if self.is_bookmarks {
            let mut display = vec!["[Bookmarks] ".to_string()];
            display.extend(self.breadcrumbs.iter().cloned());
            display
        } else if self.is_searching {
            let mut display = vec!["[Search] ".to_string()];
            display.extend(self.breadcrumbs.iter().cloned());
            display
        } else {
            self.breadcrumbs.clone()
        }
    }

    fn height(&self, width: u16) -> u16 {
        // Calculate height based on content length and width, without theme styling
        let (container, _) = spans(
            &self.display_breadcrumbs(),
            width,
            None,
            Style::default(),
            Style::default(),
            Style::default(),
        );
        container.len() as u16
    }

    fn set_directory(&mut self, directory: PathInfo) -> CommandResult {
        self.breadcrumbs = directory.breadcrumbs();
        self.is_bookmarks = false;
        CommandResult::Handled
    }

    fn to_path(&self, end_index: usize) -> Option<PathInfo> {
        if let Some(components) = self.breadcrumbs.get(0..=end_index) {
            let path = if components.len() == 1 {
                // Clicked on the root element, which is empty string
                MAIN_SEPARATOR.to_string()
            } else {
                components.join(std::path::MAIN_SEPARATOR_STR)
            };
            PathInfo::try_from(path).ok()
        } else {
            None
        }
    }
}

impl CommandHandler for BreadcrumbsView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::NavigatedDirectory { directory, .. } => {
                self.set_directory(directory.clone());
                self.is_searching = false;
                CommandResult::Handled
            }
            Command::RefreshedDirectory { directory, .. } => self.set_directory(directory.clone()),
            Command::StartSearch(_) => {
                self.is_searching = true;
                CommandResult::Handled
            }
            Command::ResetView => {
                self.is_searching = false;
                CommandResult::Handled
            }
            Command::Bookmarks { .. } => {
                let dir = Config::global().bookmarks_dir();
                self.breadcrumbs = path_breadcrumbs(&dir);
                self.is_bookmarks = true;
                self.positions.clear();
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let x = event.column.saturating_sub(self.area.x);
                let y = event.row.saturating_sub(self.area.y);
                // Positions are populated in render(); guard against a stale area or a
                // mouse event arriving before the first render.
                let Some(row) = self.positions.get(y as usize) else {
                    return CommandResult::Handled;
                };
                let has_tag = self.is_bookmarks || self.is_searching;
                let clicked_index = row.iter().find_map(|p| {
                    if p.intersects(x) {
                        let i = p.index();
                        if has_tag {
                            if i == 0 { None } else { Some(i - 1) }
                        } else {
                            Some(i)
                        }
                    } else {
                        None
                    }
                });
                if let Some(path) = clicked_index.and_then(|i| self.to_path(i)) {
                    Command::Open(path).into()
                } else {
                    CommandResult::Handled
                }
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        self.area.contains(ratatui::layout::Position {
            x: event.column,
            y: event.row,
        })
    }
}

impl View for BreadcrumbsView {
    fn constraint(&self, area: Rect) -> Constraint {
        Constraint::Length(self.height(area.width))
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.area = area;
        let theme = Config::global().theme();
        let display = self.display_breadcrumbs();

        let tag_style = if self.is_bookmarks {
            Some(theme.breadcrumbs.bookmarks())
        } else if self.is_searching {
            Some(theme.breadcrumbs.search())
        } else {
            None
        };
        let (mut container, mut positions) = spans(
            &display,
            self.area.width,
            tag_style,
            theme.breadcrumbs.basename(),
            theme.breadcrumbs.ancestor(),
            theme.breadcrumbs.separator(),
        );

        // Prioritize displaying the deepest directories.
        // positions.len() >= area.height always holds: constraint() requests exactly
        // self.height() rows, and the layout engine never allocates more than requested.
        // This invariant is relied upon by handle_mouse, which indexes into self.positions
        // using a y offset guaranteed to be < self.area.height by should_handle_mouse.
        debug_assert!(
            positions.len() >= self.area.height as usize,
            "layout allocated more height than the header requested"
        );
        let at = positions.len().saturating_sub(self.area.height as usize);
        let container = container.split_off(at);
        self.positions = positions.split_off(at);

        let text: Vec<_> = container.into_iter().map(Line::from).collect();

        let widget = Paragraph::new(text).style(theme.breadcrumbs.base());
        widget.render(self.area, frame.buffer_mut());
    }
}

#[derive(Debug)]
struct Position {
    x_start: u16,
    x_end: u16, // inclusive end of the name; excludes the trailing separator
    index: usize,
}

impl Position {
    fn intersects(&self, x: u16) -> bool {
        x >= self.x_start && x <= self.x_end
    }

    fn index(&self) -> usize {
        self.index
    }
}

fn spans<'a>(
    breadcrumbs: &[String],
    width: u16,
    tag_style: Option<Style>,
    basename_style: Style,
    ancestor_style: Style,
    separator_style: Style,
) -> (Vec<Vec<Span<'a>>>, Vec<Vec<Position>>) {
    let mut container: Vec<Vec<Span<'a>>> = Vec::new();
    let mut positions: Vec<Vec<Position>> = Vec::new();
    let mut row_len: u16 = 0;

    let mut it = breadcrumbs.iter().enumerate().peekable();
    while let Some((i, name)) = it.next() {
        let is_last = it.peek().is_none();
        let is_tag = i == 0 && tag_style.is_some();
        let name_style = if is_tag {
            tag_style.unwrap()
        } else if is_last {
            basename_style
        } else {
            ancestor_style
        };

        let display_name = if is_last && name.is_empty() {
            MAIN_SEPARATOR_STR
        } else {
            name
        };
        let name_len = display_name.cell_width();
        // Tags and the last entry have no trailing separator. Path components
        // between them occupy name_len + 1 columns (name + separator).
        let entry_len = name_len + if is_last || is_tag { 0 } else { 1 };

        if container.is_empty() || (row_len + entry_len > width && row_len > 0) {
            row_len = 0;
            container.push(Vec::new());
            positions.push(Vec::new());
        }

        let x_start = row_len;
        let x_end = row_len + name_len.saturating_sub(1);
        row_len += entry_len;

        let container_row = container.last_mut().unwrap();
        container_row.push(Span::styled(display_name.to_owned(), name_style));
        if !is_last && !is_tag {
            container_row.push(Span::styled(MAIN_SEPARATOR_STR, separator_style));
        }

        let positions_row = positions.last_mut().unwrap();
        positions_row.push(Position {
            x_start,
            x_end,
            index: i,
        });
    }
    (container, positions)
}

#[cfg(test)]
mod tests {
    use ratatui::style::Style;
    use test_case::test_case;

    use super::{MAIN_SEPARATOR, spans};

    fn bc(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    const SEP: &str = if MAIN_SEPARATOR == '/' { "/" } else { "\\" };

    fn run_spans(parts: &[&str], width: u16) -> (Vec<Vec<String>>, Vec<Vec<super::Position>>) {
        let (rows, positions) = spans(
            &bc(parts),
            width,
            None,
            Style::default(),
            Style::default(),
            Style::default(),
        );
        let content = rows
            .into_iter()
            .map(|row| row.into_iter().map(|s| s.content.into_owned()).collect())
            .collect();
        (content, positions)
    }

    fn run_tagged_spans(
        parts: &[&str],
        width: u16,
        tag_style: Style,
    ) -> (Vec<Vec<String>>, Vec<Vec<super::Position>>) {
        let (rows, positions) = spans(
            &bc(parts),
            width,
            Some(tag_style),
            Style::default(),
            Style::default(),
            Style::default(),
        );
        let content = rows
            .into_iter()
            .map(|row| row.into_iter().map(|s| s.content.into_owned()).collect())
            .collect();
        (content, positions)
    }

    // ── tag display ───────────────────────────────────────────────────────────

    #[test]
    fn tagged_breadcrumb_includes_tag_without_trailing_separator() {
        let (rows, _) = run_tagged_spans(&["[Search] ", "home", "user"], 80, Style::default());
        assert_eq!(
            rows,
            vec![vec![
                "[Search] ".to_string(),
                "home".to_string(),
                SEP.to_string(),
                "user".to_string()
            ]]
        );
    }

    #[test]
    fn tagged_breadcrumb_shows_root_separator_at_last_position() {
        let (rows, _) = run_tagged_spans(&["[Search] ", ""], 80, Style::default());
        assert_eq!(rows, vec![vec!["[Search] ".to_string(), SEP.to_string()]]);
    }

    #[test]
    fn tagged_breadcrumb_skip_tag_in_click_test_on_tag_only() {
        let (_, positions) = run_tagged_spans(&["[Search] ", "home", "user"], 80, Style::default());
        // Tag at index 0, click on tag should return None
        let tag_hit = positions[0].iter().find_map(|p| {
            if p.intersects(0) {
                (p.index() == 0).then_some(())
            } else {
                None
            }
        });
        assert_eq!(tag_hit, Some(()));
    }

    // ── row count ─────────────────────────────────────────────────────────────

    #[test_case(&[], 80 => 0 ; "empty input yields no rows")]
    #[test_case(&["", "home", "user"], 80 => 1 ; "all fit in one row")]
    #[test_case(&["", "home", "user"], 1 => 3 ; "each entry on its own row when width=1")]
    fn row_count(parts: &[&str], width: u16) -> usize {
        run_spans(parts, width).0.len()
    }

    // ── span content ──────────────────────────────────────────────────────────

    #[test_case(
        &[""], 80
        => vec![vec![SEP.to_string()]]
        ; "root only displays separator"
    )]
    #[test_case(
        &["", "home", "user"], 80
        => vec![vec!["".to_string(), SEP.to_string(), "home".to_string(), SEP.to_string(), "user".to_string()]]
        ; "single row: root sep home sep user, no trailing separator"
    )]
    #[test_case(
        &["", "home", "user"], 3
        => vec![
            vec!["".to_string(), SEP.to_string()],
            vec!["home".to_string(), SEP.to_string()],
            vec!["user".to_string()],
        ]
        ; "wraps when too narrow: no trailing separator on last row"
    )]
    fn span_content(parts: &[&str], width: u16) -> Vec<Vec<String>> {
        run_spans(parts, width).0
    }

    // ── click hit-test ────────────────────────────────────────────────────────
    //
    // Layout for &["", "home", "user"] at width=80:
    //   col 0       → "" (root, width=0, x_start=0, x_end=0 via saturating_sub) + "/" sep
    //   col 1..=4   → "home" (x_start=1, x_end=4)
    //   col 5       → "/" separator
    //   col 6..=9   → "user" (x_start=6, x_end=9)

    #[test_case(&["", "home", "user"], 80, 0, 0  => Some(0) ; "click on root (col 0)")]
    #[test_case(&["", "home", "user"], 80, 0, 1  => Some(1) ; "click on first char of home")]
    #[test_case(&["", "home", "user"], 80, 0, 4  => Some(1) ; "click on last char of home")]
    #[test_case(&["", "home", "user"], 80, 0, 5  => None    ; "click on separator between home and user")]
    #[test_case(&["", "home", "user"], 80, 0, 6  => Some(2) ; "click on first char of user")]
    #[test_case(&["", "home", "user"], 80, 0, 9  => Some(2) ; "click on last char of user")]
    #[test_case(&["", "home", "user"], 80, 0, 10 => None    ; "click past end")]
    fn click_index(parts: &[&str], width: u16, row: usize, x: u16) -> Option<usize> {
        let positions = run_spans(parts, width).1;
        positions[row].iter().find_map(|p| {
            if p.intersects(x) {
                Some(p.index())
            } else {
                None
            }
        })
    }
}
