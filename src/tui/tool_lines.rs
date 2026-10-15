//! One live line per tool call.
//!
//! Each tool has an executor in `crate::tools` and a presenter here. A
//! presenter is a pure function that turns the call's arguments into a short
//! summary, such as the file and line range a read asked for. Presenters never
//! call tools, and the executors and runtime know nothing about this view.
use super::styled::{Role, Styled};
use serde_json::Value;
use std::time::Duration;
use unicode_width::UnicodeWidthChar;

/// Braille spinner frames, advanced by time while any call is running.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub(super) const SPINNER_FRAME: Duration = Duration::from_millis(90);

pub(super) fn spinner(frame: usize) -> &'static str {
    SPINNER[frame % SPINNER.len()]
}

/// Longest path, in cells, before its start is elided.
const PATH_CELLS: usize = 48;
/// Longest command or free text, in cells, before its end is elided.
const TEXT_CELLS: usize = 64;
/// Longest value shown by the generic presenter.
const VALUE_CELLS: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CallState {
    Running,
    Succeeded,
    Failed,
    /// The run ended before the call reported a result.
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ToolLine {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) summary: String,
    pub(super) state: CallState,
}

impl ToolLine {
    pub(super) fn started(call_id: &str, name: &str, arguments: &Value) -> Self {
        Self {
            call_id: call_id.to_owned(),
            name: name.to_owned(),
            summary: summarize(name, arguments),
            state: CallState::Running,
        }
    }

    /// Whether a finish report belongs to this call. Calls without an ID are
    /// matched by name, oldest first, because tools in a batch run in order.
    pub(super) fn answers(&self, call_id: &str, name: &str) -> bool {
        self.state == CallState::Running
            && if call_id.is_empty() || self.call_id.is_empty() {
                self.name == name
            } else {
                self.call_id == call_id
            }
    }

    /// Append `  <marker> <tool> <summary>` and a newline. The markers are
    /// distinct glyphs, so the state reads without colour.
    pub(super) fn render(&self, frame: usize, out: &mut Styled) {
        let (role, marker) = match self.state {
            CallState::Running => (Role::Working, spinner(frame)),
            CallState::Succeeded => (Role::Success, "✓"),
            CallState::Failed => (Role::Failure, "✗"),
            CallState::Stopped => (Role::Muted, "–"),
        };
        out.push(Role::Plain, "  ");
        out.push(role, marker);
        out.push(Role::Plain, " ");
        let name_role = if self.state == CallState::Stopped {
            Role::Muted
        } else {
            Role::Plain
        };
        out.push(name_role, &self.name);
        if !self.summary.is_empty() {
            out.push(Role::Muted, &format!(" {}", self.summary));
        }
        if self.state == CallState::Stopped {
            out.push(Role::Muted, " · stopped");
        }
        out.push(Role::Plain, "\n");
    }
}

/// A presenter renders one tool's arguments as a one-line summary.
type Presenter = fn(&Value) -> String;

fn presenter(name: &str) -> Presenter {
    match name {
        "read_file" => read_file,
        "list_tree" => list_tree,
        "write_file" => write_file,
        "edit_file" => edit_file,
        "replace_file_lines" => replace_file_lines,
        "insert_file_lines" => insert_file_lines,
        "apply_change_set" => apply_change_set,
        "apply_patch" => apply_patch,
        "shell_command" => shell_command,
        "execute_probe" => execute_probe,
        "submit_acceptance_evidence" => submit_acceptance_evidence,
        _ => generic,
    }
}

pub(super) fn summarize(name: &str, arguments: &Value) -> String {
    presenter(name)(arguments)
}

fn read_file(arguments: &Value) -> String {
    let range = match (
        number(arguments, "line_start"),
        number(arguments, "line_end"),
    ) {
        (Some(start), Some(end)) => format!(":{start}-{end}"),
        (Some(start), None) => format!(":{start}-"),
        (None, Some(end)) => format!(":1-{end}"),
        (None, None) => String::new(),
    };
    format!("{}{range}", path(arguments, "path"))
}

fn list_tree(arguments: &Value) -> String {
    let mut summary = match text(arguments, "path") {
        Some(value) if !value.trim().is_empty() => elide_start(value, PATH_CELLS),
        _ => ".".to_owned(),
    };
    if let Some(depth) = number(arguments, "max_depth") {
        summary.push_str(&format!(" depth {depth}"));
    }
    if arguments["include_hidden"] == true {
        summary.push_str(" +hidden");
    }
    summary
}

