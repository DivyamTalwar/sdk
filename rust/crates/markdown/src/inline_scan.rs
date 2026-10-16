use super::*;

#[derive(Default)]
struct State {
    escape: Option<usize>,
    delimiter: Option<usize>,
    bracket: Vec<usize>,
    labels: Vec<Range<usize>>,
    entity: Option<usize>,
    opaque: Option<usize>,
    run: Option<usize>,
    bang: Option<usize>,
}

impl State {
    fn neutral(&self) -> bool {
        self.escape.is_none()
            && self.delimiter.is_none()
            && self.bracket.is_empty()
            && self.labels.is_empty()
            && self.entity.is_none()
            && self.opaque.is_none()
            && self.run.is_none()
            && self.bang.is_none()
    }

    fn label(&mut self, range: Range<usize>) {
        self.labels.push(range);
        if self.labels.len() > 2 {
            self.labels.remove(0);
        }
    }

    fn earliest(&self, end: usize) -> usize {
        [
            self.escape,
            self.delimiter,
            self.bracket.first().copied(),
            self.labels.first().map(|label| label.start),
            self.entity,
            self.opaque,
            self.run,
            self.bang,
        ]
        .into_iter()
        .flatten()
        .fold(end, usize::min)
    }
}

// Parser scopes are already closed syntax, including any nested inline syntax.
// The scanner still owns their right edge: a delimiter run or reference label
// at the input edge is unresolved until its following character is known.
fn atoms(source: &str, events: &[(Event<'_>, Range<usize>)]) -> Vec<Range<usize>> {
    let mut atoms = Vec::new();
    let mut depth = 0usize;
    for (event, range) in events {
        match event {
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {}
            Event::Start(_) => {
                if depth == 0 {
                    atoms.push(range.clone());
                }
                depth += 1;
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Text(text) if depth == 0 && source.get(range.clone()) != Some(text.as_ref()) => {
                atoms.push(range.clone());
            }
            Event::Text(_) | Event::SoftBreak | Event::HardBreak => {}
            _ if depth == 0 => atoms.push(range.clone()),
            _ => {}
        }
    }
    atoms
}

pub(super) fn frontier(
    source: &str,
    events: &[(Event<'_>, Range<usize>)],
    sentinel: usize,
) -> usize {
    let atoms = atoms(source, events);
    let mut scopes = atoms.iter().peekable();
    let mut state = State::default();
    let mut cursor = sentinel;
    let mut frontier = sentinel;
    while cursor < source.len() {
        while scopes.peek().is_some_and(|atom| atom.end <= cursor) {
            scopes.next();
        }
        if let Some(atom) = scopes.peek().filter(|atom| atom.start == cursor) {
            let end = atom.end;
            edge(source, atom, &mut state);
            cursor = end;
            scopes.next();
        } else {
            cursor = literal(source, cursor, &mut state);
        }
        if state.neutral() {
            frontier = cursor;
        }
    }
    let limit = state.earliest(source.len());
    // A pending label can become irrelevant once two later labels follow it.
    // Choose a source position outside every remaining scanner scope.
    let mut candidate = frontier.max(limit);
    for atom in &atoms {
        if atom.start < candidate && candidate < atom.end {
            candidate = atom.start;
        }
    }
    candidate.max(sentinel)
}

fn edge(source: &str, atom: &Range<usize>, state: &mut State) {
    state.entity = None;
    state.bang = None;
    let before = source.as_bytes().get(atom.end.saturating_sub(1)).copied();
    let after = source.as_bytes().get(atom.end).copied();
    let unsettled = after.is_none()
        || before == after
            && source
                .get(atom.end..)
                .is_some_and(|tail| tail.bytes().all(|byte| Some(byte) == before));
    if before.is_some_and(|byte| matches!(byte, b'*' | b'_' | b'~' | b'`')) && unsettled {
        state.run = Some(atom.start);
    } else if before == Some(b']') {
        state.label(atom.clone());
    } else {
        state.labels.clear();
    }
}

fn literal(source: &str, position: usize, state: &mut State) -> usize {
    let Some(character) = source.get(position..).and_then(|tail| tail.chars().next()) else {
        return source.len();
    };
    #[cfg(test)]
    work::scan(character.len_utf8());
    let end = position + character.len_utf8();
    if state.escape.take().is_some() {
        return end;
    }
    if character != '[' {
        state.bang = None;
    }
    if state.entity.is_some() && !(character.is_ascii_alphanumeric() || character == '#') {
        state.entity = None;
    }
    if !matches!(character, '[' | ']' | '(') && !character.is_whitespace() {
        state.labels.clear();
    }
    match character {
        '\\' => state.escape = Some(position),
        '[' => {
            let start = state.bang.take().unwrap_or(position);
            state.bracket.push(start);
        }
        ']' => {
            if let Some(start) = state.bracket.pop()
                && state.bracket.is_empty()
            {
                state.label(start..end);
            }
        }
        '(' if !state.labels.is_empty() => {
            if let Some(label) = state.labels.first() {
                state.opaque.get_or_insert(label.start);
            }
        }
        '&' => state.entity = Some(position),
        '`' | '<' => {
            state.opaque.get_or_insert(position);
        }
        '*' | '_' | '~' => {
            let run = source
                .get(position..)
                .unwrap_or_default()
                .chars()
                .take_while(|next| *next == character)
                .count();
            let end = position + run;
            #[cfg(test)]
            work::scan(run);
            let after = source.get(end..).and_then(|tail| tail.chars().next());
            if after.is_none() {
                state.run.get_or_insert(position);
            }
            if can_open(source, position, end, character) {
                state.delimiter.get_or_insert(position);
            }
            return end;
        }
        '!' => state.bang = Some(position),
        _ => {}
    }
    end
}

fn can_open(source: &str, start: usize, end: usize, delimiter: char) -> bool {
    let before = source
        .get(..start)
        .and_then(|prefix| prefix.chars().next_back())
        .unwrap_or(' ');
    let after = source
        .get(end..)
        .and_then(|suffix| suffix.chars().next())
        .unwrap_or(' ');
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
    left && (delimiter != '_' || !right || before.is_ascii_punctuation())
}
