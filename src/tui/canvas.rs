//! A screen-sized grid of styled cells.
//!
//! Layout composes the screen into a canvas, the launch animation overlays
//! it, and painting turns it into terminal output. Composing and overlaying
//! are pure; only `paint` writes to the terminal.
use super::styled::{Palette, Role};
use crossterm::{
    cursor::MoveTo,
    queue,
    style::PrintStyledContent,
    terminal::{Clear, ClearType},
};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cell {
    /// Never written; painting skips trailing empty cells.
    Empty,
    Char(char, Role),
    /// The right half of a double-width character.
    Continuation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Canvas {
    cols: usize,
    cells: Vec<Vec<Cell>>,
}

impl Canvas {
    pub(super) fn new(rows: usize, cols: usize) -> Self {
        Self {
            cols,
            cells: vec![vec![Cell::Empty; cols]; rows],
        }
    }

    pub(super) fn rows(&self) -> usize {
        self.cells.len()
    }

    pub(super) fn cols(&self) -> usize {
        self.cols
    }

    /// Write styled runs from `col`, clipping at the right edge. Characters
    /// without width, such as controls, are skipped.
    pub(super) fn put(&mut self, row: usize, col: usize, runs: &[(Role, String)]) {
        let Some(cells) = self.cells.get_mut(row) else {
            return;
        };
        let mut col = col;
        for (role, text) in runs {
            for ch in text.chars() {
                let width = ch.width().unwrap_or(0);
                if width == 0 {
                    continue;
                }
                if col + width > self.cols {
                    return;
                }
                cells[col] = Cell::Char(ch, *role);
                if width == 2 {
                    cells[col + 1] = Cell::Continuation;
                }
                col += width;
            }
        }
    }

    pub(super) fn put_str(&mut self, row: usize, col: usize, text: &str, role: Role) {
        self.put(row, col, &[(role, text.to_owned())]);
    }

    /// Copy one row from another canvas of the same width.
    pub(super) fn copy_row(&mut self, from: &Canvas, row: usize) {
        if let (Some(target), Some(source)) = (self.cells.get_mut(row), from.cells.get(row)) {
            target.clone_from(source);
        }
    }

    /// The row's characters, with empty cells as spaces and trailing ones dropped.
    #[cfg(test)]
    pub(super) fn row_text(&self, row: usize) -> String {
        let text: String = self.cells[row]
            .iter()
            .filter_map(|cell| match cell {
                Cell::Empty => Some(' '),
                Cell::Char(ch, _) => Some(*ch),
                Cell::Continuation => None,
            })
            .collect();
        text.trim_end().to_owned()
    }

    /// The role of the character at a cell, if one starts there.
    #[cfg(test)]
    pub(super) fn role_at(&self, row: usize, col: usize) -> Option<Role> {
        match self.cells.get(row)?.get(col)? {
            Cell::Char(_, role) => Some(*role),
            _ => None,
        }
    }

    /// The first cell where `text` starts, searching row by row.
    pub(super) fn find(&self, text: &str) -> Option<(usize, usize)> {
        let wanted: Vec<char> = text.chars().collect();
        for (row, cells) in self.cells.iter().enumerate() {
            let starts: Vec<(usize, char)> = cells
                .iter()
                .enumerate()
                .filter_map(|(col, cell)| match cell {
                    Cell::Char(ch, _) => Some((col, *ch)),
                    Cell::Empty => Some((col, ' ')),
                    Cell::Continuation => None,
                })
                .collect();
            if let Some(index) = starts
                .windows(wanted.len())
                .position(|window| window.iter().map(|(_, ch)| *ch).eq(wanted.iter().copied()))
            {
                return Some((row, starts[index].0));
            }
        }
        None
    }

    /// Recolour the characters in `[col, col + len)` of a row.
    pub(super) fn restyle(&mut self, row: usize, col: usize, len: usize, role: Role) {
        if let Some(cells) = self.cells.get_mut(row) {
            for cell in cells.iter_mut().skip(col).take(len) {
                if let Cell::Char(ch, _) = cell {
                    *cell = Cell::Char(*ch, role);
                }
            }
        }
    }

    /// Terminal output that clears the screen and draws every written cell.
    pub(super) fn paint(&self, palette: Palette) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
        for (row, cells) in self.cells.iter().enumerate() {
            let Some(first) = cells.iter().position(|cell| *cell != Cell::Empty) else {
                continue;
            };
            let last = cells
                .iter()
                .rposition(|cell| *cell != Cell::Empty)
                .unwrap_or(first);
            queue!(out, MoveTo(first as u16, row as u16))?;
            let mut run: Option<(Role, String)> = None;
            for cell in &cells[first..=last] {
                let (ch, role) = match cell {
                    Cell::Empty => (' ', Role::Plain),
                    Cell::Char(ch, role) => (*ch, *role),
                    Cell::Continuation => continue,
                };
                match &mut run {
                    Some((current, text)) if *current == role => text.push(ch),
                    _ => {
                        if let Some((current, text)) = run.take() {
                            queue!(out, PrintStyledContent(palette.style(current).apply(text)))?;
                        }
                        run = Some((role, ch.to_string()));
                    }
                }
            }
            if let Some((current, text)) = run {
                queue!(out, PrintStyledContent(palette.style(current).apply(text)))?;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_characters_take_two_cells_and_clip_at_the_edge() {
        let mut canvas = Canvas::new(2, 6);
        canvas.put_str(0, 1, "🌿 ab", Role::Plain);
        assert_eq!(canvas.row_text(0), " 🌿 ab");
        canvas.put_str(1, 4, "界界", Role::Plain);
        assert_eq!(canvas.row_text(1), "    界");
        assert_eq!(canvas.find("ab"), Some((0, 4)));
    }

    #[test]
    fn painting_skips_unwritten_edges_and_groups_runs() {
        let mut canvas = Canvas::new(2, 10);
        canvas.put(
            0,
            1,
            &[
                (Role::UserLabel, "You:".into()),
                (Role::Plain, " hi".into()),
            ],
        );
        let mono = String::from_utf8(canvas.paint(Palette { colour: false }).unwrap()).unwrap();
        assert!(mono.contains("\u{1b}[1;2H"), "{mono:?}");
        assert!(mono.contains("You:") && mono.contains(" hi"));
        assert!(!mono.contains("\u{1b}[2;"), "an empty row was painted");
        let colour = String::from_utf8(canvas.paint(Palette { colour: true }).unwrap()).unwrap();
        assert!(colour.contains("38;5;"));
    }
}
