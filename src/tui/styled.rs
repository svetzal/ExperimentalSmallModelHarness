//! Styled text: roles, the colour palette, and Unicode-aware wrapping.
use crossterm::style::{Attribute, Color, ContentStyle};
use unicode_width::UnicodeWidthChar;

/// What a run of text means on screen. Every role stays readable without colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Plain,
    Muted,
    UserLabel,
    AgentLabel,
    Success,
    Failure,
    Model,
    Effort,
    Folder,
    Branch,
    Dirty,
    /// The launch wordmark and its brightest sparks.
    Wordmark,
    Spark,
    SparkDim,
    /// A letter about to land, before it takes the wordmark colour.
    Flash,
    Thinking,
    /// A path to copy, such as the evidence trace: wrapped, never shortened.
    Evidence,
    Error,
    Prompt,
    System,
    /// Context panel: an element the previous request did not have.
    Fresh,
    /// Context panel: text the harness summarized, merged, or left out,
    /// rather than what the model or user wrote.
    Rewritten,
    Ready,
    Working,
    Stopped,
}

/// Terminal styling, decided once at startup.
///
/// Named ANSI colours follow the user's terminal theme, so the palette works on
/// light and dark backgrounds. The normal-intensity colours are used because the
/// bright variants (bright yellow, bright cyan) wash out on light backgrounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Palette {
    pub(super) colour: bool,
}

impl Palette {
    /// `NO_COLOR` (<https://no-color.org>) disables colour when present and
    /// non-empty. Attributes such as bold and dim remain, as the convention allows.
    pub(super) fn from_no_color(value: Option<&std::ffi::OsStr>) -> Self {
        Self {
            colour: value.is_none_or(|value| value.is_empty()),
        }
    }

    pub(super) fn style(self, role: Role) -> ContentStyle {
        let (colour, attributes): (Option<Color>, &[Attribute]) = match role {
            Role::Plain => (None, &[]),
            Role::AgentLabel | Role::Wordmark => (Some(Color::DarkMagenta), &[Attribute::Bold]),
            Role::Spark => (Some(Color::DarkMagenta), &[]),
            Role::SparkDim => (Some(Color::DarkMagenta), &[Attribute::Dim]),
            // Bold default foreground: bright on dark themes, dark on light ones.
            Role::Flash => (None, &[Attribute::Bold]),
            Role::UserLabel | Role::Prompt => (Some(Color::DarkCyan), &[Attribute::Bold]),
            Role::Muted | Role::Evidence => (None, &[Attribute::Dim]),
            Role::System => (None, &[Attribute::Italic]),
            // The model's thinking: dim and italic, so it reads apart from replies
            // with or without colour.
            Role::Thinking => (None, &[Attribute::Dim, Attribute::Italic]),
            Role::Success | Role::Fresh => (Some(Color::DarkGreen), &[Attribute::Bold]),
            Role::Rewritten => (Some(Color::DarkYellow), &[Attribute::Italic]),
            Role::Model => (Some(Color::DarkGreen), &[]),
            Role::Effort => (Some(Color::DarkYellow), &[]),
            Role::Folder => (None, &[Attribute::Bold]),
            Role::Branch => (Some(Color::DarkCyan), &[]),
            // Orange has no named ANSI colour; 208 reads on light and dark themes.
            Role::Dirty => (Some(Color::AnsiValue(208)), &[]),
            Role::Failure => (Some(Color::DarkRed), &[Attribute::Bold]),
            Role::Error => (Some(Color::DarkRed), &[]),
            Role::Ready => (Some(Color::DarkGreen), &[Attribute::Bold]),
            Role::Working => (Some(Color::DarkYellow), &[Attribute::Bold]),
            Role::Stopped => (Some(Color::DarkRed), &[Attribute::Bold]),
        };
        let mut style = ContentStyle::new();
        if self.colour {
            style.foreground_color = colour;
        }
        for attribute in attributes {
            style.attributes.set(*attribute);
        }
        style
    }
}

