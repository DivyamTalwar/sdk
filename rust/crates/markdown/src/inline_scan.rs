use super::*;
#[path = "inline_tokens.rs"]
mod tokens;
use tokens::{Kind, Tokens};

#[derive(Default)]
pub(super) struct Scanner {
    tokens: Tokens,
}

#[derive(Default)]
struct State {
    delimiter: Option<usize>,
    bracket: Vec<usize>,
    labels: Vec<Range<usize>>,
    opaque: Option<usize>,
}

impl State {
    fn neutral(&self) -> bool {
        self.delimiter.is_none()
            && self.bracket.is_empty()
            && self.labels.is_empty()
            && self.opaque.is_none()
    }

    fn label(&mut self, range: Range<usize>) {
        self.labels.push(range);
        if self.labels.len() > 2 {
            self.labels.remove(0);
        }
    }

    fn earliest(&self, end: usize) -> usize {
        [
            self.delimiter,
            self.bracket.first().copied(),
            self.labels.first().map(|label| label.start),
            self.opaque,
        ]
        .into_iter()
        .flatten()
        .fold(end, usize::min)
    }
}

// Closed parser subtrees are atomic. The retained lexer owns their right edge:
// EOF never settles an escape, an entity or a delimiter run.
fn atoms(events: &[(Event<'_>, Range<usize>)]) -> Vec<Range<usize>> {
    let mut atoms = Vec::new();
    let mut depth = 0usize;
    for (event, range) in events {
        #[cfg(test)]
        work::scan(1);
        match event {
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {}
            Event::Start(_) => {
                if depth == 0 {
                    atoms.push(range.clone());
                }
                depth += 1;
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Text(_) | Event::SoftBreak | Event::HardBreak => {}
            _ if depth == 0 => atoms.push(range.clone()),
            _ => {}
        }
    }
    #[cfg(test)]
    work::copy(atoms.len() * std::mem::size_of::<Range<usize>>() * 2);
    atoms
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
        }
    }

    #[cfg(test)]
    pub(super) fn retained(&self) -> usize {
        self.tokens.retained()
    }

    pub(super) fn discard_before(&mut self, position: usize) {
        self.tokens.discard_before(position);
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
        let atoms = atoms(events);
        let pending = self.tokens.preview().map(|token| token.range.start);
        let pending = pending.map(|start| start.saturating_sub(base) + sentinel);
        let mut scopes = atoms.iter().peekable();
        let mut state = State::default();
        let mut cursor = sentinel;
        let mut frontier = sentinel;
        for token in &self.tokens.ready {
            #[cfg(test)]
            work::scan(1);
            let start = token.range.start.max(base).saturating_sub(base) + sentinel;
            let end = token.range.end.saturating_sub(base) + sentinel;
            if end <= cursor {
                continue;
            }
            while scopes.peek().is_some_and(|atom| atom.end <= cursor) {
                scopes.next();
            }
            if let Some(atom) = scopes.peek().filter(|atom| atom.start == start.max(cursor)) {
                edge(source, atom, pending, &mut state);
                cursor = atom.end;
                scopes.next();
            } else {
                literal(start.max(cursor)..end, token.kind, &mut state);
                cursor = end;
            }
            if state.neutral()
                && (self.tokens.neutral() || pending.is_some_and(|start| start >= cursor))
            {
                frontier = cursor;
            }
        }
        let limit = state.earliest(pending.unwrap_or(source.len()));
        let mut candidate = frontier.max(limit);
        for atom in &atoms {
            #[cfg(test)]
            work::scan(1);
            if atom.start < candidate && candidate < atom.end {
                candidate = atom.start;
            }
        }
        candidate.max(sentinel)
    }
}

fn edge(source: &str, atom: &Range<usize>, pending: Option<usize>, state: &mut State) {
    if pending.is_some_and(|start| start < atom.end) {
        state.opaque.get_or_insert(atom.start);
    } else if source.as_bytes().get(atom.end.saturating_sub(1)) == Some(&b']') {
        state.label(atom.clone());
    } else {
        state.labels.clear();
    }
}

fn literal(range: Range<usize>, kind: Kind, state: &mut State) {
    #[cfg(test)]
    work::scan(1);
    if !matches!(kind, Kind::Open { .. } | Kind::Close | Kind::ParenOpen) {
        state.labels.clear();
    }
    match kind {
        Kind::Open { .. } => state.bracket.push(range.start),
        Kind::Close => {
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
        } if can_open(before, after, byte) => {
            state.delimiter.get_or_insert(range.start);
        }
        _ => {}
    }
}

fn can_open(before: char, after: char, delimiter: u8) -> bool {
    if !before.is_ascii() || !after.is_ascii() {
        return true;
    }
    let left = !after.is_whitespace()
        && (!after.is_ascii_punctuation()
            || before.is_whitespace()
            || before.is_ascii_punctuation());
    let right = !before.is_whitespace()
        && (!before.is_ascii_punctuation()
            || after.is_whitespace()
            || after.is_ascii_punctuation());
    left && (delimiter != b'_' || !right || before.is_ascii_punctuation())
}
