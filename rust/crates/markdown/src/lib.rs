//! Streaming Markdown as styled, unwrapped lines. Feed one instance per text
//! stream; keep viewport layout and task/query ownership in the consuming client.

mod code;
mod render;
mod style;
mod table;

use pulldown_cmark::{BrokenLink, CodeBlockKind, Event, Options, Parser, Tag};
use std::{collections::BTreeMap, ops::Range};
pub use style::{Color, Line, Span, Style};

type References = BTreeMap<unicase::UniCase<String>, (String, String, usize)>;

/// Disposable incremental presentation of one append-only Markdown stream.
/// Completed blocks are retained. Only the open block is parsed again, and
/// open top-level code fences advance a lexer checkpoint one line at a time.
#[derive(Default)]
pub struct Markdown {
    source: String,
    offset: usize,
    stable_lines: usize,
    lines: Vec<Line>,
    references: References,
    fence: Option<Fence>,
    cr: bool,
}

struct Fence {
    marker: u8,
    count: usize,
    indent: usize,
    code: code::Code,
}

impl Markdown {
    /// Append a UTF-8 text delta and return the first changed display line.
    /// A consumer can truncate its presentation there and replace the suffix
    /// from [`Self::lines`]. Empty deltas leave the presentation unchanged.
    pub fn push(&mut self, delta: &str) -> usize {
        let old_len = self.lines().len();
        if delta.is_empty() {
            return old_len;
        }
        let previous = self.source.len();
        self.append(delta);
        if self.source.len() == previous {
            return old_len;
        }
        let mut changed = self.stable_lines.min(old_len);
        self.lines.truncate(self.stable_lines);
        if self.fence.is_some() && !self.advance_fence() {
            return changed;
        }

        let tail = self.source.get(self.offset..).unwrap_or_default();
        let parser = Parser::new_ext(tail, options());
        let mut references = self.references.clone();
        references.retain(|_, (_, _, position)| *position < self.offset);
        for (label, definition) in parser.reference_definitions().iter() {
            let position = self.offset + definition.span.start;
            let value = (
                definition.dest.to_string(),
                definition
                    .title
                    .as_ref()
                    .map_or_else(String::new, ToString::to_string),
                position,
            );
            let entry = references
                .entry(normalize(label))
                .or_insert_with(|| value.clone());
            if position <= entry.2 {
                *entry = value;
            }
        }
        if references != self.references {
            // A later reference definition can resolve labels in any earlier
            // block. Rebuild on definition changes, never on ordinary deltas.
            self.references = references;
            self.offset = 0;
            self.stable_lines = 0;
            self.lines.clear();
            changed = 0;
        }
        self.parse_tail();
        changed
    }

    /// Current unwrapped lines. A partial delimiter remains visible until it
    /// forms valid Markdown; no input is hidden waiting for a future delta.
    pub fn lines(&self) -> &[Line] {
        let end = self
            .lines
            .iter()
            .rposition(|line| !line.spans.is_empty())
            .map_or(0, |index| index + 1);
        self.lines.get(..end).unwrap_or_default()
    }

