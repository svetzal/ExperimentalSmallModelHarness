//! The status bar: model, effort, project folder, git state, and context fill.
//!
//! Everything here is pure except `git_status`, a thin shell around one
//! `git status` call. Segments drop from the least important when the
//! terminal is narrow, so the bar never wraps.
use super::styled::{Role, Styled};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use unicode_width::UnicodeWidthChar;

const GAUGE_CELLS: usize = 10;
/// Partial cells in eighths, from one eighth to seven.
const EIGHTHS: [&str; 7] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"];
const SEPARATOR: &str = "  ";
const GIT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct GitStatus {
    /// The branch name, or the short commit when HEAD is detached.
    pub(super) branch: String,
    pub(super) ahead: u64,
    pub(super) behind: u64,
    /// Tracked entries with changes, including conflicts.
    pub(super) changed: usize,
    pub(super) untracked: usize,
}

/// Parse `git status --porcelain=v2 --branch`. `None` when the output has no
/// branch header, which means it did not come from that command.
pub(super) fn parse_git_status(porcelain: &str) -> Option<GitStatus> {
    let mut status = GitStatus::default();
    let mut oid = "";
    let mut head = None;
    for line in porcelain.lines() {
        if let Some(rest) = line.strip_prefix("# branch.oid ") {
            oid = rest;
        } else if let Some(rest) = line.strip_prefix("# branch.head ") {
            head = Some(rest);
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            for part in rest.split_whitespace() {
                if let Some(ahead) = part.strip_prefix('+') {
                    status.ahead = ahead.parse().unwrap_or(0);
                } else if let Some(behind) = part.strip_prefix('-') {
                    status.behind = behind.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") || line.starts_with("u ") {
            status.changed += 1;
        } else if line.starts_with("? ") {
            status.untracked += 1;
        }
    }
    status.branch = match head? {
        "(detached)" => oid.chars().take(7).collect(),
        name => name.to_owned(),
    };
    Some(status)
}

/// Ask git about the project. `None` outside a repository, without git, or
/// when git does not answer in time; the bar then omits the segment.
pub(super) async fn git_status(root: &Path) -> Option<GitStatus> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain=v2", "--branch"])
        .current_dir(root)
        // A status bar must never take the index lock from the user's git.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(GIT_TIMEOUT, output).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    parse_git_status(&String::from_utf8_lossy(&output.stdout))
}

/// Context use as the runtime estimated it for the latest model request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ContextUsage {
    pub(super) used_tokens: usize,
    /// `None` when neither the model table nor the provider gives a size.
    pub(super) window_tokens: Option<usize>,
}

/// `ctx:[███▌░░░░░░]35%`, coloured by fill. Without a known window, only the
/// token count: `ctx: 12.4k`. The bar shape and number carry the meaning
/// without colour.
pub(super) fn context_gauge(usage: ContextUsage) -> Styled {
    let Some(window) = usage.window_tokens.filter(|window| *window > 0) else {
        return Styled::default().with(
            Role::Muted,
            &format!("ctx: {}", compact_tokens(usage.used_tokens)),
        );
    };
    let percent = (usage.used_tokens as f64 * 100.0 / window as f64).round() as usize;
    let eighths = (usage.used_tokens.saturating_mul(GAUGE_CELLS * 8) / window).min(GAUGE_CELLS * 8);
    let (full, part) = (eighths / 8, eighths % 8);
    let mut bar = "█".repeat(full);
    if part > 0 {
        bar.push_str(EIGHTHS[part - 1]);
    }
    let empty = GAUGE_CELLS - full - usize::from(part > 0);
    bar.push_str(&"░".repeat(empty));
    let role = match percent {
        0..60 => Role::Success,
        60..=85 => Role::Working,
        _ => Role::Failure,
    };
    Styled::default()
        .with(Role::Muted, "ctx:[")
        .with(role, &bar)
        .with(Role::Muted, "]")
        .with(role, &format!("{percent}%"))
}

fn compact_tokens(tokens: usize) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.0),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

fn git_segment(git: &GitStatus) -> Styled {
    let mut segment = Styled::default().with(Role::Branch, &format!("🌿 {}", git.branch));
    if git.changed > 0 {
        segment.push(Role::Dirty, &format!(" ●{}", git.changed));
    }
    if git.untracked > 0 {
        segment.push(Role::Muted, &format!(" …{}", git.untracked));
    }
    if git.ahead > 0 {
        segment.push(Role::Muted, &format!(" ↑{}", git.ahead));
    }
    if git.behind > 0 {
        segment.push(Role::Muted, &format!(" ↓{}", git.behind));
    }
    segment
}