fn write_file(arguments: &Value) -> String {
    path(arguments, "path")
}

/// `edit_file` has an `edits` list in one schema and a single `operation` in
/// the line-oriented schema.
fn edit_file(arguments: &Value) -> String {
    let mut summary = path(arguments, "path");
    if let Some(edits) = arguments["edits"].as_array() {
        summary.push_str(&format!(" · {}", count(edits.len(), "edit")));
    } else if let Some(operation) = text(arguments, "operation") {
        summary.push_str(&format!(" · {}", one_line(operation, VALUE_CELLS)));
        match (number(arguments, "line"), number(arguments, "end_line")) {
            (Some(line), Some(end)) => summary.push_str(&format!(" lines {line}-{end}")),
            (Some(line), None) => summary.push_str(&format!(" line {line}")),
            _ => {}
        }
    }
    summary
}

fn replace_file_lines(arguments: &Value) -> String {
    match (
        number(arguments, "start_line"),
        number(arguments, "end_line"),
    ) {
        (Some(start), Some(end)) => format!("{}:{start}-{end}", path(arguments, "path")),
        _ => path(arguments, "path"),
    }
}

fn insert_file_lines(arguments: &Value) -> String {
    let mut summary = path(arguments, "path");
    if let Some(anchor) = number(arguments, "anchor_line") {
        let position = text(arguments, "position").unwrap_or("at");
        summary.push_str(&format!(" · {} line {anchor}", one_line(position, 8)));
    }
    summary
}

