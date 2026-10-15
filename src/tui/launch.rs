//! The "burst" launch animation.
//!
//! A seed at the centre bursts into the letters of the wordmark, which glides
//! into the welcome line while the status bar sprouts below. Every frame is a
//! pure function of elapsed time and the real resting screen, so the last
//! frame is the resting screen exactly. The timeline keeps the approved
//! prototype's proportions; `PACE` sets its speed.
use super::canvas::Canvas;
use super::status_bar::join_segments;
use super::styled::{Role, Styled};
use std::ffi::OsStr;
use std::time::Duration;

const WORD: &str = "microgentic";
/// Index of the middle "g", where the burst starts.
const MID: usize = 5;
/// Fixed launch jitter per letter, in units of the burst's length. These are
/// the prototype's seeded random values, kept as a table so every run is
/// identical.
const JITTER: [f64; 11] = [
    0.0389, 0.0181, 0.0781, 0.0087, 0.0643, 0.0439, 0.007, 0.0609, 0.0045, 0.052, 0.0084,
];
const SPARKS: [&str; 4] = ["✦", "✧", "•", "·"];
const BURST_STEPS: usize = 10;
const GLIDE_STEPS: usize = 7;
/// Smaller terminals fall back to the resting screen.
const MIN_COLS: u16 = 40;
const MIN_ROWS: u16 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// The seed grows: `·`, `•`, `✧`, `✦`.
    Seed(usize),
    /// Letters fly out from the centre.
    Burst,
    /// Glints around the word, bright then dim.
    Glint { bright: bool },
    /// The word glides to its place in the welcome line.
    Glide,
    /// The welcome sentence appears around the landed word.
    Sentence,
    /// The bottom chrome appears and status segments tick in.
    Sprout,
    /// The remaining welcome lines appear.
    Lines,
}

/// Playback speed against the prototype's timeline, whose holds sum to
/// 1474 ms at 1.0; raise it to play faster, lower it to play slower. At 0.88
/// it takes 1675 ms, 25% slower than the previous 1.1 pace.
const PACE: f64 = 0.88;

/// Each stage and how long it holds in the prototype at speed 1, in
/// milliseconds. `PACE` divides every hold, so proportions never change.
const TIMELINE: [(Stage, f64); 11] = [
    (Stage::Seed(0), 140.0),
    (Stage::Seed(1), 90.0),
    (Stage::Seed(2), 70.0),
    (Stage::Seed(3), 120.0),
    (Stage::Burst, 350.0),
    (Stage::Glint { bright: true }, 60.0),
    (Stage::Glint { bright: false }, 60.0),
    (Stage::Glide, 154.0),
    (Stage::Sentence, 60.0),
    (Stage::Sprout, 280.0),
    (Stage::Lines, 90.0),
];

fn hold_ms(prototype_ms: f64) -> f64 {
    prototype_ms / PACE
}

pub(super) fn duration() -> Duration {
    Duration::from_secs_f64(TIMELINE.iter().map(|(_, ms)| hold_ms(*ms)).sum::<f64>() / 1_000.0)
}

/// When the stage at `index` in the timeline begins.
#[cfg(test)]
pub(super) fn start_of(index: usize) -> Duration {
    Duration::from_secs_f64(
        TIMELINE[..index]
            .iter()
            .map(|(_, ms)| hold_ms(*ms))
            .sum::<f64>()
            / 1_000.0,
    )
}

/// Play only where it makes sense: not when `MICROGENTIC_NO_ANIMATION` is set
/// and non-empty, not on a dumb terminal, and not where the word cannot fly.
pub(super) fn should_play(
    no_animation: Option<&OsStr>,
    term: Option<&OsStr>,
    cols: u16,
    rows: u16,
) -> bool {
    no_animation.is_none_or(OsStr::is_empty)
        && term != Some(OsStr::new("dumb"))
        && cols >= MIN_COLS
        && rows >= MIN_ROWS
}

