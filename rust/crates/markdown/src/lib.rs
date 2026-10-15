//! Streaming Markdown as styled, unwrapped lines. Feed one instance per text
//! stream; keep viewport layout and task/query ownership in the consuming client.

mod code;
mod events;
mod references;
mod render;
mod style;
mod table;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use references::{Key, References, Template, normalize};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};
pub use style::{Color, Line, Span, Style};

/// Disposable incremental presentation of one append-only Markdown stream.
/// Completed leaves and fence lines remain behind a parser checkpoint, including
/// leaves inside containers. Reference definitions select cached inline nodes.
#[derive(Default)]
pub struct Markdown {
    source: String,
    offset: usize,
    stable_lines: usize,
    lines: Vec<Line>,
    state: render::Renderer,
    context: Vec<Container>,
    references: References,
    dependencies: HashMap<Key, Vec<usize>>,
    cached: Vec<Cached>,
    fence: Option<Fence>,
    plain: bool,
    plain_spaces: usize,
    cr: bool,
    #[cfg(test)]
    parsed_bytes: usize,
}

#[derive(Clone)]
struct Container {
    end: TagEnd,
    opening: String,
    continuation: String,
}

struct Cached {
    template: Template,
    state: render::Renderer,
    lines: Range<usize>,
}

impl Cached {
    fn new(
        source: &str,
        range: Range<usize>,
        lines: Range<usize>,
        state: render::Renderer,
    ) -> Option<Self> {
        let quotes = source
            .get(line_start(source, range.start)..range.start)
            .unwrap_or_default()
            .bytes()
            .filter(|byte| *byte == b'>')
            .count();
        let raw = source.get(range).unwrap_or_default();
        if !raw.contains('[') {
            return None;
        }
        Some(Self {
            template: Template::new(raw, quotes),
            state,
            lines,
        })
    }
}

struct Fence {
    marker: u8,
    count: usize,
    indent: usize,
    opening: String,
    parser_opening: String,
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
        let appended = self.source.get(previous..).unwrap_or_default();
        if self.plain && plain_text(appended) {
            if let Some(line) = self.lines.get_mut(old_len.saturating_sub(1)) {
                let visible = appended.trim_end_matches(' ');
                if visible.is_empty() {
                    self.plain_spaces += appended.len();
                } else {
                    if let Some(span) = line.spans.last_mut() {
                        span.text
                            .extend(std::iter::repeat_n(' ', self.plain_spaces));
                        span.text.push_str(visible);
                    }
                    self.plain_spaces = appended.len() - visible.len();
                }
            }
            return old_len.saturating_sub(1);
        }
        self.plain = false;
        let mut changed = self.stable_lines.min(old_len);
        self.lines.truncate(self.stable_lines);
        if self.fence.is_some() && !self.advance_fence() {
            return changed;
        }
        loop {
            let tail = self.source.get(self.offset..).unwrap_or_default();
            let prefix = context_prefix(&self.context);
            let source = format!("{prefix}{tail}");
            let keys = self.references.update(&source, self.offset, prefix.len());
            changed = changed.min(self.resolve(&keys));
            if !self.parse_tail() {
                break;
            }
        }
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

    fn resolve(&mut self, keys: &HashSet<Key>) -> usize {
        let mut affected: HashSet<usize> = HashSet::new();
        for key in keys {
            if let Some(indices) = self.dependencies.get(key) {
                affected.extend(indices);
            }
        }
        let mut changed = self.stable_lines;
        let mut affected: Vec<_> = affected.into_iter().collect();
        affected.sort_unstable_by_key(|index| {
            std::cmp::Reverse(self.cached.get(*index).map_or(0, |cache| cache.lines.start))
        });
        for index in affected {
            let Some(cache) = self.cached.get(index) else {
                continue;
            };
            let mut renderer = cache.state.checkpoint();
            for event in cache.template.events(&self.references) {
                renderer.event(&event);
            }
            renderer.flush();
            let range = cache.lines.clone();
            let length = renderer.lines.len();
            changed = changed.min(range.start);
            if length == range.len() {
                for (target, line) in self
                    .lines
                    .get_mut(range)
                    .unwrap_or_default()
                    .iter_mut()
                    .zip(renderer.lines)
                {
                    *target = line;
                }
            } else {
                self.lines.splice(range.clone(), renderer.lines);
                self.stable_lines = self.stable_lines - range.len() + length;
                for (other, cache) in self.cached.iter_mut().enumerate() {
                    if other == index {
                        cache.lines.end = range.start + length;
                    } else if cache.lines.start >= range.end {
                        cache.lines.start = cache.lines.start - range.len() + length;
                        cache.lines.end = cache.lines.end - range.len() + length;
                    }
                }
            }
        }
        changed
    }

