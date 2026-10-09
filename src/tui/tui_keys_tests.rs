use super::*;
use crossterm::event::KeyModifiers as M;

fn key(c: char) -> Option<TuiCommand> {
    invoice_table_key_command(KeyCode::Char(c), M::NONE, false)
}

#[test]
fn documented_shortcuts_map_to_the_menu_commands() {
    assert_eq!(key('j'), Some(TuiCommand::MoveDown));
    assert_eq!(key('k'), Some(TuiCommand::MoveUp));
    assert_eq!(key('u'), main_menu_command(MI_UPLOAD));
    assert_eq!(key('a'), main_menu_command(MI_APPROVE));
    assert_eq!(key('r'), main_menu_command(MI_REJECT));
    assert_eq!(key('n'), main_menu_command(MI_CLEAR));
    assert_eq!(key('c'), main_menu_command(MI_COMMIT));
    assert_eq!(key('e'), main_menu_command(MI_EDIT));
    assert_eq!(key('q'), Some(TuiCommand::Quit));
    assert_eq!(key('u'), Some(TuiCommand::MarkUpload));
    assert_eq!(key('a'), Some(TuiCommand::MarkApprove));
    assert_eq!(key('r'), Some(TuiCommand::MarkReject));
    assert_eq!(key('n'), Some(TuiCommand::ClearMarks));
    assert_eq!(key('c'), Some(TuiCommand::Commit));
    assert_eq!(key('e'), Some(TuiCommand::Edit));
}

#[test]
fn arrows_and_escape() {
    let cmd = |code, menu_open| invoice_table_key_command(code, M::NONE, menu_open);
    assert_eq!(cmd(KeyCode::Down, false), Some(TuiCommand::MoveDown));
    assert_eq!(cmd(KeyCode::Up, false), Some(TuiCommand::MoveUp));
    assert_eq!(cmd(KeyCode::Esc, false), Some(TuiCommand::Quit));
    assert_eq!(cmd(KeyCode::Esc, true), Some(TuiCommand::CloseMenu));
    assert_eq!(
        invoice_table_key_command(KeyCode::Down, M::SHIFT, false),
        Some(TuiCommand::MoveDown)
    );
}

#[test]
fn action_keys_are_inactive_while_the_menu_is_open() {
    for c in ['u', 'a', 'r', 'n', 'c', 'e'] {
        assert_eq!(
            invoice_table_key_command(KeyCode::Char(c), M::NONE, true),
            None,
            "{c}"
        );
    }
    assert_eq!(
        invoice_table_key_command(KeyCode::Char('c'), M::SUPER, true),
        None
    );
    // Movement and quitting still work.
    assert_eq!(
        invoice_table_key_command(KeyCode::Char('j'), M::NONE, true),
        Some(TuiCommand::MoveDown)
    );
    assert_eq!(
        invoice_table_key_command(KeyCode::Char('q'), M::NONE, true),
        Some(TuiCommand::Quit)
    );
}

#[test]
fn ctrl_c_quits_and_never_executes() {
    assert_eq!(
        invoice_table_key_command(KeyCode::Char('c'), M::CONTROL, false),
        Some(TuiCommand::Quit)
    );
    for modifiers in [M::SUPER, M::META] {
        assert_eq!(
            invoice_table_key_command(KeyCode::Char('c'), modifiers, false),
            Some(TuiCommand::Commit)
        );
        assert_eq!(
            invoice_table_key_command(KeyCode::Enter, modifiers, false),
            Some(TuiCommand::Commit)
        );
    }
    for c in ['u', 'a', 'r', 'n', 'e', 'j', 'k', 'q'] {
        assert_eq!(
            invoice_table_key_command(KeyCode::Char(c), M::CONTROL, false),
            None,
            "ctrl+{c}"
        );
        assert_eq!(
            invoice_table_key_command(KeyCode::Char(c), M::ALT, false),
            None,
            "alt+{c}"
        );
    }
}

#[test]
fn unrelated_keys_are_left_to_the_view() {
    for c in ['s', 'S', 'f', 'v', ' ', 'U', 'x'] {
        assert_eq!(key(c), None, "{c}");
    }
    assert_eq!(
        invoice_table_key_command(KeyCode::Enter, M::NONE, false),
        None
    );
}

#[test]
fn every_main_menu_entry_has_a_command() {
    let commands = (0..MAIN_COUNT)
        .map(|idx| main_menu_command(idx).expect("command"))
        .collect::<Vec<_>>();
    assert_eq!(commands.len(), MAIN_COUNT);
    assert_eq!(main_menu_command(MAIN_COUNT), None);
    assert_eq!(main_menu_command(MI_MENU), Some(TuiCommand::OpenMenu));
    assert_eq!(main_menu_command(MI_SYNC), Some(TuiCommand::Sync));
}

#[test]
fn work_commands_are_refused_while_an_operation_runs() {
    for command in [
        TuiCommand::Sync,
        TuiCommand::Reconcile,
        TuiCommand::Llm,
        TuiCommand::MarkUpload,
        TuiCommand::MarkApprove,
        TuiCommand::MarkReject,
        TuiCommand::ClearMarks,
        TuiCommand::Commit,
        TuiCommand::Edit,
        TuiCommand::OpenMenu,
    ] {
        assert!(command.requires_idle(), "{command:?}");
    }
    for command in [
        TuiCommand::Quit,
        TuiCommand::CloseMenu,
        TuiCommand::MoveDown,
        TuiCommand::MoveUp,
    ] {
        assert!(!command.requires_idle(), "{command:?}");
    }
}

#[test]
fn quit_during_operation_needs_a_second_press() {
    assert_eq!(invoice_table_quit_request(false, false), QuitRequest::Exit);
    assert_eq!(invoice_table_quit_request(true, false), QuitRequest::Warn);
    assert_eq!(invoice_table_quit_request(true, true), QuitRequest::Exit);
}

#[test]
fn help_line_lists_the_real_shortcuts() {
    for actionable_only in [false, true] {
        let help = invoice_table_help_text(actionable_only);
        for needle in [
            "j/k",
            "u=upload",
            "a=zatwierdź",
            "r=odrzuć",
            "n=wyczyść",
            "c=wykonaj",
            "e=popraw",
            "q=wyjdź",
        ] {
            assert!(help.contains(needle), "{needle} missing in {help}");
        }
        assert!(!help.contains('⌘'));
    }
}

#[test]
fn review_threshold_is_validated() {
    assert_eq!(parse_review_threshold("70"), Ok(70));
    assert_eq!(parse_review_threshold("50"), Ok(50));
    assert_eq!(parse_review_threshold("100"), Ok(100));
    assert_eq!(parse_review_threshold(" 85 "), Ok(85));
    for bad in ["0", "49", "101", "255", "999", "", "abc"] {
        let err = parse_review_threshold(bad).unwrap_err();
        assert!(err.contains("50–100"), "{bad}: {err}");
    }
}
