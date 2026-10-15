//! An inline list for choosing one option with the arrow keys.
//!
//! The picker's state and key handling are pure, so they are tested without a
//! terminal. Drawing turns the state into styled rows for the caller to place.
use super::styled::{Role, Styled};

/// What a choice is for, so the caller knows which setting to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Purpose {
    Model,
    Effort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PickerKey {
    Up,
    Down,
    Home,
    End,
    Choose,
    Cancel,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Open,
    Chosen(String),
    Cancelled,
}

/// What sits above the input while a choice is pending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Panel {
    /// Waiting for options, such as the provider's model list.
    Loading(String),
    Choose(Picker),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Picker {
    pub(super) purpose: Purpose,
    title: String,
    options: Vec<String>,
    current: Option<usize>,
    selected: usize,
}

impl Picker {
    /// `None` when there is nothing to choose from. The current option, when
    /// present, starts selected.
    pub(super) fn new(
        purpose: Purpose,
        title: &str,
        options: Vec<String>,
        current: &str,
    ) -> Option<Self> {
        if options.is_empty() {
            return None;
        }
        let current = options.iter().position(|option| option == current);
        Some(Self {
            purpose,
            title: title.to_owned(),
            selected: current.unwrap_or(0),
            current,
            options,
        })
    }

    /// Up and Down wrap around the ends; Home and End jump to them.
    pub(super) fn handle(&mut self, key: PickerKey) -> Outcome {
        let last = self.options.len() - 1;
        match key {
            PickerKey::Up => self.selected = self.selected.checked_sub(1).unwrap_or(last),
            PickerKey::Down => {
                self.selected = if self.selected == last {
                    0
                } else {
                    self.selected + 1
                }
            }
            PickerKey::Home => self.selected = 0,
            PickerKey::End => self.selected = last,
            PickerKey::Choose => return Outcome::Chosen(self.options[self.selected].clone()),
            PickerKey::Cancel => return Outcome::Cancelled,
        }
        Outcome::Open
    }

    /// Rows that fit in `height`: a title, then a window of options that
    /// scrolls to keep the selection visible. The selection has a `›` marker,
    /// so it survives without colour.
    pub(super) fn rows(&self, height: usize) -> Vec<Styled> {
        let visible = height.saturating_sub(1).max(1).min(self.options.len());
        let first = self
            .selected
            .saturating_sub(visible - 1)
            .min(self.options.len() - visible);
        let position = if visible < self.options.len() {
            format!(" ({}/{})", self.selected + 1, self.options.len())
        } else {
            String::new()
        };
        let mut rows =
            vec![Styled::default().with(Role::System, &format!("{}{position}", self.title))];
        for (index, option) in self.options.iter().enumerate().skip(first).take(visible) {
            let row = if index == self.selected {
                Styled::default().with(Role::Prompt, &format!("› {option}"))
            } else {
                Styled::default().with(Role::Plain, &format!("  {option}"))
            };
            rows.push(if Some(index) == self.current {
                row.with(Role::Muted, " (current)")
            } else {
                row
            });
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker(current: &str) -> Picker {
        let options = ["a", "b", "c", "d"].map(String::from).to_vec();
        Picker::new(Purpose::Model, "Choose", options, current).unwrap()
    }

    fn text(rows: &[Styled]) -> Vec<String> {
        rows.iter()
            .map(|row| row.runs.iter().map(|(_, text)| text.as_str()).collect())
            .collect()
    }

    #[test]
    fn the_current_option_starts_selected_and_marked() {
        let picker = picker("c");
        assert_eq!(
            text(&picker.rows(10)),
            ["Choose", "  a", "  b", "› c (current)", "  d"]
        );
        assert_eq!(
            super::super::styled::Role::Prompt,
            picker.rows(10)[3].runs[0].0
        );
    }

    #[test]
    fn an_unknown_current_option_selects_the_first_and_marks_none() {
        assert_eq!(text(&picker("zz").rows(10))[1], "› a");
        assert!(Picker::new(Purpose::Effort, "Choose", vec![], "a").is_none());
    }

    #[test]
    fn arrows_wrap_and_home_end_jump() {
        let mut picker = picker("a");
        assert_eq!(picker.handle(PickerKey::Up), Outcome::Open);
        assert_eq!(
            picker.handle(PickerKey::Choose),
            Outcome::Chosen("d".into())
        );
        picker.handle(PickerKey::Down);
        assert_eq!(
            picker.handle(PickerKey::Choose),
            Outcome::Chosen("a".into())
        );
        picker.handle(PickerKey::End);
        assert_eq!(
            picker.handle(PickerKey::Choose),
            Outcome::Chosen("d".into())
        );
        picker.handle(PickerKey::Home);
        picker.handle(PickerKey::Down);
        assert_eq!(
            picker.handle(PickerKey::Choose),
            Outcome::Chosen("b".into())
        );
        assert_eq!(picker.handle(PickerKey::Cancel), Outcome::Cancelled);
    }

    #[test]
    fn long_lists_scroll_to_keep_the_selection_visible() {
        let mut picker = picker("a");
        assert_eq!(
            text(&picker.rows(3)),
            ["Choose (1/4)", "› a (current)", "  b"]
        );
        picker.handle(PickerKey::End);
        assert_eq!(text(&picker.rows(3)), ["Choose (4/4)", "  c", "› d"]);
        picker.handle(PickerKey::Up);
        picker.handle(PickerKey::Up);
        assert_eq!(
            text(&picker.rows(3)),
            ["Choose (2/4)", "  a (current)", "› b"]
        );
        // Even with no room, the selection stays on screen.
        assert_eq!(text(&picker.rows(0)), ["Choose (2/4)", "› b"]);
    }
}