    fn parse_tail(&mut self) -> bool {
        let base = self.offset;
        let prefix = context_prefix(&self.context);
        let source = format!("{prefix}{}", self.source.get(base..).unwrap_or_default());
        #[cfg(test)]
        {
            self.parsed_bytes += source.len();
        }
        let events = events::parse(&source, &self.references);
        self.seal_leading_definitions(base, &source, &events);
        let mut out = self.state.checkpoint();
        let mut stack = self.context.clone();
        let mut candidate = None;
        let mut leaf = None;
        let mut caches: Vec<(usize, Cached)> = Vec::new();
        let mut synthetic_leaf = false;
        for (index, (event, range)) in events.iter().enumerate() {
            if let Event::Start(tag) = event
                && range.start < prefix.len()
            {
                synthetic_leaf = is_leaf(tag);
                continue;
            }
            if synthetic_leaf {
                if let Event::End(end) = event
                    && is_leaf_end(*end)
                {
                    synthetic_leaf = false;
                }
                continue;
            }
            if let Event::Start(tag) = event {
                if is_leaf(tag) {
                    leaf = Some((
                        range.start,
                        self.lines.len() + out.lines.len(),
                        out.checkpoint(),
                    ));
                    if self.start_fence(&source, (base, prefix.len()), range, tag, &mut out, &stack)
                    {
                        for (end, cache) in caches {
                            if base + end.saturating_sub(prefix.len()) <= self.offset {
                                self.cache(cache);
                            }
                        }
                        return self.advance_fence();
                    }
                }
                stack.push(container(&source, range.start, tag));
            }
            out.event(event);
            if let Event::End(end) = event {
                if stack.last().is_some_and(|container| container.end == *end) {
                    stack.pop();
                }
                let leaf_end = is_leaf_end(*end);
                if leaf_end
                    && let Some((start, first, state)) = leaf.take()
                    && let Some(cache) = Cached::new(
                        &source,
                        start..range.end,
                        first..self.lines.len() + out.lines.len(),
                        state,
                    )
                {
                    caches.push((range.end, cache));
                }
                if stack.is_empty() {
                    out.lines.push(Line::default());
                }
                let following = events.get(index + 1..).unwrap_or_default();
                let next_block = following.iter().any(|(event, range)| {
                    matches!(event, Event::Start(_))
                        && source
                            .get(range.start..)
                            .is_some_and(|tail| tail.contains('\n'))
                });
                if range.end >= prefix.len()
                    && (leaf_end || (stack.is_empty() && next_block))
                    && sealed(&source, range.end, *end, following)
                {
                    candidate = Some((
                        base + range.end - prefix.len(),
                        self.lines.len() + out.lines.len(),
                        out.checkpoint(),
                        stack.clone(),
                    ));
                }
            } else if stack.is_empty() {
                out.flush();
                out.lines.push(Line::default());
            }
        }
        out.flush();
        self.lines.extend(out.lines);
        self.finish_tail(candidate, caches, (base, prefix.len()));
        self.finish_definitions(&source, &events, (base, prefix.len()));
        false
    }

