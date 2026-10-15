//! Styled text: roles, the colour palette, and Unicode-aware wrapping.
use crossterm::style::{Attribute, Color, ContentStyle};
use unicode_width::UnicodeWidthChar;

/// What a run of text means on screen. Every role stays readable without colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Plain,
    Title,
    Muted,
    Value,
    UserLabel,
    AgentLabel,
    Warning,
    Error,
    Prompt,
    System,
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
        let (colour, attribute) = match role {
            Role::Plain => (None, None),
            Role::Value => (None, Some(Attribute::Bold)),
            Role::Title | Role::AgentLabel => (Some(Color::DarkMagenta), Some(Attribute::Bold)),
            Role::UserLabel | Role::Prompt => (Some(Color::DarkCyan), Some(Attribute::Bold)),
            Role::Muted => (None, Some(Attribute::Dim)),
            Role::System => (None, Some(Attribute::Italic)),
            Role::Warning => (Some(Color::DarkYellow), None),
            Role::Error => (Some(Color::DarkRed), None),
            Role::Ready => (Some(Color::DarkGreen), Some(Attribute::Bold)),
            Role::Working => (Some(Color::DarkYellow), Some(Attribute::Bold)),
            Role::Stopped => (Some(Color::DarkRed), Some(Attribute::Bold)),
        };
        let mut style = ContentStyle::new();
        if self.colour {
            style.foreground_color = colour;
        }
        if let Some(attribute) = attribute {
            style.attributes.set(attribute);
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

/// Split styled text into rows of at most `width` terminal cells. Newlines end
/// rows, other control characters are removed, and roles stay attached to
/// their characters across row breaks.
pub(super) fn wrap(text: &Styled, width: usize) -> Vec<Row> {
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

pub(super) fn first_row(text: &Styled, width: usize) -> Row {
    wrap(text, width).swap_remove(0)
}

/// Drop leading characters so one line fits `width` cells, marking the cut
/// with "…". A long project path then gives way before the model and effort.
pub(super) fn fit_from_end(text: &Styled, width: usize) -> Styled {
    let cells = |run: &str| run.chars().map(|ch| ch.width().unwrap_or(0)).sum::<usize>();
    let total: usize = text.runs.iter().map(|(_, run)| cells(run)).sum();
    let Some((first_role, _)) = text.runs.first() else {
        return text.clone();
    };
    if total <= width || width < 2 {
        return text.clone();
    }
    let mut excess = total - (width - 1);
    let mut fitted = Styled::default().with(*first_role, "…");
    for (role, run) in &text.runs {
        let kept: String = run
            .chars()
            .skip_while(|ch| {
                let drop = excess > 0;
                excess = excess.saturating_sub(ch.width().unwrap_or(0));
                drop
            })
            .collect();
        fitted.push(*role, &kept);
    }
    fitted
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
        rows_text(&wrap(&Styled::default().with(Role::Plain, text), width))
    }

    #[test]
    fn wrapping_tracks_terminal_cells_and_removes_control_sequences() {
        assert_eq!(wrapped("界a界", 3), ["界a", "界"]);
        assert_eq!(wrapped("abc\ndef", 2), ["ab", "c", "de", "f"]);
        assert!(!wrapped("\u{1b}[2J", 10)[0].contains('\u{1b}'));
    }

    #[test]
    fn wrapping_counts_cells_across_runs_and_keeps_each_role() {
        let text = Styled::default()
            .with(Role::UserLabel, "You:")
            .with(Role::Plain, " 界界\nok");
        let rows = wrap(&text, 7);
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
        for role in [Role::Title, Role::UserLabel, Role::Error, Role::Ready] {
            assert!(colour.style(role).foreground_color.is_some(), "{role:?}");
            assert!(mono.style(role).foreground_color.is_none(), "{role:?}");
        }
        assert!(mono.style(Role::Title).attributes.has(Attribute::Bold));
        assert!(mono.style(Role::Muted).attributes.has(Attribute::Dim));
        assert_eq!(colour.style(Role::Plain), ContentStyle::new());
    }

    #[test]
    fn long_headers_lose_the_start_of_the_path_first() {
        let header = Styled::default()
            .with(Role::Muted, "/a/long/project | omlx | ")
            .with(Role::Value, "m1");
        let fitted = fit_from_end(&header, 16);
        assert_eq!(rows_text(&wrap(&fitted, 16)), ["…ect | omlx | m1"]);
        assert_eq!(fitted.runs.last(), Some(&(Role::Value, "m1".to_owned())));
        assert_eq!(fit_from_end(&header, 80), header);
        // A wide character that straddles the cut is dropped whole.
        let wide = Styled::default().with(Role::Muted, "界界ab");
        assert_eq!(rows_text(&wrap(&fit_from_end(&wide, 4), 4)), ["…ab"]);
    }
}
