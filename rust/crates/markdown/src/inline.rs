use super::*;

#[derive(Default, Clone, Copy)]
struct Mark {
    line: usize,
    visible: usize,
    spans: usize,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct Checkpoint {
    offset: usize,
    mark: Mark,
    cost: usize,
    pub(super) valid: Range<usize>,
}

#[derive(Default)]
pub(super) struct Inline {
    pub(super) checkpoint: Checkpoint,
    start: usize,
    scanner: inline_scan::Scanner,
}

impl Inline {
    #[cfg(test)]
    pub(super) fn retained(&self) -> usize {
        self.scanner.retained()
    }
    pub(super) fn visible_frontier(&self, stable: usize, fallback: usize) -> (usize, usize) {
        let mark = self.checkpoint.mark;
        (
            stable + mark.line,
            if mark.visible == 0 {
                fallback
            } else {
                stable + mark.visible
            },
        )
    }
}

impl Markdown {
    pub(super) fn advance_inline(&mut self, previous: usize) -> Option<usize> {
        let mut inline = self.inline.take()?;
        let checkpoint = &inline.checkpoint;
        let cost = checkpoint.cost;
        let base = checkpoint.offset;
        let mark = checkpoint.mark;
        let tail = self.source.get(base..).unwrap_or_default();
        let context = if base == inline.start {
            String::new()
        } else if self
            .source
            .get(previous..)
            .unwrap_or_default()
            .contains('\n')
            || tail.contains('\n')
        {
            row_prefix(self.source.get(..base).unwrap_or_default())
        } else {
            left_context(self.source.get(..base).unwrap_or_default())
        };
        let prefix = context;
        let sentinel = prefix.len();
        let source = format!("{prefix}{tail}");
        #[cfg(test)]
        work::copy(source.len() + prefix.len());
        let (events, calls) = events::scoped(
            &source,
            &self.references,
            options(),
            self.source
                .len()
                .max(100_000)
                .saturating_sub(self.reference_cost)
                .saturating_sub(cost),
        );
        if !paragraph(&source, &events)
            || events.iter().any(|(event, range)| {
                matches!(event, Event::Start(tag) if !matches!(tag, Tag::Paragraph))
                    && range.start < sentinel
                    && range.end > sentinel
            })
        {
            return None;
        }
        let frontier = inline
            .scanner
            .frontier(&self.source, base, &source, &events, sentinel);
        let boundary = boundary(&source, &events, sentinel, frontier);
        let (cost, valid) = references::extend_expansion(
            cost,
            &checkpoint.valid,
            calls.iter().filter(|(end, _)| *end <= boundary),
        );
        let first = self.stable_lines + mark.line;
        let mut line = self
            .lines
            .get_mut(first)
            .map(std::mem::take)
            .unwrap_or_default();
        self.lines.truncate(first);
        line.spans.truncate(mark.spans);
        if let Some(span) = line.spans.last_mut() {
            span.text.truncate(mark.bytes);
        }
        let mut renderer = render::Renderer::inline(line);
        let mut committed = mark;
        let mut depth = 0usize;
        for (event, range) in &events {
            match event {
                Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => continue,
                Event::Text(text) if source.get(range.clone()) == Some(text.as_ref()) => {
                    let start = sentinel.saturating_sub(range.start).min(text.len());
                    if boundary >= range.start + start && boundary < range.end {
                        let cut = boundary - range.start;
                        renderer.event_in(
                            &Event::Text(text.get(start..cut).unwrap_or_default().into()),
                            Some(&self.references),
                        );
                        committed = renderer.inline_mark(mark);
                        renderer.event_in(
                            &Event::Text(text.get(cut..).unwrap_or_default().into()),
                            Some(&self.references),
                        );
                    } else {
                        renderer.event_in(
                            &Event::Text(text.get(start..).unwrap_or_default().into()),
                            Some(&self.references),
                        );
                    }
                }
                Event::Text(text) if range.start < sentinel => {
                    let text = text.get(sentinel - range.start..).unwrap_or_default();
                    renderer.event_in(&Event::Text(text.into()), Some(&self.references));
                }
                _ => renderer.event_in(event, Some(&self.references)),
            }
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth == 0 && range.end == boundary {
                committed = renderer.inline_mark(mark);
            }
        }
        #[cfg(test)]
        {
            let count = renderer.lines.len() + 1;
            work::copy(count * std::mem::size_of::<Line>());
            if self.lines.capacity() - self.lines.len() < count {
                work::copy(self.lines.len() * std::mem::size_of::<Line>());
            }
        }
        self.lines.reserve(renderer.lines.len() + 1);
        self.lines.append(&mut renderer.lines);
        self.lines.push(renderer.into_inline());
        inline
            .scanner
            .discard_before(base + boundary.saturating_sub(sentinel));
        self.inline = Some(Inline {
            checkpoint: Checkpoint {
                offset: base + boundary.saturating_sub(sentinel),
                mark: committed,
                cost,
                valid,
            },
            start: inline.start,
            scanner: inline.scanner,
        });
        let fallback = if mark.visible == 0 {
            self.visibility.end
        } else {
            self.stable_lines + mark.visible
        };
        self.visible_lines = self.visible_end(first, self.lines.len(), fallback);
        let tail = source.get(sentinel..).unwrap_or_default();
        self.plain_links = tail.contains('[')
            && tail
                .rsplit_once(']')
                .is_none_or(|(_, rest)| rest.starts_with('('));
        self.plain_entity = tail
            .rsplit_once('&')
            .is_some_and(|(_, rest)| rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '#'));
        self.plain_spaces = tail.bytes().rev().take_while(|byte| *byte == b' ').count();
        self.plain = tail.len() >= 8 && !table_candidate(&source) && !block_pending(tail.rsplit('\n').next().unwrap_or_default()) && events.iter().rev().find(|(event, _)| !matches!(event, Event::End(_)))
            .is_some_and(|(event, range)| matches!(event, Event::Text(text) if text.chars().next_back().is_some_and(plain_character))
                && range.end > sentinel && range.end == source.trim_end_matches(' ').len());
        Some(first)
    }

