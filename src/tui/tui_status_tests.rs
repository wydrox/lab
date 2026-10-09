use super::*;

const POLISH_MESSAGES: [&str; 5] = [
    "✗ Błąd Akceptuj (upload 1, zatw. 0, odrz. 0): upload Saldeo nie powiódł się (1/1 błędów): faktura-źródłowa.pdf: żądanie odrzucone",
    "✓ Akceptuj (upload 0, zatw. 2, odrz. 0) zakończone: udane 2, nieudane 0; 120 faktur, 2 nowych/zmienionych",
    "Zatwierdź: brak dokumentów Saldeo/KSeF do oznaczenia (wybierz wiersz „nieozn.”)",
    "Tabela: status KSeF w Saldeo niedostępny — użyj Menu → Saldeo, aby odświeżyć sesję",
    "ąęćłńóśźż ĄĘĆŁŃÓŚŹŻ zażółć gęślą jaźń",
];

fn display_width(text: &str) -> usize {
    ratatui::text::Line::from(text).width()
}

#[test]
fn status_line_never_splits_characters_and_fits_every_width() {
    for message in POLISH_MESSAGES {
        for prefix in ["", "◐"] {
            for width in 1..=120 {
                let text = status_bar_text(message, prefix, width);
                assert!(
                    display_width(&text) <= width,
                    "width {width}: {text:?} is {} columns",
                    display_width(&text)
                );
                assert!(text.chars().count() <= width, "width {width}: {text:?}");
                let full = status_bar_text(message, prefix, usize::MAX);
                if display_width(&full) <= width {
                    assert_eq!(text, full);
                } else {
                    assert!(text.ends_with('…'), "width {width}: {text:?}");
                    let kept = text.trim_end_matches('…');
                    assert!(full.starts_with(kept), "width {width}: {text:?}");
                }
            }
        }
    }
}

#[test]
fn audit_example_at_80_columns() {
    let message = "✗ Błąd Akceptuj (upload 1, zatw. 0, odrz. 0): upload Saldeo nie powiódł się (1/1 błędów): plik.pdf: HTTP 500";
    let text = status_bar_text(message, "", 80);
    assert_eq!(text.chars().count(), 80);
    assert!(text.starts_with(" ✗ Błąd Akceptuj"));
}

#[test]
fn status_line_flattens_multiline_errors() {
    let text = status_bar_text("✗ błąd:\nlinia druga\tdalej", "", 120);
    assert!(!text.chars().any(char::is_control), "{text:?}");
}

#[test]
fn zero_width_is_empty() {
    assert_eq!(status_bar_text("łódź", "", 0), "");
}

#[test]
fn notice_is_visible_while_progress_is_shown() {
    assert_eq!(status_bar_message(None, None, "gotowe"), "gotowe");
    assert_eq!(
        status_bar_message(Some("Upload Saldeo 1/2"), None, "stare"),
        "Upload Saldeo 1/2"
    );
    assert_eq!(status_bar_message(Some(""), None, "stare"), "stare");
    let text = status_bar_message(
        Some("Upload Saldeo 1/2"),
        Some(QUIT_WHILE_RUNNING_WARNING),
        "stare",
    );
    assert!(text.starts_with("⚠ Trwa operacja"), "{text}");
    assert!(text.ends_with("Upload Saldeo 1/2"), "{text}");
}