    /// Discard this presentation when a query resets or selects another turn.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn append(&mut self, delta: &str) {
        if !self.cr && !delta.contains('\r') {
            self.source.push_str(delta);
            return;
        }
        for character in delta.chars() {
            if character == '\n' && self.cr {
                self.cr = false;
                continue;
            }
            self.cr = character == '\r';
            self.source.push(if self.cr { '\n' } else { character });
        }
    }

    fn parse_tail(&mut self) {
        let source = self.source.get(self.offset..).unwrap_or_default();
        let mut callback = |link: BrokenLink<'_>| {
            self.references
                .get(&normalize(&link.reference))
                .map(|(dest, title, _)| (dest.clone().into(), title.clone().into()))
        };
        let parser = Parser::new_with_broken_link_callback(source, options(), Some(&mut callback));
        let events = parser.into_offset_iter().map(|(mut event, range)| {
            if let Event::Start(
                Tag::Link {
                    id,
                    dest_url,
                    title,
                    ..
                }
                | Tag::Image {
                    id,
                    dest_url,
                    title,
                    ..
                },
            ) = &mut event
                && !id.is_empty()
                && let Some((destination, label, _)) = self.references.get(&normalize(id))
            {
                *dest_url = destination.clone().into();
                *title = label.clone().into();
            }
            (event, range)
        });
        let blocks = blocks(events);
        let count = blocks.len();
        // A partial next line can still become part of the preceding table or
        // container. Seal that predecessor only once the next line is complete.
        let pending = blocks
            .last()
            .and_then(|(range, _)| source.get(range.start..))
            .map_or(2, |tail| if tail.contains('\n') { 1 } else { 2 });
        let commit_before = count.saturating_sub(pending);
        let base = self.offset;
        for (index, (range, events)) in blocks.into_iter().enumerate() {
            let last = index + 1 == count;
            let line_start = source
                .get(..range.start)
                .and_then(|prefix| prefix.rfind('\n'))
                .map_or(0, |position| position + 1);
            let block_start = if source
                .get(line_start..range.start)
                .is_some_and(|prefix| prefix.bytes().all(|byte| byte == b' '))
            {
                line_start
            } else {
                range.start
            };
            let block_source = source.get(block_start..range.end).unwrap_or_default();
            if last
                && matches!(
                    events.first(),
                    Some(Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))))
                )
                && let Some((marker, count, indent, info)) = code::opening(block_source)
            {
                let language = info.split_whitespace().next().unwrap_or_default();
                self.lines.push(code::header(language));
                self.fence = Some(Fence {
                    marker,
                    count,
                    indent,
                    code: code::highlighter(language),
                });
                self.offset =
                    base + block_start + block_source.find('\n').map_or(0, |position| position + 1);
                self.stable_lines = self.lines.len();
                break;
            }
            self.lines.extend(render::render(&events));
            self.lines.push(Line::default());
            if index < commit_before {
                self.offset = base + range.end;
                self.stable_lines = self.lines.len();
            }
        }
        if self.fence.is_some() {
            self.advance_fence();
        }
    }

    fn advance_fence(&mut self) -> bool {
        let Some(fence) = &mut self.fence else {
            return true;
        };
        let tail = self.source.get(self.offset..).unwrap_or_default();
        for line in tail.split_inclusive('\n') {
            if !line.ends_with('\n') {
                break;
            }
            self.offset += line.len();
            if code::closing(line, fence.marker, fence.count) {
                self.lines.push(code::footer());
                self.lines.push(Line::default());
                self.stable_lines = self.lines.len();
                self.fence = None;
                return true;
            }
            self.lines.push(code::highlight(
                &mut fence.code,
                code::dedent(line, fence.indent),
            ));
        }
        self.stable_lines = self.lines.len();
        let partial = self.source.get(self.offset..).unwrap_or_default();
        let content = code::dedent(partial, fence.indent);
        if !content.is_empty() && !code::closing(partial, fence.marker, fence.count) {
            self.lines.push(code::preview(&fence.code, content));
        }
        self.lines.push(code::footer());
        false
    }
}

fn normalize(label: &str) -> unicase::UniCase<String> {
    unicase::UniCase::new(label.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

fn blocks<'a>(
    events: impl Iterator<Item = (Event<'a>, Range<usize>)>,
) -> Vec<(Range<usize>, Vec<Event<'a>>)> {
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut block = Vec::new();
    for (event, range) in events {
        if depth == 0 {
            start = range.start;
        }
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        block.push(event);
        if depth == 0 {
            blocks.push((start..range.end, std::mem::take(&mut block)));
        }
    }
    blocks
}

#[cfg(test)]
#[path = "../tests/full_parse.rs"]
mod tests;
