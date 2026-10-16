use super::{GoalFileView, GoalQueryView};
use crate::goal::GoalStatus;
use acyclic_harness_tui::ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Widget},
};

/// Goal presentation borrowing only the shared ClientViews fold.
pub struct GoalPanel<'a> {
    /// Status and count at their independent filesystem publication cuts.
    pub view: GoalQueryView<'a>,
}

impl Widget for GoalPanel<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.render_counted(area, buffer);
    }
}

impl GoalPanel<'_> {
    // Counts source scalars actually traversed, including clipped text. A redraw
    // never searches an unbounded line for its next newline.
    fn render_counted(self, area: Rect, buffer: &mut Buffer) -> usize {
        let block = Block::bordered().title("Goal");
        let body = block.inner(area);
        block.render(area, buffer);
        if body.is_empty() {
            return 0;
        }
        let (status, reason, markdown, color) = match self.view.goal {
            GoalFileView::Missing => ("missing", None, "", Color::DarkGray),
            GoalFileView::Invalid { reason } => ("invalid", Some(reason.as_str()), "", Color::Red),
            GoalFileView::Present { goal } => (
                match goal.status {
                    GoalStatus::Active => "active",
                    GoalStatus::Blocked => "blocked",
                    GoalStatus::Done => "done",
                },
                goal.reason.as_deref(),
                goal.markdown.as_str(),
                match goal.status {
                    GoalStatus::Active => Color::Yellow,
                    GoalStatus::Blocked => Color::Red,
                    GoalStatus::Done => Color::Green,
                },
            ),
        };
        let count = self.view.progress.map_or_else(
            || "unknown".into(),
            |progress| progress.continuations.to_string(),
        );
        let title = format!("Status: {status} | Continuations: {count}");
        let style = Style::default().fg(color);
        let width = usize::from(body.width);
        let mut work = 0;
        let mut y = body.y;
        for text in [
            Some(("", title.as_str())),
            reason.map(|text| ("Reason: ", text)),
        ]
        .into_iter()
        .flatten()
        {
            if y == body.bottom() {
                return work;
            }
            let safe = escaped(text.0.chars().chain(text.1.chars()).take(width), &mut work);
            buffer.set_stringn(body.x, y, safe, width, style);
            y += 1;
        }
        let budget = (width + 1) * usize::from(body.bottom() - y);
        let mut line = String::new();
        let mut scalars = 0;
        let mut trailing_cr = false;
        // The viewport bounds both output and traversal. If a clipped line's
        // newline lies outside this cut, later lines are outside this viewport.
        for ch in markdown.chars().take(budget) {
            work += 1;
            if ch == '\n' {
                if trailing_cr {
                    line.truncate(line.len() - 2);
                }
                buffer.set_stringn(body.x, y, &line, width, style);
                line.clear();
                scalars = 0;
                trailing_cr = false;
                y += 1;
                if y == body.bottom() {
                    return work;
                }
            } else if scalars < width {
                escape(ch, &mut line);
                scalars += 1;
                trailing_cr = ch == '\r';
            } else {
                trailing_cr = false;
            }
        }
        if y < body.bottom() {
            buffer.set_stringn(body.x, y, line, width, style);
        }
        work
    }
}

fn escaped(chars: impl Iterator<Item = char>, work: &mut usize) -> String {
    let mut text = String::new();
    for ch in chars {
        *work += 1;
        escape(ch, &mut text);
    }
    text
}

fn escape(ch: char, text: &mut String) {
    if ch.is_control() {
        text.extend(ch.escape_debug());
    } else {
        text.push(ch);
    }
}

#[cfg(test)]
mod tests;