/// The resting screen and the places the animation needs from it.
pub(super) struct Scene {
    pub(super) rest: Canvas,
    /// Rows above the rule, where the welcome text sits.
    pub(super) history_rows: usize,
    /// Rows from the rule to the input, inclusive.
    pub(super) chrome_rows: std::ops::Range<usize>,
    pub(super) bar_row: usize,
    pub(super) hint_row: usize,
    /// The status bar segments the resting screen shows, left to right.
    pub(super) segments: Vec<Styled>,
}

/// The screen at `elapsed` since launch. At or past the end it is the
/// resting screen.
pub(super) fn frame(scene: &Scene, elapsed: Duration) -> Canvas {
    let Some((stage, progress)) = stage_at(elapsed) else {
        return scene.rest.clone();
    };
    let Some(landing) = scene.rest.find("Microgentic") else {
        return scene.rest.clone();
    };
    let (rows, cols) = (scene.rest.rows(), scene.rest.cols());
    let centre = (rows / 2 - 1, cols / 2);
    let start = centre.1 - MID;
    let mut canvas = Canvas::new(rows, cols);
    match stage {
        Stage::Seed(step) => {
            let (glyph, role) = [
                ("·", Role::Muted),
                ("•", Role::Plain),
                ("✧", Role::Spark),
                ("✦", Role::Wordmark),
            ][step];
            canvas.put_str(centre.0, centre.1, glyph, role);
        }
        Stage::Burst => {
            let step = ((progress * BURST_STEPS as f64) as usize + 1).min(BURST_STEPS);
            burst(&mut canvas, centre, step);
        }
        Stage::Glint { bright } => {
            canvas.put_str(centre.0, start, WORD, Role::Wordmark);
            let role = if bright {
                Role::Wordmark
            } else {
                Role::SparkDim
            };
            for (row, col) in [
                (centre.0 - 1, start - 2),
                (centre.0 + 1, start + WORD.len() + 1),
                (centre.0 - 1, start + WORD.len()),
                (centre.0 + 1, start - 1),
            ] {
                canvas.put_str(row, col, "✧", role);
            }
        }
        Stage::Glide => {
            let step = ((progress * GLIDE_STEPS as f64) as usize + 1).min(GLIDE_STEPS);
            let t = ease(step as f64 / GLIDE_STEPS as f64);
            let row = lerp(centre.0, landing.0, t);
            let col = lerp(start, landing.1, t);
            let word = if step == GLIDE_STEPS {
                "Microgentic"
            } else {
                WORD
            };
            canvas.put_str(row, col, word, Role::Wordmark);
        }
        Stage::Sentence => {
            canvas.copy_row(&scene.rest, landing.0);
            canvas.copy_row(&scene.rest, scene.hint_row);
            canvas.restyle(landing.0, landing.1, WORD.len(), Role::Wordmark);
        }
        Stage::Sprout => {
            canvas.copy_row(&scene.rest, landing.0);
            canvas.copy_row(&scene.rest, scene.hint_row);
            for row in scene.chrome_rows.clone() {
                canvas.copy_row(&scene.rest, row);
            }
            let shown =
                ((progress * scene.segments.len() as f64) as usize + 1).min(scene.segments.len());
            let mut segments = scene.segments[..shown].to_vec();
            if let Some(newest) = segments.last_mut() {
                sprout(newest);
            }
            canvas.put(scene.bar_row, 1, &join_segments(&segments).runs);
            canvas.restyle(landing.0, landing.1, WORD.len(), Role::Wordmark);
        }
        Stage::Lines => {
            let step = (progress * 3.0) as usize + 1;
            let others = scene.history_rows.saturating_sub(1);
            let shown = 1 + (step * others).div_ceil(3);
            for row in
                (0..scene.rest.rows()).filter(|row| *row >= scene.history_rows || *row < shown)
            {
                canvas.copy_row(&scene.rest, row);
            }
            canvas.restyle(landing.0, landing.1, WORD.len(), Role::Wordmark);
        }
    }
    canvas
}

