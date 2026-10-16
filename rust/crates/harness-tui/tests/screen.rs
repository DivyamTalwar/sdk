#![cfg(feature = "tui")]
use acyclic_harness_tui::ratatui::{Terminal, backend::TestBackend, layout::Rect, text::Line};
use acyclic_harness_tui::{Editor, PromptInput, Screen, Viewport};
use proptest::prelude::*;

fn visible(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn composition_renders_followed_panels_prompt_and_status() -> Result<(), Box<dyn std::error::Error>>
{
    let transcript = [Line::from("streaming assistant")];
    let terminal_rows = [Line::from("shell output")];
    let diff = [Line::from("+new line")];
    let goal = [Line::from("finish rewrite")];
    let plans = [Line::from("implement layout")];
    let notices = [Line::from("task completed")];
    let screen = Screen {
        header: Line::from("parent / ● current"),
        transcript: &transcript,
        terminal: &terminal_rows,
        diff: &diff,
        goal: &goal,
        plans: &plans,
        notifications: &notices,
        status: Line::from("Esc · model · tokens"),
        ..Screen::default()
    };
    let input = PromptInput::new(Editor::new("draft"));
    let mut viewport = Viewport::default();
    let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
    terminal.draw(|frame| {
        screen.render(frame, &input, &mut viewport, 0, 1000);
    })?;
    let text = visible(&terminal);
    for expected in [
        "parent / ● current",
        "streaming assistant",
        "shell output",
        "+new line",
        "GOAL.md",
        "finish rewrite",
        "plans/",
        "implement layout",
        "task completed",
        "draft",
        "Esc · model · tokens",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
    assert!(viewport.cursor.is_some());
    Ok(())
}

#[test]
fn completion_overlays_above_prompt_and_resize_preserves_input()
-> Result<(), Box<dyn std::error::Error>> {
    let completions = [Line::from("/help"), Line::from("/theme")];
    let screen = Screen {
        completions: &completions,
        ..Screen::default()
    };
    let input = PromptInput::new(Editor::new("界\nsecond"));
    let mut viewport = Viewport::default();
    let mut terminal = Terminal::new(TestBackend::new(100, 20))?;
    terminal.draw(|frame| {
        let areas = screen.render(frame, &input, &mut viewport, 0, 1000);
        assert_eq!(areas.completions.bottom(), areas.prompt.y);
        assert_eq!(areas.completions.width, 60);
    })?;
    assert!(visible(&terminal).contains("/help"));
    terminal.backend_mut().resize(20, 10);
    terminal.resize(Rect::new(0, 0, 20, 10))?;
    terminal.draw(|frame| {
        screen.render(frame, &input, &mut viewport, 0, 1000);
    })?;
    assert!(visible(&terminal).contains("second"));
    assert_eq!(input.editor.text(), "界\nsecond");
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    // Property: every visible region stays inside the supplied viewport,
    // primary regions never overlap, and prompt growth stays bounded.
    #[test]
    fn layout_is_bounded_at_every_size(width in 0u16..160, height in 0u16..80, x in 0u16..10, y in 0u16..10, count in 0usize..100) {
        let rows = vec![Line::from("row"); count];
        let screen = Screen { terminal: &rows, diff: &rows, goal: &rows, plans: &rows, notifications: &rows, completions: &rows, ..Screen::default() };
        let input = PromptInput::new(Editor::new(&"line\n".repeat(count)));
        let outer = Rect::new(x, y, width, height);
        let areas = screen.areas(outer, &input);
        let primary = [areas.header, areas.transcript, areas.side, areas.terminal, areas.notifications, areas.prompt, areas.status];
        for area in primary.into_iter().chain([areas.completions]) {
            prop_assert!(area.x >= outer.x && area.y >= outer.y);
            prop_assert!(area.right() <= outer.right() && area.bottom() <= outer.bottom());
        }
        for (i, a) in primary.iter().enumerate() {
            for b in &primary[i + 1..] {
                prop_assert!(a.intersection(*b).is_empty());
            }
        }
        prop_assert!(areas.prompt.height <= height / 2);
    }
}
