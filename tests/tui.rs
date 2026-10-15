//! End-to-end tests of the terminal interface, on a real pseudo-terminal.
#![cfg(unix)]

mod support;

use support::*;

/// The resting screen with no provider: animation off, an unreachable host.
fn open_idle(no_color: Option<&str>) -> (Workspace, Tui) {
    let workspace = Workspace::new();
    let mut launch = Launch::new(microgentic())
        .arg("--config")
        .arg(&workspace.config)
        .cwd(&workspace.project)
        .size(24, 90)
        .env("OMLX_HOST", "http://127.0.0.1:1")
        .env("MICROGENTIC_NO_ANIMATION", "1");
    if let Some(value) = no_color {
        launch = launch.env("NO_COLOR", value);
    }
    let tui = launch.spawn();
    (workspace, tui)
}

/// A crossterm foreground colour code in raw output; attributes are not colour.
fn has_colour_codes(raw: &[u8]) -> bool {
    String::from_utf8_lossy(raw).contains("\x1b[38;5;")
}

#[test]
fn tui_opens_without_a_provider_and_restores_the_terminal() {
    let (_workspace, mut tui) = open_idle(None);
    let screen = tui.wait_text("Enter send");
    assert!(screen.contains("Talk to Microgentic."), "{screen}");
    let (row, col) = screen.locate("● Ready").expect("the ready status");
    let marker = screen.cell(row, col);
    assert_eq!(marker.fgcolor(), ansi(2), "the ready marker is green");
    assert!(marker.bold());
    assert!(
        has_colour_codes(&tui.raw()),
        "the default view has no colour"
    );
    tui.assert_raw_mode_active();
    // A small resize and Unicode input exercise layout without submitting work.
    tui.resize(5, 20);
    tui.paste("世界");
    std::thread::sleep(std::time::Duration::from_millis(100));
    tui.quit();
}

#[test]
fn an_empty_no_color_keeps_colour() {
    let (_workspace, mut tui) = open_idle(Some(""));
    tui.wait_text("Enter send");
    assert!(
        has_colour_codes(&tui.raw()),
        "an empty NO_COLOR disabled colour"
    );
    tui.quit();
}

#[test]
fn no_color_removes_colour_but_keeps_labels_and_attributes() {
    let (_workspace, mut tui) = open_idle(Some("1"));
    let screen = tui.wait_text("Enter send");
    assert!(
        !has_colour_codes(&tui.raw()),
        "NO_COLOR still produced colour"
    );
    let (row, col) = screen
        .locate("● Ready")
        .expect("the monochrome status keeps its label");
    let marker = screen.cell(row, col);
    assert_eq!(marker.fgcolor(), vt100::Color::Default);
    assert!(marker.bold(), "attributes remain without colour");
    tui.quit();
}