pub(super) struct BarState<'a> {
    pub(super) model: &'a str,
    pub(super) effort: Option<&'a str>,
    pub(super) folder: &'a str,
    pub(super) git: Option<&'a GitStatus>,
    pub(super) context: Option<ContextUsage>,
}

/// The segments that fit in `width` cells, in order. When they do not all
/// fit, git goes first, then the folder, then effort. Model and context stay
/// longest.
pub(super) fn status_segments(state: &BarState, width: usize) -> Vec<Styled> {
    // (keep priority, segment): higher priority survives longer.
    let mut segments: Vec<(u8, Styled)> = vec![
        (
            5,
            Styled::default().with(Role::Model, &format!("🤖 {}", state.model)),
        ),
        (
            3,
            Styled::default().with(
                Role::Effort,
                &format!("🧠 {}", state.effort.unwrap_or("default")),
            ),
        ),
        (
            2,
            Styled::default().with(Role::Folder, &format!("📁 {}", state.folder)),
        ),
    ];
    if let Some(git) = state.git {
        segments.push((1, git_segment(git)));
    }
    if let Some(context) = state.context {
        segments.push((4, context_gauge(context)));
    }
    let total = |segments: &[(u8, Styled)]| {
        segments
            .iter()
            .map(|(_, segment)| cells(segment))
            .sum::<usize>()
            + cells_of(SEPARATOR) * segments.len().saturating_sub(1)
    };
    while segments.len() > 1 && total(&segments) > width {
        let lowest = segments
            .iter()
            .enumerate()
            .min_by_key(|(_, (priority, _))| *priority)
            .map(|(index, _)| index)
            .expect("segments are not empty");
        segments.remove(lowest);
    }
    segments.into_iter().map(|(_, segment)| segment).collect()
}

/// Segments side by side, separated as the bar separates them.
pub(super) fn join_segments(segments: &[Styled]) -> Styled {
    let mut bar = Styled::default();
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            bar.push(Role::Plain, SEPARATOR);
        }
        for (role, text) in &segment.runs {
            bar.push(*role, text);
        }
    }
    bar
}

fn cells_of(text: &str) -> usize {
    text.chars().map(|ch| ch.width().unwrap_or(0)).sum()
}