/// Text made of runs that each carry a role. Adjacent text with the same role
/// shares one run, so streamed reply chunks extend the reply in place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Styled {
    pub(super) runs: Vec<(Role, String)>,
}

impl Styled {
    pub(super) fn push(&mut self, role: Role, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some((last, existing)) if *last == role => existing.push_str(text),
            _ => self.runs.push((role, text.to_owned())),
        }
    }

    pub(super) fn with(mut self, role: Role, text: &str) -> Self {
        self.push(role, text);
        self
    }
}

/// One screen row of styled runs.
pub(super) type Row = Vec<(Role, String)>;

/// Split styled text into rows of at most `width` terminal cells, breaking
/// anywhere. The input line uses this, so every typed character stays
/// visible; conversation text uses the word wrapping in `wrap`. Newlines end
/// rows, other control characters are removed, and roles stay attached to
/// their characters across row breaks.
pub(super) fn wrap_chars(text: &Styled, width: usize) -> Vec<Row> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = Row::new();
    let mut cells = 0;
    for (role, run) in &text.runs {
        for ch in run.chars() {
            if ch == '\n' {
                rows.push(std::mem::take(&mut row));
                cells = 0;
                continue;
            }
            if ch.is_control() {
                continue;
            }
            let size = ch.width().unwrap_or(0);
            if cells + size > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                cells = 0;
            }
            match row.last_mut() {
                Some((last, existing)) if last == role => existing.push(ch),
                _ => row.push((*role, ch.to_string())),
            }
            cells += size;
        }
    }
    rows.push(row);
    rows
}

/// Row text without roles, for layout assertions in tests.
#[cfg(test)]
pub(super) fn rows_text(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| row.iter().map(|(_, text)| text.as_str()).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Row text without roles, for layout assertions.
    fn wrapped(text: &str, width: usize) -> Vec<String> {
        rows_text(&wrap_chars(
            &Styled::default().with(Role::Plain, text),
            width,
        ))
    }

    #[test]
    fn input_wrapping_breaks_anywhere_by_cells_and_removes_control_sequences() {
        assert_eq!(wrapped("界a界", 3), ["界a", "界"]);
        assert_eq!(wrapped("abc\ndef", 2), ["ab", "c", "de", "f"]);
        assert!(!wrapped("\u{1b}[2J", 10)[0].contains('\u{1b}'));
    }

    #[test]
    fn input_wrapping_counts_cells_across_runs_and_keeps_each_role() {
        let text = Styled::default()
            .with(Role::UserLabel, "You:")
            .with(Role::Plain, " 界界\nok");
        let rows = wrap_chars(&text, 7);
        assert_eq!(rows_text(&rows), ["You: 界", "界", "ok"]);
        assert_eq!(
            rows[0],
            [
                (Role::UserLabel, "You:".to_owned()),
                (Role::Plain, " 界".to_owned())
            ]
        );
        assert_eq!(rows[1], [(Role::Plain, "界".to_owned())]);
    }

    #[test]
    fn no_color_disables_colour_only_when_present_and_nonempty() {
        use std::ffi::OsStr;
        assert!(Palette::from_no_color(None).colour);
        assert!(Palette::from_no_color(Some(OsStr::new(""))).colour);
        assert!(!Palette::from_no_color(Some(OsStr::new("1"))).colour);
    }

    #[test]
    fn monochrome_styles_keep_attributes_without_colour() {
        let mono = Palette { colour: false };
        let colour = Palette { colour: true };
        for role in [Role::AgentLabel, Role::UserLabel, Role::Error, Role::Ready] {
            assert!(colour.style(role).foreground_color.is_some(), "{role:?}");
            assert!(mono.style(role).foreground_color.is_none(), "{role:?}");
        }
        assert!(mono.style(Role::AgentLabel).attributes.has(Attribute::Bold));
        assert!(mono.style(Role::Muted).attributes.has(Attribute::Dim));
        assert_eq!(colour.style(Role::Plain), ContentStyle::new());
    }
}