fn stage_at(elapsed: Duration) -> Option<(Stage, f64)> {
    let mut remaining = elapsed.as_secs_f64() * 1_000.0;
    for (stage, ms) in TIMELINE {
        let ms = hold_ms(ms);
        if remaining < ms {
            return Some((stage, remaining / ms));
        }
        remaining -= ms;
    }
    None
}

/// Cubic ease-out.
fn ease(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

fn lerp(from: usize, to: usize, t: f64) -> usize {
    (from as f64 + (to as f64 - from as f64) * t)
        .round()
        .max(0.0) as usize
}

/// Letters leave later the farther they sit from the middle "g". Odd letters
/// arc up about a row and a half mid-flight. In flight a letter is a spark,
/// in its last 30% the real letter, and on arrival the wordmark style.
fn burst(canvas: &mut Canvas, (row, centre): (usize, usize), step: usize) {
    let delays: Vec<f64> = (0..WORD.len())
        .map(|i| i.abs_diff(MID) as f64 * 0.06 + JITTER[i])
        .collect();
    let longest = delays.iter().copied().fold(0.0, f64::max);
    let start = centre - MID;
    let progress = step as f64 / BURST_STEPS as f64;
    for (i, letter) in WORD.chars().enumerate() {
        let t = ((progress - delays[i]) / (1.0 - longest)).clamp(0.0, 1.0);
        if t >= 1.0 {
            canvas.put_str(row, start + i, &letter.to_string(), Role::Wordmark);
            continue;
        }
        let col = lerp(centre, start + i, ease(t));
        let lift = if i % 2 == 1 {
            (1.5 * (1.0 - (2.0 * t - 1.0).abs())).round() as usize
        } else {
            0
        };
        if t > 0.7 {
            canvas.put_str(row - lift, col, &letter.to_string(), Role::Flash);
        } else {
            let spark = SPARKS[(step + i) % SPARKS.len()];
            canvas.put_str(row - lift, col, spark, Role::Spark);
        }
    }
}

/// The branch segment shows a seedling on the frame it first appears.
fn sprout(segment: &mut Styled) {
    if let Some((_, text)) = segment.runs.first_mut()
        && let Some(rest) = text.strip_prefix('🌿')
    {
        *text = format!("🌱{rest}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::styled::Palette;

    const WELCOME: [&str; 4] = [
        "Talk to Microgentic. Ask for work when you're ready.",
        "When requested, Microgentic can edit files and run project commands.",
        "Configure defaults with microgentic setup outside this view.",
        "Type /help for commands.",
    ];

    /// A resting screen laid out like the real one, 90 by 24.
    fn scene() -> Scene {
        let (rows, cols) = (24, 90);
        let mut rest = Canvas::new(rows, cols);
        for (row, line) in WELCOME.iter().enumerate() {
            rest.put_str(row, 1, line, Role::Muted);
        }
        rest.put_str(19, 1, &"─".repeat(88), Role::Muted);
        rest.put_str(20, 1, "● Ready", Role::Plain);
        rest.put_str(21, 1, "> ", Role::Prompt);
        let segments = vec![
            Styled::default().with(Role::Model, "🤖 m"),
            Styled::default().with(Role::Effort, "🧠 low"),
            Styled::default().with(Role::Branch, "🌿 main"),
        ];
        rest.put(22, 1, &join_segments(&segments).runs);
        rest.put_str(23, 1, "Enter send | Esc quit", Role::Muted);
        Scene {
            rest,
            history_rows: 19,
            chrome_rows: 19..22,
            bar_row: 22,
            hint_row: 23,
            segments,
        }
    }

    fn at(time: Duration) -> Canvas {
        frame(&scene(), time)
    }

    /// A point `fraction` of the way through the stage at `index`.
    fn during(index: usize, fraction: f64) -> Duration {
        start_of(index) + (start_of(index + 1) - start_of(index)).mul_f64(fraction)
    }

    fn all_text(canvas: &Canvas) -> String {
        (0..canvas.rows())
            .map(|row| canvas.row_text(row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_whole_animation_takes_about_1675_ms() {
        let total = duration().as_millis();
        assert!((1625..=1725).contains(&total), "{total} ms");
        assert_eq!(start_of(TIMELINE.len()), duration());
    }

    #[test]
    fn it_starts_with_a_dim_seed_at_the_centre() {
        let canvas = at(Duration::ZERO);
        assert_eq!(canvas.row_text(11).trim(), "·");
        assert_eq!(canvas.role_at(11, 45), Some(Role::Muted));
        assert_eq!(all_text(&canvas).trim(), "·");
        assert_eq!(
            at(during(3, 0.5)).role_at(11, 45),
            Some(Role::Wordmark),
            "the seed ends as ✦"
        );
    }

    #[test]
    fn mid_burst_shows_sparks_and_some_landed_letters() {
        // Step 8 of 10: the middle letters have landed, the outer ones fly.
        let canvas = at(during(4, 0.75));
        let text = all_text(&canvas);
        assert!(
            text.chars()
                .any(|ch| SPARKS.contains(&ch.to_string().as_str())),
            "{text}"
        );
        let landed = (40..51)
            .filter(|col| canvas.role_at(11, *col) == Some(Role::Wordmark))
            .count();
        assert!((1..11).contains(&landed), "{landed} letters landed\n{text}");
        // The middle "g" has landed in place.
        assert_eq!(canvas.row_text(11).chars().nth(45), Some('g'));
    }

    #[test]
    fn the_word_assembles_then_glides_into_the_welcome_line() {
        assert_eq!(at(during(4, 0.99)).row_text(11).trim(), "microgentic");
        let glints = at(start_of(5));
        assert!(all_text(&glints).contains('✧'));
        let landed = at(during(7, 0.99));
        assert_eq!(landed.row_text(0), "         Microgentic");
        assert_eq!(landed.role_at(0, 9), Some(Role::Wordmark));
    }

    #[test]
    fn the_status_bar_sprouts_segment_by_segment() {
        let first = at(start_of(9));
        assert_eq!(first.row_text(22), " 🤖 m");
        assert!(first.row_text(0).starts_with(" Talk to Microgentic."));
        assert_eq!(first.role_at(0, 9), Some(Role::Wordmark));
        assert_eq!(first.row_text(1), "", "later welcome lines wait");
        let branch = at(during(9, 0.7));
        assert_eq!(branch.row_text(22), " 🤖 m  🧠 low  🌱 main");
        let lines = at(duration() - Duration::from_millis(1));
        assert!(lines.row_text(22).ends_with("🌿 main"));
        assert_eq!(lines.row_text(3), " Type /help for commands.");
    }

    #[test]
    fn at_and_after_the_end_it_is_the_resting_screen() {
        let scene = scene();
        assert_eq!(frame(&scene, duration()), scene.rest);
        assert_eq!(frame(&scene, Duration::from_secs(5)), scene.rest);
        assert_ne!(
            frame(&scene, duration() - Duration::from_millis(1)),
            scene.rest
        );
    }

    #[test]
    fn it_plays_only_where_it_makes_sense() {
        let term = Some(OsStr::new("xterm-256color"));
        assert!(should_play(None, term, 90, 24));
        assert!(should_play(Some(OsStr::new("")), term, 90, 24));
        assert!(!should_play(Some(OsStr::new("1")), term, 90, 24));
        assert!(!should_play(None, Some(OsStr::new("dumb")), 90, 24));
        assert!(!should_play(None, term, 39, 24));
        assert!(!should_play(None, term, 90, 11));
    }

    #[test]
    fn without_colour_every_frame_uses_glyphs_and_attributes_only() {
        let scene = scene();
        let mono = Palette { colour: false };
        for ms in (0..duration().as_millis() as u64 + 20).step_by(7) {
            let bytes = frame(&scene, Duration::from_millis(ms))
                .paint(mono)
                .unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("38;5;"), "colour at {ms} ms");
        }
        let colour = frame(&scene, during(4, 0.5))
            .paint(Palette { colour: true })
            .unwrap();
        assert!(String::from_utf8(colour).unwrap().contains("38;5;"));
    }
}
