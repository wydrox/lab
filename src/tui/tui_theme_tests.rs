use super::*;
use std::cell::Cell;

fn inputs(
    lab_theme: Option<&str>,
    colorfgbg: Option<&str>,
    term_program: Option<&str>,
) -> ThemeInputs {
    ThemeInputs {
        lab_theme: lab_theme.map(String::from),
        colorfgbg: colorfgbg.map(String::from),
        term_program: term_program.map(String::from),
        ..ThemeInputs::default()
    }
}

/// Wynik i liczba zapytań o wygląd systemu.
fn decide(inputs: &ThemeInputs, system_dark: Option<bool>) -> (bool, usize) {
    let calls = Cell::new(0);
    let light = theme_is_light(inputs, || {
        calls.set(calls.get() + 1);
        system_dark
    });
    (light, calls.get())
}

#[test]
fn lab_theme_override_wins_over_everything() {
    for term in [None, Some("Apple_Terminal")] {
        assert_eq!(
            decide(&inputs(Some("light"), Some("15;0"), term), Some(true)),
            (true, 0)
        );
        assert_eq!(
            decide(&inputs(Some("DARK"), Some("0;15"), term), Some(false)),
            (false, 0)
        );
    }
    let mut both = inputs(Some("dark"), None, None);
    both.lab_tui_theme = Some("light".into());
    both.term_background = Some("light".into());
    assert_eq!(decide(&both, None), (false, 0));
}

#[test]
fn invalid_lab_theme_falls_back_to_normal_detection() {
    for value in ["", "jasny", "auto", "lightish"] {
        assert_eq!(
            decide(&inputs(Some(value), Some("0;15"), None), None),
            (true, 0),
            "{value:?}"
        );
        assert_eq!(
            decide(&inputs(Some(value), None, None), None),
            (false, 0),
            "{value:?}"
        );
    }
}

#[test]
fn existing_overrides_and_colorfgbg_are_unchanged() {
    let mut tui = inputs(None, Some("0;15"), Some("Apple_Terminal"));
    tui.lab_tui_theme = Some("jasny".into());
    assert_eq!(decide(&tui, Some(true)), (true, 0));
    tui.lab_tui_theme = Some("dark".into());
    assert_eq!(decide(&tui, Some(false)), (false, 0));
    let mut bg = inputs(None, Some("0;15"), None);
    bg.term_background = Some("Light".into());
    assert_eq!(decide(&bg, None), (true, 0));
    bg.term_background = Some("dark".into());
    assert_eq!(decide(&bg, None), (false, 0));
    for (value, light) in [
        ("15;0", false),
        ("0;15", true),
        ("0;7", true),
        ("0;default;15", true),
        ("7;8", false),
    ] {
        assert_eq!(
            decide(
                &inputs(None, Some(value), Some("Apple_Terminal")),
                Some(!light)
            ),
            (light, 0),
            "{value}"
        );
    }
}

#[test]
fn apple_terminal_follows_system_appearance_only_when_undecided() {
    let term = Some("Apple_Terminal");
    assert_eq!(decide(&inputs(None, None, term), Some(false)), (true, 1));
    assert_eq!(decide(&inputs(None, None, term), Some(true)), (false, 1));
    assert_eq!(decide(&inputs(None, None, term), None), (false, 1));
    // Nieczytelny COLORFGBG niczego nie rozstrzyga.
    assert_eq!(
        decide(&inputs(None, Some("x;y"), term), Some(false)),
        (true, 1)
    );
    assert_eq!(
        decide(&inputs(Some("auto"), None, term), Some(false)),
        (true, 1)
    );
}

#[test]
fn other_terminals_default_to_dark_without_asking_the_system() {
    for term in [
        None,
        Some("iTerm.app"),
        Some("vscode"),
        Some("apple_terminal"),
    ] {
        assert_eq!(
            decide(&inputs(None, None, term), Some(false)),
            (false, 0),
            "{term:?}"
        );
    }
}
