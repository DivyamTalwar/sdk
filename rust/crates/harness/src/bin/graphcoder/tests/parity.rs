//! Each row is executable scenario data for the composed shared prompt.
use acyclic_harness::tui::{
    Editor, PromptInput, Screen, Viewport,
    key_event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    ratatui::{Terminal, backend::TestBackend},
};

#[test]
fn declarative_prompt_parity_rows_render_through_test_backend() {
    let rows: Vec<serde_json::Value> = serde_json::from_str(include_str!("parity.json")).unwrap();
    for row in rows {
        let name = row["row"].as_str().unwrap();
        let mut input = PromptInput::new(Editor::new(row["text"].as_str().unwrap()));
        let mut effects = Vec::new();
        for (index, event) in row["events"].as_array().unwrap().iter().enumerate() {
            let now = (index as u64 + 1) * 1000;
            if let Some(text) = event["text"].as_str() {
                effects.extend(input.committed_text(text, now));
            } else if let Some(text) = event["paste"].as_str() {
                effects.extend(input.paste(text, now));
            } else {
                let code = match event["key"].as_str().unwrap() {
                    "Enter" => KeyCode::Enter,
                    "Home" => KeyCode::Home,
                    "End" => KeyCode::End,
                    "Left" => KeyCode::Left,
                    "Right" => KeyCode::Right,
                    "Backspace" => KeyCode::Backspace,
                    character => KeyCode::Char(character.chars().next().unwrap()),
                };
                let mut modifiers = KeyModifiers::NONE;
                for (field, modifier) in [
                    ("ctrl", KeyModifiers::CONTROL),
                    ("shift", KeyModifiers::SHIFT),
                    ("alt", KeyModifiers::ALT),
                ] {
                    if event[field].as_bool() == Some(true) {
                        modifiers |= modifier;
                    }
                }
                let mut key = KeyEvent::new(code, modifiers);
                if event["repeat"].as_bool() == Some(true) {
                    key.kind = KeyEventKind::Repeat;
                }
                effects.extend(input.key(key, now, 38));
            }
        }
        assert_eq!(
            input.editor.text(),
            row["expected"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(
            effects.len(),
            row["effects"].as_u64().unwrap_or(0) as usize,
            "{name}"
        );
        for width in [40, 80] {
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            terminal
                .draw(|frame| {
                    Screen::default().render(frame, &input, &mut Viewport::default(), 0, 10_000);
                })
                .unwrap();
            let picture = super::screen_text(&terminal);
            for expected in row["visible"].as_array().unwrap() {
                assert!(
                    picture.contains(expected.as_str().unwrap()),
                    "{name}: {picture}"
                );
            }
        }
    }
}