    pub(super) fn begin_inline(
        &mut self,
        source: &str,
        events: &[(Event<'_>, Range<usize>)],
        base: usize,
    ) {
        let content = source.trim_start_matches([' ', '\t']);
        if possible_definition(content)
            || !self.context.is_empty()
            || self.offset != base
            || !paragraph(source, events)
        {
            return;
        }
        self.inline = Some(Inline {
            checkpoint: Checkpoint {
                offset: base + source.len() - source.trim_start_matches([' ', '\t']).len(),
                mark: Mark::default(),
                cost: 0,
                valid: 0..usize::MAX,
            },
            start: base + source.len() - source.trim_start_matches([' ', '\t']).len(),
            scanner: inline_scan::Scanner::new(
                &self.source,
                base + source.len() - source.trim_start_matches([' ', '\t']).len(),
            ),
        });
        // Replay only this initial open tail; subsequent pushes retain its prefix.
        let plain = self.plain;
        let _ = self.advance_inline(self.source.len());
        self.plain = plain;
    }
}

fn left_context(source: &str) -> String {
    match source.chars().next_back() {
        Some(c) if c.is_whitespace() => "x ".into(),
        Some(c) if !c.is_ascii() => format!("x{c}"),
        Some(c) if c.is_alphanumeric() => "x".into(),
        _ => "x.".into(),
    }
}

fn table_candidate(source: &str) -> bool {
    source.rsplit_once('\n').is_some_and(|(_, next)| {
        next.bytes()
            .all(|byte| matches!(byte, b'|' | b'-' | b':' | b' ' | b'\t'))
    })
}

// Only column structure is needed to recognize a table interrupt after a soft
// break. Keep its source text in the rendered checkpoint, not in parser input.
fn row_prefix(source: &str) -> String {
    let row = source.rsplit('\n').next().unwrap_or_default();
    #[cfg(test)]
    work::scan(row.len() * 2);
    if !row.contains('|') {
        return left_context(source);
    }
    #[cfg(test)]
    work::scan(row.len());
    let (mut escaped, mut content) = (false, false);
    let mut prefix = String::new();
    for byte in row.bytes() {
        if byte == b'|' && !escaped {
            prefix.push('|');
            content = false;
        } else if !byte.is_ascii_whitespace() && !content {
            prefix.push('x');
            content = true;
        }
        escaped = byte == b'\\' && !escaped;
    }
    match left_context(source).as_str() {
        "x " => prefix.push(' '),
        "x" => {}
        _ if prefix.ends_with('|') => {}
        context => {
            prefix.pop();
            prefix.push(context.chars().next_back().unwrap_or('.'));
        }
    }
    prefix
}

// Until content follows an empty marker it can still interrupt this paragraph.
fn block_pending(row: &str) -> bool {
    let row = row.trim_start_matches([' ', '\t']);
    if row
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'-' | b'_' | b'*'))
    {
        return true;
    }
    let marker = row.trim_start_matches(|c: char| c.is_ascii_digit());
    let digits = row.len() - marker.len();
    (1..=9).contains(&digits)
        && (marker.is_empty()
            || marker
                .strip_prefix(['.', ')'])
                .is_some_and(|rest| rest.bytes().all(|byte| matches!(byte, b' ' | b'\t'))))
        || row
            .strip_prefix(['-', '+', '*'])
            .is_some_and(|rest| rest.bytes().all(|byte| matches!(byte, b' ' | b'\t')))
}

