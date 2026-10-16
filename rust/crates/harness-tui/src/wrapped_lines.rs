use crate::{Editor, Layout};
use ratatui::text::{Line, Span};

/// Disposable width projection of styled logical lines. Callers supply the
/// earliest changed logical row; unchanged prefixes are never laid out again.
#[derive(Default)]
pub struct WrappedLines {
    width: Option<u16>,
    starts: Vec<usize>,
    rows: Vec<Line<'static>>,
}

impl WrappedLines {
    /// Reflow changed rows, or the whole projection after a width change.
    pub fn update(&mut self, source: &[Line<'_>], changed: usize, width: u16) {
        let changed = if self.width == Some(width) {
            changed.min(source.len()).min(self.starts.len())
        } else {
            self.width = Some(width);
            0
        };
        self.rows
            .truncate(self.starts.get(changed).copied().unwrap_or(self.rows.len()));
        self.starts.truncate(changed);
        for line in &source[changed..] {
            self.starts.push(self.rows.len());
            self.rows.extend(wrap(line, width));
        }
    }

    /// Borrow the last projection. No per-frame transcript clone is needed.
    pub fn rows(&self) -> &[Line<'static>] {
        &self.rows
    }
}

fn wrap(line: &Line<'_>, width: u16) -> Vec<Line<'static>> {
    let mut text = String::new();
    let mut styles = Vec::new();
    let mut carriage_return = false;
    for span in &line.spans {
        // Normalize before recording offsets, matching the shared prompt layout.
        let normalized = crate::buffer::normalize(&span.content);
        text.push_str(if carriage_return && span.content.starts_with('\n') {
            normalized.strip_prefix('\n').unwrap_or_default()
        } else {
            &normalized
        });
        if !span.content.is_empty() {
            carriage_return = span.content.ends_with('\r');
        }
        styles.push((text.len(), span.style));
    }
    let layout = Layout::new(&Editor::new(&text), usize::from(width));
    let mut rows: Vec<_> = layout
        .rows
        .iter()
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for cell in &row.cells {
                let index = styles.partition_point(|(end, _)| *end <= cell.source.start);
                let style = styles
                    .get(index)
                    .map(|(_, style)| *style)
                    .unwrap_or_default();
                if let Some(last) = spans.last_mut().filter(|last| last.style == style) {
                    last.content.to_mut().push_str(&cell.symbol);
                } else {
                    spans.push(Span::styled(cell.symbol.clone(), style));
                }
            }
            let mut row = Line::from(spans).style(line.style);
            row.alignment = line.alignment;
            row
        })
        .collect();
    // Prompt layout reserves an extra cursor row at an exact-width end. A
    // transcript has no insertion cursor and must not gain that blank row.
    if rows.len() > 1
        && rows.last().is_some_and(|row| row.spans.is_empty())
        && !text.ends_with('\n')
    {
        rows.pop();
    }
    rows
}
