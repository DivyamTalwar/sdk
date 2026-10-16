use super::*;
#[path = "inline_delimiters.rs"]
mod delimiters;
#[path = "inline_forest.rs"]
mod forest;
#[path = "inline_state.rs"]
mod state;
#[path = "inline_tokens.rs"]
mod tokens;
use state::State;
use tokens::{Kind, Tokens};

#[derive(Default)]
pub(super) struct Scanner {
    tokens: Tokens,
    atomic: Vec<Range<usize>>,
    state: State,
    syntax: Syntax,
    processed: usize,
    fed: usize,
    frontier: usize,
    #[cfg(test)]
    replay: bool,
    #[cfg(test)]
    fed_bytes: usize,
}

#[derive(Default)]
struct Syntax {
    outer: Vec<Range<usize>>,
    opaque: Vec<Range<usize>>,
    links: Vec<Range<usize>>,
}

impl Syntax {
    fn new(source: &str, events: &[(Event<'_>, Range<usize>)]) -> Self {
        let mut syntax = Self::default();
        let mut links = Vec::<(Range<usize>, usize)>::new();
        for (event, range) in events {
            #[cfg(test)]
            work::scan(1);
            let mut consumed = range.clone();
            match event {
                Event::Start(Tag::Link { link_type, .. } | Tag::Image { link_type, .. }) => {
                    // The parser's collapsed-link range excludes its final [].
                    if matches!(
                        link_type,
                        pulldown_cmark::LinkType::Collapsed
                            | pulldown_cmark::LinkType::CollapsedUnknown
                    ) && source
                        .get(range.end..)
                        .is_some_and(|tail| tail.starts_with("[]"))
                    {
                        consumed.end += 2;
                    }
                    if let Some((_, end)) = links.last_mut() {
                        *end = (*end).max(consumed.end);
                    }
                    if syntax
                        .links
                        .last()
                        .is_none_or(|parent| parent.end < consumed.end)
                    {
                        push_range(&mut syntax.links, consumed.clone());
                    }
                    #[cfg(test)]
                    work::copy(
                        std::mem::size_of::<(Range<usize>, usize)>()
                            * (1 + if links.len() == links.capacity() {
                                links.len()
                            } else {
                                0
                            }),
                    );
                    links.push((consumed.clone(), range.start));
                }
                Event::End(TagEnd::Link | TagEnd::Image) => {
                    if let Some((link, content_end)) = links.pop() {
                        push_range(&mut syntax.opaque, content_end..link.end);
                    }
                }
                _ => {
                    if let Some((_, end)) = links.last_mut() {
                        *end = (*end).max(range.end);
                    }
                }
            }
            match event {
                Event::Start(Tag::Link { .. } | Tag::Image { .. }) => {
                    push_range(&mut syntax.outer, consumed);
                }
                Event::InlineHtml(_) => {
                    push_range(&mut syntax.opaque, range.clone());
                    push_range(&mut syntax.outer, range.clone());
                }
                _ => {}
            }
        }
        syntax
    }

    fn absolute(&mut self, base: usize, sentinel: usize) {
        for range in self
            .outer
            .iter_mut()
            .chain(&mut self.opaque)
            .chain(&mut self.links)
        {
            #[cfg(test)]
            work::scan(1);
            *range = base + range.start.saturating_sub(sentinel)
                ..base + range.end.saturating_sub(sentinel);
        }
    }

    fn agrees_before(&self, previous: &Self, base: usize, end: usize) -> bool {
        fn agrees(left: &[Range<usize>], right: &[Range<usize>], base: usize, end: usize) -> bool {
            let clipped = |range: &Range<usize>| {
                #[cfg(test)]
                work::scan(1);
                let range = range.start.max(base)..range.end.min(end);
                (!range.is_empty()).then_some(range)
            };
            left.iter()
                .filter_map(clipped)
                .eq(right.iter().filter_map(clipped))
        }
        agrees(&self.opaque, &previous.opaque, base, end)
            && agrees(&self.links, &previous.links, base, end)
    }
}

fn push_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    #[cfg(test)]
    work::copy(
        std::mem::size_of::<Range<usize>>()
            * (1 + if ranges.len() == ranges.capacity() {
                ranges.len()
            } else {
                0
            }),
    );
    ranges.push(range);
}

fn inside(ranges: &[Range<usize>], cursor: &mut usize, position: usize) -> bool {
    while ranges
        .get(*cursor)
        .is_some_and(|range| range.end <= position)
    {
        *cursor += 1;
    }
    ranges
        .get(*cursor)
        .is_some_and(|range| range.contains(&position))
}

impl Scanner {
    pub(super) fn new(source: &str, start: usize) -> Self {
        Self {
            tokens: Tokens::at(
                start,
                source
                    .get(..start)
                    .and_then(|text| text.chars().next_back()),
            ),
            processed: start,
            frontier: start,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(super) fn retained(&self) -> usize {
        self.tokens.retained()
            + self.atomic.capacity() * std::mem::size_of::<Range<usize>>()
            + self.state.retained()
            + (self.syntax.outer.capacity()
                + self.syntax.opaque.capacity()
                + self.syntax.links.capacity())
                * std::mem::size_of::<Range<usize>>()
    }

    pub(super) fn discard_before(&mut self, position: usize) {
        let before = self.tokens.ready.len();
        self.tokens.discard_before(position);
        self.fed = self.fed.saturating_sub(before - self.tokens.ready.len());
        self.atomic.clear();
    }

    pub(super) fn glyphs(&mut self, source: &str, start: usize, end: usize) -> Option<Spans> {
        let root = self.state.projection(self.tokens.preview().as_ref());
        self.state.forest.glyphs(root, source, start, end)
    }

    #[cfg(test)]
    pub(super) fn glyph_retained(
        &self,
        nodes: &mut HashSet<usize>,
        texts: &mut HashSet<usize>,
        links: &mut HashSet<*const u8>,
    ) -> (usize, usize) {
        self.state.forest.glyph_retained(nodes, texts, links)
    }

    pub(super) fn checkpoint(&self, position: usize) -> usize {
        let (mut low, mut high) = (0, self.atomic.len());
        while low < high {
            #[cfg(test)]
            work::scan(1);
            let middle = low + (high - low) / 2;
            if self
                .atomic
                .get(middle)
                .is_some_and(|range| range.end <= position)
            {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        self.atomic.get(low).map_or(position, |range| {
            if range.start < position {
                range.start
            } else {
                position
            }
        })
    }

    pub(super) fn frontier(
        &mut self,
        original: &str,
        base: usize,
        source: &str,
        events: &[(Event<'_>, Range<usize>)],
        sentinel: usize,
    ) -> usize {
        self.tokens.push(original);
        let mut syntax = Syntax::new(source, events);
        syntax.absolute(base, sentinel);
        let pending = self.tokens.preview().map(|token| token.range.start);
        // Parser-resolved scopes can override an earlier tentative bracket or
        // HTML opener. Retain lexical state only while those classifications
        // agree over the bytes already consumed; newly appended scopes need
        // no replay. Coordinates stay in the original append-only source.
        let replay = !syntax.agrees_before(&self.syntax, base, self.processed);
        #[cfg(test)]
        let replay = replay || self.replay;
        if replay {
            self.state.reset();
            self.processed = base;
            self.fed = 0;
            self.frontier = base;
        }
        let (mut opaque, mut link) = (0, 0);
        let state = &mut self.state;
        for token in self.tokens.ready.range(self.fed..) {
            if token.range.end <= self.processed {
                continue;
            }
            #[cfg(test)]
            work::scan(1);
            let start = token.range.start.max(base).max(self.processed);
            let end = token.range.end;
            #[cfg(test)]
            {
                self.fed_bytes += end - start;
            }
            if inside(&syntax.opaque, &mut opaque, start) {
                state.opaque_text(start..end);
            } else {
                let linked = inside(&syntax.links, &mut link, start);
                state.feed(start..end, token.kind, linked);
            }
            if state.neutral()
                && (self.tokens.neutral() || pending.is_some_and(|start| start >= end))
            {
                self.frontier = end;
            }
            self.processed = end;
        }
        // The lexer may extend its final plain-text token on the next push.
        self.fed = self.tokens.ready.len().saturating_sub(usize::from(
            self.tokens
                .ready
                .back()
                .is_some_and(|token| token.kind == Kind::Text),
        ));
        if state.invalid {
            return sentinel;
        }
        let candidate = self
            .frontier
            .max(base)
            .max(state.earliest(pending.unwrap_or(original.len())));
        let scopes = if let Some(pending) = self.tokens.preview() {
            let root = state.projection(Some(&pending));
            state.forest.scopes_since(root, base)
        } else {
            state.forest.scopes_since(state.forest.snapshot(), base)
        };
        self.forbid_splits(&syntax.outer, scopes);
        self.syntax = syntax;
        self.checkpoint(candidate).saturating_sub(base) + sentinel
    }

    fn forbid_splits(&mut self, ranges: &[Range<usize>], scopes: Vec<Range<usize>>) {
        self.atomic.clear();
        let mut syntax = ranges.iter().cloned().peekable();
        let mut scopes = scopes.into_iter().peekable();
        let mut tokens = self
            .tokens
            .ready
            .iter()
            .filter(|token| token.kind != Kind::Text)
            .map(|token| token.range.clone())
            .chain(self.tokens.preview().map(|token| token.range))
            .peekable();
        loop {
            let first = syntax
                .peek()
                .into_iter()
                .chain(scopes.peek())
                .chain(tokens.peek())
                .map(|range| range.start)
                .min();
            let Some(first) = first else {
                break;
            };
            let next = if syntax.peek().is_some_and(|range| range.start == first) {
                syntax.next()
            } else if scopes.peek().is_some_and(|range| range.start == first) {
                scopes.next()
            } else {
                tokens.next()
            };
            let Some(next) = next else {
                break;
            };
            #[cfg(test)]
            work::scan(1);
            if let Some(last) = self.atomic.last_mut()
                && next.start < last.end
            {
                last.end = last.end.max(next.end);
            } else {
                #[cfg(test)]
                work::copy(
                    std::mem::size_of::<Range<usize>>()
                        * (1 + if self.atomic.len() == self.atomic.capacity() {
                            self.atomic.len()
                        } else {
                            0
                        }),
                );
                self.atomic.push(next);
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/inline_scan.rs"]
mod tests;
