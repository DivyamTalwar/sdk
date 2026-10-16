use super::*;
#[path = "inline_delimiters.rs"]
mod delimiters;
#[path = "inline_forest.rs"]
mod forest;
#[path = "inline_tokens.rs"]
mod tokens;
use delimiters::{Delimiters, Run};
use tokens::{Kind, Tokens};

#[derive(Default)]
pub(super) struct Scanner {
    tokens: Tokens,
    atomic: Vec<Range<usize>>,
}

#[derive(Default)]
struct State {
    delimiter: Delimiters,
    forest: forest::Forest,
    invalid: bool,
    bracket: Vec<usize>,
    labels: Vec<Range<usize>>,
    opaque: Option<usize>,
}

impl State {
    fn neutral(&self) -> bool {
        self.delimiter.neutral()
            && self.bracket.is_empty()
            && self.labels.is_empty()
            && self.opaque.is_none()
    }

    fn label(&mut self, range: Range<usize>) {
        #[cfg(test)]
        work::copy(
            std::mem::size_of::<Range<usize>>()
                * (1 + if self.labels.len() == self.labels.capacity() {
                    self.labels.len()
                } else {
                    0
                }),
        );
        self.labels.push(range);
        if self.labels.len() > 2 {
            #[cfg(test)]
            work::copy(2 * std::mem::size_of::<Range<usize>>());
            self.labels.remove(0);
        }
    }

    fn earliest(&self, end: usize) -> usize {
        [
            self.delimiter.first(),
            self.bracket.first().copied(),
            self.labels.first().map(|label| label.start),
            self.opaque,
        ]
        .into_iter()
        .flatten()
        .fold(end, usize::min)
    }
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
                Event::Code(_) | Event::InlineHtml(_) => {
                    push_range(&mut syntax.opaque, range.clone());
                    push_range(&mut syntax.outer, range.clone());
                }
                _ => {}
            }
        }
        syntax
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
            atomic: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn retained(&self) -> usize {
        self.tokens.retained() + self.atomic.capacity() * std::mem::size_of::<Range<usize>>()
    }

    pub(super) fn discard_before(&mut self, position: usize) {
        self.tokens.discard_before(position);
        self.atomic.clear();
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
        let syntax = Syntax::new(source, events);
        let pending = self.tokens.preview().map(|token| token.range.start);
        let pending = pending.map(|start| start.saturating_sub(base) + sentinel);
        let (mut opaque, mut link) = (0, 0);
        let mut state = State::default();
        let mut frontier = sentinel;
        for token in &self.tokens.ready {
            #[cfg(test)]
            work::scan(1);
            let start = token.range.start.max(base).saturating_sub(base) + sentinel;
            let end = token.range.end.saturating_sub(base) + sentinel;
            let cursor = state.forest.text(start..end);
            if inside(&syntax.opaque, &mut opaque, start) {
                state.labels.clear();
            } else {
                let linked = inside(&syntax.links, &mut link, start);
                literal(start..end, token.kind, &mut state, linked, cursor);
            }
            if state.neutral()
                && (self.tokens.neutral() || pending.is_some_and(|start| start >= end))
            {
                frontier = end;
            }
        }
        if state.invalid {
            return sentinel;
        }
        let candidate = frontier.max(state.earliest(pending.unwrap_or(source.len())));
        self.forbid_splits(syntax.outer, state.forest.scopes(), base, sentinel);
        self.checkpoint(base + candidate.saturating_sub(sentinel))
            .saturating_sub(base)
            + sentinel
    }

    fn forbid_splits(
        &mut self,
        ranges: Vec<Range<usize>>,
        scopes: Vec<Range<usize>>,
        base: usize,
        sentinel: usize,
    ) {
        self.atomic.clear();
        let mut syntax = ranges
            .into_iter()
            .map(|range| {
                base + range.start.saturating_sub(sentinel)
                    ..base + range.end.saturating_sub(sentinel)
            })
            .peekable();
        let mut scopes = scopes
            .into_iter()
            .map(|range| {
                base + range.start.saturating_sub(sentinel)
                    ..base + range.end.saturating_sub(sentinel)
            })
            .peekable();
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

fn literal(range: Range<usize>, kind: Kind, state: &mut State, linked: bool, cursor: usize) {
    if !matches!(kind, Kind::Open { .. } | Kind::Close | Kind::ParenOpen) {
        state.labels.clear();
    }
    match kind {
        Kind::Open { .. } if !linked => {
            #[cfg(test)]
            work::copy(
                std::mem::size_of::<usize>()
                    * (1 + if state.bracket.len() == state.bracket.capacity() {
                        state.bracket.len()
                    } else {
                        0
                    }),
            );
            state.bracket.push(range.start);
        }
        Kind::Close if !linked => {
            if let Some(start) = state.bracket.pop()
                && state.bracket.is_empty()
            {
                state.label(start..range.end);
            }
        }
        Kind::ParenOpen if !state.labels.is_empty() => {
            if let Some(label) = state.labels.first() {
                state.opaque.get_or_insert(label.start);
            }
        }
        Kind::AngleOpen | Kind::Run { byte: b'`', .. } => {
            state.opaque.get_or_insert(range.start);
        }
        Kind::Run {
            byte,
            before,
            after,
        } if !linked => {
            let mut run = Run::new(range, byte, before, after);
            run.cursor = cursor;
            state.delimiter.feed(run, |matched| {
                let wrapped = state.forest.wrap(matched);
                debug_assert!(
                    wrapped.is_some(),
                    "delimiter cursors must belong to the active forest"
                );
                if wrapped.is_none() {
                    state.invalid = true;
                }
                wrapped.unwrap_or_default()
            });
        }
        _ => {}
    }
}