fn apply_change_set(arguments: &Value) -> String {
    let changes = arguments["changes"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let paths: Vec<&str> = changes
        .iter()
        .filter_map(|change| change["path"].as_str())
        .collect();
    files_summary(&paths, changes.len(), "change")
}

/// Unified diffs name their targets on `+++` lines, or on `---` for deletions.
fn apply_patch(arguments: &Value) -> String {
    let patch = text(arguments, "patch").unwrap_or_default();
    let mut paths = Vec::new();
    let mut removed = None;
    for line in patch.lines() {
        if let Some(old) = line.strip_prefix("--- ") {
            removed = Some(old);
        } else if let Some(new) = line.strip_prefix("+++ ") {
            let target = if new.trim() == "/dev/null" {
                removed.unwrap_or(new)
            } else {
                new
            };
            paths.push(strip_diff_prefix(target));
        }
    }
    files_summary(&paths, 0, "change")
}

fn shell_command(arguments: &Value) -> String {
    let mut summary = one_line(text(arguments, "command").unwrap_or_default(), TEXT_CELLS);
    if let Some(cwd) = text(arguments, "cwd").filter(|cwd| !cwd.trim().is_empty() && *cwd != ".") {
        summary.push_str(&format!(" (in {})", elide_start(cwd, PATH_CELLS / 2)));
    }
    summary
}

fn execute_probe(arguments: &Value) -> String {
    one_line(text(arguments, "probe_id").unwrap_or_default(), TEXT_CELLS)
}

fn submit_acceptance_evidence(arguments: &Value) -> String {
    let ids: Vec<&str> = arguments["acceptance_ids"]
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    one_line(&ids.join(", "), TEXT_CELLS)
}

/// Unknown tools show their first two arguments as compact `key=value` pairs.
fn generic(arguments: &Value) -> String {
    let Some(object) = arguments.as_object() else {
        return String::new();
    };
    object
        .iter()
        .take(2)
        .map(|(key, value)| {
            let value = match value {
                Value::String(text) => one_line(text, VALUE_CELLS),
                other => one_line(&other.to_string(), VALUE_CELLS),
            };
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn text<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments[key].as_str()
}

fn number(arguments: &Value, key: &str) -> Option<u64> {
    arguments[key].as_u64()
}

fn path(arguments: &Value, key: &str) -> String {
    elide_start(text(arguments, key).unwrap_or_default(), PATH_CELLS)
}

fn strip_diff_prefix(path: &str) -> &str {
    let path = path.split('\t').next().unwrap_or(path).trim();
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `first-path +N files`, or `path · N changes` when every change hits one file.
fn files_summary(paths: &[&str], changes: usize, noun: &str) -> String {
    let mut distinct: Vec<&str> = Vec::new();
    for path in paths {
        if !distinct.contains(path) {
            distinct.push(path);
        }
    }
    match distinct.as_slice() {
        [] => String::new(),
        [only] if changes > 1 => format!(
            "{} · {}",
            elide_start(only, PATH_CELLS),
            count(changes, noun)
        ),
        [only] => elide_start(only, PATH_CELLS),
        [first, rest @ ..] => format!(
            "{} +{}",
            elide_start(first, PATH_CELLS),
            count(rest.len(), "file")
        ),
    }
}

fn cells(text: &str) -> usize {
    text.chars().map(|ch| ch.width().unwrap_or(0)).sum()
}

/// Collapse whitespace and control characters so a value fits on one line.
fn flatten(text: &str) -> String {
    text.split(|ch: char| ch.is_whitespace() || ch.is_control())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Keep the end of a path, where the file name is: `…/src/main.rs`.
fn elide_start(text: &str, max_cells: usize) -> String {
    let text = flatten(text);
    if cells(&text) <= max_cells {
        return text;
    }
    let mut kept = Vec::new();
    let mut used = 1;
    for ch in text.chars().rev() {
        let size = ch.width().unwrap_or(0);
        if used + size > max_cells {
            break;
        }
        used += size;
        kept.push(ch);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}

/// Keep the start of free text, such as a command: `cargo test --wor…`.
fn one_line(text: &str, max_cells: usize) -> String {
    let text = flatten(text);
    if cells(&text) <= max_cells {
        return text;
    }
    let mut kept = String::new();
    let mut used = 1;
    for ch in text.chars() {
        let size = ch.width().unwrap_or(0);
        if used + size > max_cells {
            break;
        }
        used += size;
        kept.push(ch);
    }
    kept.push('…');
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_show_the_path_and_line_range() {
        assert_eq!(
            summarize("read_file", &json!({"path": "src/main.rs"})),
            "src/main.rs"
        );
        assert_eq!(
            summarize(
                "read_file",
                &json!({"path": "src/main.rs", "line_start": 1, "line_end": 80})
            ),
            "src/main.rs:1-80"
        );
        assert_eq!(
            summarize("read_file", &json!({"path": "a.rs", "line_start": 5})),
            "a.rs:5-"
        );
        assert_eq!(
            summarize("read_file", &json!({"path": "a.rs", "line_end": 9})),
            "a.rs:1-9"
        );
    }

    #[test]
    fn long_paths_keep_their_file_name() {
        let deep = format!("{}/src/main.rs", "very/deep/directory".repeat(5));
        let summary = summarize(
            "read_file",
            &json!({"path": deep, "line_start": 3, "line_end": 4}),
        );
        assert!(
            summary.starts_with('…') && summary.ends_with("/src/main.rs:3-4"),
            "{summary}"
        );
        assert_eq!(cells(summary.trim_end_matches(":3-4")), PATH_CELLS);
    }

    #[test]
    fn trees_show_the_directory_depth_and_hidden_files() {
        assert_eq!(summarize("list_tree", &json!({})), ".");
        assert_eq!(
            summarize(
                "list_tree",
                &json!({"path": "src", "max_depth": 2, "include_hidden": true})
            ),
            "src depth 2 +hidden"
        );
    }

    #[test]
    fn writes_and_edits_show_the_file_and_the_change() {
        assert_eq!(
            summarize(
                "write_file",
                &json!({"path": "src/lib.rs", "content": "x\ny"})
            ),
            "src/lib.rs"
        );
        assert_eq!(
            summarize("edit_file", &json!({"path": "a.rs", "edits": [{}, {}]})),
            "a.rs · 2 edits"
        );
        assert_eq!(
            summarize("edit_file", &json!({"path": "a.rs", "edits": [{}]})),
            "a.rs · 1 edit"
        );
        assert_eq!(
            summarize(
                "edit_file",
                &json!({"path": "a.rs", "operation": "replace", "line": 4, "end_line": 6})
            ),
            "a.rs · replace lines 4-6"
        );
        assert_eq!(
            summarize(
                "replace_file_lines",
                &json!({"path": "a.rs", "start_line": 2, "end_line": 3, "replacement": "z"})
            ),
            "a.rs:2-3"
        );
        assert_eq!(
            summarize(
                "insert_file_lines",
                &json!({"path": "a.rs", "anchor_line": 7, "position": "after", "content": "z"})
            ),
            "a.rs · after line 7"
        );
    }

    #[test]
    fn change_sets_and_patches_name_their_files() {
        let changes = json!({"changes": [
            {"kind": "replace", "path": "a.rs"},
            {"kind": "replace", "path": "a.rs"},
        ]});
        assert_eq!(summarize("apply_change_set", &changes), "a.rs · 2 changes");
        let changes = json!({"changes": [
            {"kind": "replace", "path": "a.rs"},
            {"kind": "write", "path": "b.rs"},
            {"kind": "write", "path": "c.rs"},
        ]});
        assert_eq!(summarize("apply_change_set", &changes), "a.rs +2 files");
        let patch = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-a\n+b\n--- a/old.rs\n+++ /dev/null\n";
        assert_eq!(
            summarize("apply_patch", &json!({"patch": patch})),
            "src/lib.rs +1 file"
        );
        assert_eq!(summarize("apply_patch", &json!({"patch": "nonsense"})), "");
    }

    #[test]
    fn commands_are_one_line_and_truncated_at_the_end() {
        assert_eq!(
            summarize(
                "shell_command",
                &json!({"command": "cargo check --offline", "timeout_secs": 30})
            ),
            "cargo check --offline"
        );
        assert_eq!(
            summarize(
                "shell_command",
                &json!({"command": "cd x &&\n  make", "cwd": "sub"})
            ),
            "cd x && make (in sub)"
        );
        let long = summarize("shell_command", &json!({"command": "echo ".repeat(40)}));
        assert!(long.ends_with('…') && long.starts_with("echo echo"));
        assert_eq!(cells(&long), TEXT_CELLS);
    }

    #[test]
    fn acceptance_tools_show_their_identifiers() {
        assert_eq!(
            summarize("execute_probe", &json!({"probe_id": "unit-tests"})),
            "unit-tests"
        );
        assert_eq!(
            summarize(
                "submit_acceptance_evidence",
                &json!({"acceptance_ids": ["a1", "a2"], "evidence": "long"})
            ),
            "a1, a2"
        );
    }

    #[test]
    fn unknown_tools_show_two_compact_arguments() {
        assert_eq!(
            summarize(
                "mystery",
                &json!({"alpha": "one\ntwo", "beta": 2, "gamma": true})
            ),
            "alpha=one two beta=2"
        );
        assert_eq!(summarize("mystery", &json!("not an object")), "");
        let long = summarize("mystery", &json!({"text": "x".repeat(100)}));
        assert_eq!(cells(long.trim_start_matches("text=")), VALUE_CELLS);
    }

    #[test]
    fn missing_arguments_never_panic() {
        for name in [
            "read_file",
            "list_tree",
            "write_file",
            "edit_file",
            "replace_file_lines",
            "insert_file_lines",
            "apply_change_set",
            "apply_patch",
            "shell_command",
            "execute_probe",
            "submit_acceptance_evidence",
        ] {
            let _ = summarize(name, &json!({}));
            let _ = summarize(name, &Value::Null);
        }
    }

    fn rendered(line: &ToolLine, frame: usize) -> (String, Vec<Role>) {
        let mut out = Styled::default();
        line.render(frame, &mut out);
        (
            out.runs.iter().map(|(_, text)| text.as_str()).collect(),
            out.runs.iter().map(|(role, _)| *role).collect(),
        )
    }

    #[test]
    fn each_state_has_its_own_glyph() {
        let mut line = ToolLine::started("c1", "read_file", &json!({"path": "a.rs"}));
        assert_eq!(rendered(&line, 0).0, "  ⠋ read_file a.rs\n");
        assert_eq!(rendered(&line, 11).0, "  ⠙ read_file a.rs\n");
        line.state = CallState::Succeeded;
        let (text, roles) = rendered(&line, 0);
        assert_eq!(text, "  ✓ read_file a.rs\n");
        assert!(roles.contains(&Role::Success));
        line.state = CallState::Failed;
        assert_eq!(rendered(&line, 0).0, "  ✗ read_file a.rs\n");
        line.state = CallState::Stopped;
        assert_eq!(rendered(&line, 0).0, "  – read_file a.rs · stopped\n");
    }

    #[test]
    fn finish_reports_match_by_id_or_by_name_without_one() {
        let line = ToolLine::started("c1", "read_file", &json!({}));
        assert!(line.answers("c1", "read_file"));
        assert!(!line.answers("c2", "read_file"));
        let anonymous = ToolLine::started("", "read_file", &json!({}));
        assert!(anonymous.answers("", "read_file"));
        assert!(!anonymous.answers("", "list_tree"));
    }
}
