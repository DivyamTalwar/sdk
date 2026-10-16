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
        let block = Block::bordered().title("Goal");
        let body = block.inner(area);
        block.render(area, buffer);
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
        let reason = reason.map(|reason| format!("Reason: {reason}"));
        for (y, line) in (body.y..body.bottom()).zip(
            std::iter::once(title.as_str())
                .chain(reason.as_deref())
                .chain(markdown.lines()),
        ) {
            let mut safe = String::new();
            for ch in line.chars() {
                if ch.is_control() {
                    safe.extend(ch.escape_debug());
                } else {
                    safe.push(ch);
                }
            }
            buffer.set_stringn(
                body.x,
                y,
                safe,
                usize::from(body.width),
                Style::default().fg(color),
            );
        }
    }
}