pub(super) fn cells(text: &Styled) -> usize {
    text.runs.iter().map(|(_, run)| cells_of(run)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_bar(state: &BarState, width: usize) -> Styled {
        join_segments(&status_segments(state, width))
    }

    fn text(styled: &Styled) -> String {
        styled.runs.iter().map(|(_, text)| text.as_str()).collect()
    }

    fn gauge(percent: usize) -> Styled {
        context_gauge(ContextUsage {
            used_tokens: percent * 1_000,
            window_tokens: Some(100_000),
        })
    }

    #[test]
    fn porcelain_for_a_clean_branch_with_an_upstream() {
        let status = parse_git_status(
            "# branch.oid 57d988f7ebda760c\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n",
        )
        .unwrap();
        assert_eq!(
            status,
            GitStatus {
                branch: "main".into(),
                ..GitStatus::default()
            }
        );
    }

    #[test]
    fn porcelain_counts_changes_untracked_files_and_divergence() {
        let status = parse_git_status(concat!(
            "# branch.oid 57d988f\n# branch.head feature\n# branch.upstream origin/feature\n# branch.ab +2 -3\n",
            "1 .M N... 100644 100644 100644 aaa aaa src/lib.rs\n",
            "2 R. N... 100644 100644 100644 aaa aaa R100 new.rs\told.rs\n",
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc both.rs\n",
            "? notes.txt\n? scratch/\n",
        ))
        .unwrap();
        assert_eq!(
            status,
            GitStatus {
                branch: "feature".into(),
                ahead: 2,
                behind: 3,
                changed: 3,
                untracked: 2
            }
        );
        assert_eq!(text(&git_segment(&status)), "🌿 feature ●3 …2 ↑2 ↓3");
    }

    #[test]
    fn porcelain_for_detached_head_and_no_upstream() {
        let detached =
            parse_git_status("# branch.oid 57d988f7ebda760c\n# branch.head (detached)\n").unwrap();
        assert_eq!(detached.branch, "57d988f");
        let local =
            parse_git_status("# branch.oid (initial)\n# branch.head main\n? a.txt\n").unwrap();
        assert_eq!((local.ahead, local.behind, local.untracked), (0, 0, 1));
        assert_eq!(parse_git_status("fatal: not a git repository"), None);
    }

    #[test]
    fn the_gauge_fills_in_eighths_and_changes_colour_by_fill() {
        let expected = [
            (0, "ctx:[░░░░░░░░░░]0%", Role::Success),
            (31, "ctx:[███░░░░░░░]31%", Role::Success),
            (59, "ctx:[█████▉░░░░]59%", Role::Success),
            (60, "ctx:[██████░░░░]60%", Role::Working),
            (85, "ctx:[████████▌░]85%", Role::Working),
            (100, "ctx:[██████████]100%", Role::Failure),
        ];
        for (percent, shown, role) in expected {
            let gauge = gauge(percent);
            assert_eq!(text(&gauge), shown);
            assert_eq!(gauge.runs[1].0, role, "{percent}%");
            assert_eq!(cells(&gauge), cells_of(shown));
        }
        // An estimate past the window fills the bar and reports the real number.
        assert_eq!(text(&gauge(130)), "ctx:[██████████]130%");
    }

    #[test]
    fn an_unknown_window_shows_only_the_token_count() {
        let unknown = |used| {
            text(&context_gauge(ContextUsage {
                used_tokens: used,
                window_tokens: None,
            }))
        };
        assert_eq!(unknown(850), "ctx: 850");
        assert_eq!(unknown(12_400), "ctx: 12.4k");
        assert_eq!(unknown(2_500_000), "ctx: 2.5M");
        assert_eq!(
            text(&context_gauge(ContextUsage {
                used_tokens: 5,
                window_tokens: Some(0)
            })),
            "ctx: 5"
        );
    }

    fn state<'a>(git: Option<&'a GitStatus>) -> BarState<'a> {
        BarState {
            model: "qwen3.8:27b",
            effort: Some("high"),
            folder: "Operations",
            git,
            context: Some(ContextUsage {
                used_tokens: 9_000,
                window_tokens: Some(100_000),
            }),
        }
    }

    #[test]
    fn the_full_bar_reads_left_to_right() {
        let git = GitStatus {
            branch: "main".into(),
            changed: 3,
            untracked: 2,
            ..GitStatus::default()
        };
        let bar = status_bar(&state(Some(&git)), 200);
        assert_eq!(
            text(&bar),
            "🤖 qwen3.8:27b  🧠 high  📁 Operations  🌿 main ●3 …2  ctx:[▉░░░░░░░░░]9%"
        );
        // Emoji count as two cells each.
        assert_eq!(cells(&bar), text(&bar).chars().count() + 4);
    }

    #[test]
    fn narrow_bars_drop_git_then_folder_then_effort() {
        let git = GitStatus {
            branch: "main".into(),
            changed: 3,
            ..GitStatus::default()
        };
        let full = cells(&status_bar(&state(Some(&git)), 200));
        let widths_and_shown = [
            (
                full,
                "🤖 qwen3.8:27b  🧠 high  📁 Operations  🌿 main ●3  ctx:[▉░░░░░░░░░]9%",
            ),
            (
                full - 1,
                "🤖 qwen3.8:27b  🧠 high  📁 Operations  ctx:[▉░░░░░░░░░]9%",
            ),
            (50, "🤖 qwen3.8:27b  🧠 high  ctx:[▉░░░░░░░░░]9%"),
            (34, "🤖 qwen3.8:27b  ctx:[▉░░░░░░░░░]9%"),
            (10, "🤖 qwen3.8:27b"),
        ];
        for (width, shown) in widths_and_shown {
            let bar = status_bar(&state(Some(&git)), width);
            assert_eq!(text(&bar), shown, "width {width}");
            assert!(cells(&bar) <= width || bar.runs.len() == 1, "width {width}");
        }
    }

    #[test]
    fn effort_defaults_and_git_or_context_may_be_absent() {
        let bar = status_bar(
            &BarState {
                effort: None,
                context: None,
                ..state(None)
            },
            200,
        );
        assert_eq!(text(&bar), "🤖 qwen3.8:27b  🧠 default  📁 Operations");
    }
}