fn paragraph(source: &str, events: &[(Event<'_>, Range<usize>)]) -> bool {
    matches!(events.first(), Some((Event::Start(Tag::Paragraph), _)))
        && events.iter().filter(|(event, _)| matches!(event, Event::Start(Tag::Paragraph))).count() == 1
        && !events.iter().any(|(event, _)| matches!(event, Event::Start(tag) if super::is_leaf(tag) && !matches!(tag, Tag::Paragraph)))
        && events.last().is_some_and(|(event, range)| matches!(event, Event::End(TagEnd::Paragraph))
            && source.get(range.end..).unwrap_or_default().bytes().all(|byte| matches!(byte, b' ' | b'\t')))
}

fn possible_definition(source: &str) -> bool {
    source.starts_with('[')
        && source
            .split_once(']')
            .is_none_or(|(_, rest)| rest.starts_with(':') || rest.chars().all(char::is_whitespace))
}

fn boundary(
    source: &str,
    events: &[(Event<'_>, Range<usize>)],
    sentinel: usize,
    frontier: usize,
) -> usize {
    let (prefix, row) = source.rsplit_once('\n').unwrap_or(("", source));
    let mut limit = frontier;
    if block_pending(row) {
        limit = limit.min(prefix.len());
    }
    if table_candidate(source) {
        limit = limit.min(source.rfind('\n').unwrap_or(source.len()));
    }
    limit = limit.min(source.trim_end_matches([' ', '\t']).len());
    let mut depth = 0usize;
    let mut boundary = sentinel;
    for (event, range) in events {
        match event {
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => continue,
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 {
            if range.end <= limit && range.end > sentinel {
                boundary = range.end;
            } else if let Event::Text(text) = event
                && source.get(range.clone()) == Some(text.as_ref())
                && range.start <= limit
                && range.end > limit
                && limit > sentinel
            {
                boundary = limit;
            }
        }
    }
    boundary
}

impl render::Renderer {
    fn inline_mark(&self, base: Mark) -> Mark {
        let line = self.inline_line();
        let position = base.line + self.lines.len();
        let visible = if line.spans.is_empty() {
            self.lines
                .iter()
                .rposition(|line| {
                    #[cfg(test)]
                    work::visibility(1);
                    !line.spans.is_empty()
                })
                .map_or(base.visible, |index| base.line + index + 1)
        } else {
            position + 1
        };
        Mark {
            line: position,
            visible,
            spans: line.spans.len(),
            bytes: line.spans.last().map_or(0, |span| span.text.len()),
        }
    }
}