    fn seal_leading_definitions(
        &mut self,
        base: usize,
        source: &str,
        events: &[(Event<'_>, Range<usize>)],
    ) {
        if self.context.is_empty()
            && let Some((_, first)) = events.first()
            && source
                .get(..first.start)
                .is_some_and(|prefix| prefix.ends_with("\n\n"))
        {
            self.offset = base + first.start;
            self.references.commit(self.offset);
        }
    }

    fn start_fence(
        &mut self,
        source: &str,
        (base, prefix): (usize, usize),
        range: &Range<usize>,
        tag: &Tag<'_>,
        out: &mut render::Renderer,
        stack: &[Container],
    ) -> bool {
        let Tag::CodeBlock(CodeBlockKind::Fenced(info)) = tag else {
            return false;
        };
        let row = source.get(range.start..).unwrap_or_default();
        let Some((marker, count, indent, _)) = code::opening(row) else {
            return false;
        };
        out.event(&Event::Start(tag.clone()));
        self.state = out.checkpoint();
        self.lines.append(&mut out.lines);
        let containers = context_prefix(stack);
        self.context = stack.to_vec();
        let length = row.find('\n').map_or(0, |position| position + 1);
        self.offset = base + range.start.saturating_sub(prefix) + length;
        self.stable_lines = self.lines.len();
        self.fence = Some(Fence {
            marker,
            count,
            indent,
            opening: source
                .get(line_start(source, range.start)..range.start)
                .unwrap_or_default()
                .to_owned(),
            parser_opening: format!(
                "{containers}{}",
                source
                    .get(line_start(source, range.start)..range.start + length)
                    .unwrap_or_default()
            ),
            code: code::highlighter(info.split_whitespace().next().unwrap_or_default()),
        });
        self.references.commit(self.offset);
        true
    }

    fn finish_tail(
        &mut self,
        candidate: Option<(usize, usize, render::Renderer, Vec<Container>)>,
        caches: Vec<(usize, Cached)>,
        (base, prefix): (usize, usize),
    ) {
        if let Some((offset, lines, state, context)) = candidate {
            self.offset = offset;
            self.stable_lines = lines;
            self.state = state;
            self.context = context;
            for (end, cache) in caches {
                if base + end.saturating_sub(prefix) <= offset && !cache.template.keys.is_empty() {
                    self.cache(cache);
                }
            }
        }
    }

    fn finish_definitions(
        &mut self,
        source: &str,
        events: &[(Event<'_>, Range<usize>)],
        (base, prefix): (usize, usize),
    ) {
        // Definition-only prefixes have no display events. Seal definitions
        // terminated by a blank line without retaining an ever-growing suffix.
        let tail = self.source.get(self.offset..).unwrap_or_default();
        if events.is_empty() && tail.ends_with("\n\n") {
            self.offset = self.source.len();
        } else if events.is_empty() {
            self.offset = self.references.frontier(self.offset);
        }
        if self.context.is_empty() {
            while self.source.as_bytes().get(self.offset) == Some(&b'\n') {
                self.offset += 1;
            }
        }
        self.references.commit(self.offset);
        let tail = self.source.get(self.offset..).unwrap_or_default();
        let frontier = self.offset.saturating_sub(base) + prefix;
        let mut leaves = events.iter().filter_map(|(event, range)| match event {
            Event::Start(tag) if is_leaf(tag) && range.start >= frontier => Some(tag),
            _ => None,
        });
        let only_inline_leaf = matches!(leaves.next(), Some(Tag::Paragraph | Tag::Heading { .. }))
            && leaves.next().is_none();
        self.plain = tail.len() >= 8
            && tail.chars().next_back().is_some_and(plain_character)
            && only_inline_leaf
            && events
                .iter()
                .rev()
                .find(|(event, _)| !matches!(event, Event::End(_)))
                .is_some_and(|(event, range)| {
                    matches!(event, Event::Text(_))
                        && range.end == source.trim_end_matches(' ').len()
                });
        self.plain_spaces = tail.bytes().rev().take_while(|byte| *byte == b' ').count();
    }

    fn cache(&mut self, cache: Cached) {
        if cache.template.keys.is_empty() {
            return;
        }
        let index = self.cached.len();
        for key in &cache.template.keys {
            self.dependencies
                .entry(key.clone())
                .or_default()
                .push(index);
        }
        self.cached.push(cache);
    }

    fn advance_fence(&mut self) -> bool {
        let Some(mut fence) = self.fence.take() else {
            return true;
        };
        let mut out = self.state.checkpoint();
        let tail = self.source.get(self.offset..).unwrap_or_default();
        for raw in tail.split_inclusive('\n') {
            let decoded = fence_line(raw, &fence);
            if matches!(decoded, FenceLine::Outside) {
                out.event(&Event::End(TagEnd::CodeBlock));
                self.state = out.checkpoint();
                self.lines.extend(out.lines);
                if self.context.is_empty() {
                    self.lines.push(Line::default());
                }
                self.stable_lines = self.lines.len();
                return true;
            }
            if !raw.ends_with('\n') {
                break;
            }
            self.offset += raw.len();
            if matches!(decoded, FenceLine::Close) {
                out.event(&Event::End(TagEnd::CodeBlock));
                self.state = out.checkpoint();
                self.lines.extend(out.lines);
                if self.context.is_empty() {
                    self.lines.push(Line::default());
                }
                self.stable_lines = self.lines.len();
                return true;
            }
            if let FenceLine::Text(line) = decoded {
                out.block_line(code::highlight(&mut fence.code, &line));
            }
        }
        self.lines.extend(out.lines);
        self.stable_lines = self.lines.len();
        let raw = self.source.get(self.offset..).unwrap_or_default();
        let mut preview = self.state.checkpoint();
        if let FenceLine::Text(content) = fence_line(raw, &fence)
            && !content.is_empty()
        {
            preview.block_line(code::preview(&fence.code, &content));
        }
        preview.block_line(code::footer());
        self.lines.extend(preview.lines);
        self.fence = Some(fence);
        false
    }
}

fn plain_text(text: &str) -> bool {
    text.chars().all(plain_character)
}

fn plain_character(character: char) -> bool {
    !character.is_ascii() || character.is_ascii_alphanumeric() || character == ' '
}

fn is_leaf(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Paragraph | Tag::Heading { .. } | Tag::CodeBlock(_) | Tag::Table(_) | Tag::HtmlBlock
    )
}

fn is_leaf_end(tag: TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::CodeBlock
            | TagEnd::Table
            | TagEnd::HtmlBlock
    )
}

fn line_start(source: &str, position: usize) -> usize {
    source
        .get(..position)
        .and_then(|text| text.rfind('\n'))
        .map_or(0, |position| position + 1)
}

fn container(source: &str, position: usize, tag: &Tag<'_>) -> Container {
    if !matches!(tag, Tag::Item | Tag::BlockQuote(_)) {
        return Container {
            end: tag.to_end(),
            opening: String::new(),
            continuation: String::new(),
        };
    }
    let line = source
        .get(line_start(source, position)..)
        .unwrap_or_default();
    let before = position - line_start(source, position);
    let parent: String = line
        .get(..before)
        .unwrap_or_default()
        .chars()
        .map(|character| {
            if character == '>' || character.is_ascii_whitespace() {
                character
            } else {
                ' '
            }
        })
        .collect();
    let length = match tag {
        Tag::Item => {
            let marker = line.get(before..).unwrap_or_default();
            let end = marker.find([' ', '\t', '\n']).unwrap_or(marker.len());
            before
                + end
                + usize::from(
                    marker
                        .get(end..)
                        .is_some_and(|tail| tail.starts_with([' ', '\t'])),
                )
        }
        Tag::BlockQuote(_) => before + 1,
        _ => {
            return Container {
                end: tag.to_end(),
                opening: String::new(),
                continuation: String::new(),
            };
        }
    };
    let marker = line.get(before..length).unwrap_or_default();
    let opening = format!(
        "{parent}{marker}{}\n",
        if matches!(tag, Tag::Item) { "x" } else { "" }
    );
    let continuation = if matches!(tag, Tag::Item) {
        format!("{parent}{}\n", " ".repeat(marker.len()))
    } else {
        opening.clone()
    };
    Container {
        end: tag.to_end(),
        opening,
        continuation,
    }
}

fn context_prefix(context: &[Container]) -> String {
    let mut prefix: String = context
        .iter()
        .map(|container| container.opening.as_str())
        .collect();
    if let Some(container) = context
        .iter()
        .rev()
        .find(|container| !container.continuation.is_empty())
    {
        prefix.push_str(&container.continuation);
    }
    prefix
}

enum FenceLine<'a> {
    Text(std::borrow::Cow<'a, str>),
    Close,
    Outside,
}

fn fence_line<'a>(line: &'a str, fence: &Fence) -> FenceLine<'a> {
    if fence.opening.is_empty() {
        return if code::closing(line, fence.marker, fence.count) {
            FenceLine::Close
        } else {
            FenceLine::Text(code::dedent(line, fence.indent).into())
        };
    }
    let source = format!("{}{line}", fence.parser_opening);
    let mut inside = false;
    let mut text = String::new();
    for (event, range) in Parser::new_ext(&source, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => inside = true,
            Event::Text(content) if inside => text.push_str(&content),
            Event::End(TagEnd::CodeBlock) => {
                if source.get(range.end..).is_some_and(|rest| {
                    !rest
                        .trim_matches(|character: char| character.is_ascii_whitespace())
                        .is_empty()
                }) {
                    return FenceLine::Outside;
                }
                return if text.is_empty() && !line.is_empty() {
                    FenceLine::Close
                } else {
                    FenceLine::Text(text.into())
                };
            }
            _ => {}
        }
    }
    FenceLine::Outside
}

fn sealed(source: &str, end: usize, tag: TagEnd, following: &[(Event<'_>, Range<usize>)]) -> bool {
    let rest = source.get(end..).unwrap_or_default();
    let next_complete = following
        .iter()
        .find(|(event, _)| matches!(event, Event::Start(_)))
        .is_some_and(|(_, range)| {
            source
                .get(range.start..)
                .is_some_and(|tail| tail.contains('\n'))
        });
    if tag == TagEnd::Table {
        let rest = if source.as_bytes().get(end.saturating_sub(1)) == Some(&b'\n') {
            rest
        } else {
            rest.strip_prefix('\n').unwrap_or(rest)
        };
        return rest.starts_with('\n') || next_complete;
    }
    if rest.starts_with('\n') || rest.starts_with("\r\n") {
        return true;
    }
    next_complete
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

#[cfg(test)]
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
