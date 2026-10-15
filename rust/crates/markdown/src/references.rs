use pulldown_cmark::{BrokenLink, Event, LinkType, Parser, Tag, TagEnd};
use std::collections::{HashMap, HashSet};

mod chain;

pub(crate) type Key = unicase::UniCase<String>;
type Definition = (String, String, usize);

// All callers receive labels already normalized by pulldown-cmark. In particular,
// its ASCII whitespace rules must not be applied again as Unicode whitespace.
pub(crate) fn normalize(label: &str) -> Key {
    unicase::UniCase::new(label.to_owned())
}

#[derive(Default)]
pub(crate) struct References {
    committed: HashMap<Key, Definition>,
    pending: HashMap<Key, Definition>,
    #[cfg(test)]
    pub(crate) work: usize,
}

impl References {
    pub(crate) fn frontier(&self, offset: usize) -> usize {
        self.pending
            .values()
            .map(|definition| definition.2)
            .max()
            .unwrap_or(offset)
    }

    pub(crate) fn get(&self, key: &Key) -> Option<&Definition> {
        self.committed.get(key).or_else(|| self.pending.get(key))
    }

    pub(crate) fn update(&mut self, source: &str, offset: usize, prefix: usize) -> HashSet<Key> {
        #[cfg(test)]
        PARSED_BYTES.with(|work| work.set(work.get() + source.len()));
        let parser = Parser::new_ext(source, crate::options());
        let pending: HashMap<_, _> = parser
            .reference_definitions()
            .iter()
            .map(|(key, value)| {
                (
                    normalize(key),
                    (
                        value.dest.to_string(),
                        value
                            .title
                            .as_ref()
                            .map_or_else(String::new, ToString::to_string),
                        offset + value.span.start.saturating_sub(prefix),
                    ),
                )
            })
            .collect();
        #[cfg(test)]
        {
            self.work += self.pending.len() + pending.len();
        }
        let mut changed = HashSet::new();
        for key in self.pending.keys().chain(pending.keys()) {
            if !self.committed.contains_key(key) && self.pending.get(key) != pending.get(key) {
                changed.insert(key.clone());
            }
        }
        self.pending = pending;
        changed
    }

    pub(crate) fn commit(&mut self, offset: usize) {
        #[cfg(test)]
        {
            self.work += self.pending.len();
        }
        self.pending.retain(|key, value| {
            if value.2 < offset {
                self.committed
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
                false
            } else {
                true
            }
        });
    }
}

// Cache alternatives as a graph. A failed reference can expose its following
// reference; that suffix is shared by both branches instead of copied recursively.
type Shared = std::sync::Arc<[Node]>;

pub(crate) struct Template {
    nodes: Shared,
    pub(crate) keys: HashSet<Key>,
}

enum Node {
    Event(Event<'static>),
    Sequence(Shared),
    Choice {
        key: Key,
        present: Shared,
        missing: Shared,
    },
    Reference {
        key: Key,
        start: Event<'static>,
        end: Event<'static>,
        content: Shared,
        following: Shared,
        missing: Shared,
    },
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Blocked {
    key: Key,
    start: usize,
    end: usize,
}

#[derive(PartialEq, Eq, Hash)]
struct MemoKey {
    start: usize,
    end: usize,
    context: String,
    blocked: Vec<Blocked>,
}

#[derive(Default)]
struct Compiler {
    keys: HashSet<Key>,
    memo: HashMap<MemoKey, Shared>,
    #[cfg(test)]
    nodes: usize,
}

impl Template {
    pub(crate) fn new(
        source: &str,
        quotes: usize,
        events: &[(Event<'_>, std::ops::Range<usize>)],
    ) -> Self {
        let paragraph = matches!(events.first(), Some((Event::Start(Tag::Paragraph), _)));
        let task = events
            .iter()
            .any(|(event, _)| matches!(event, Event::TaskListMarker(_)));
        let mut normalized = String::new();
        if task {
            normalized.push_str("- ");
        }
        for (index, mut line) in source.split_inclusive('\n').enumerate() {
            if index > 0 {
                for _ in 0..quotes {
                    let trimmed = line.trim_start_matches([' ', '\t']);
                    if let Some(rest) = trimmed.strip_prefix('>') {
                        line = rest.strip_prefix([' ', '\t']).unwrap_or(rest);
                    } else {
                        break;
                    }
                }
            }
            normalized.push_str(line);
        }
        let mut compiler = Compiler::default();
        let nodes = compiler.parse(&normalized, 0, &[], (false, paragraph, task));
        #[cfg(test)]
        COMPILED_NODES.with(|count| count.set(count.get() + compiler.nodes));
        Self {
            nodes,
            keys: compiler.keys,
        }
    }

    pub(crate) fn events(&self, references: &References) -> Vec<Event<'static>> {
        let mut events = Vec::new();
        emit(&self.nodes, references, &mut events);
        events
    }
}

#[cfg(test)]
std::thread_local! {
    pub(crate) static COMPILED_NODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static PARSED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Compiler {
    fn parse(
        &mut self,
        source: &str,
        offset: usize,
        blocked: &[Blocked],
        (inline, paragraph, task): (bool, bool, bool),
    ) -> Shared {
        let blocked = relevant(blocked, offset, offset + source.len());
        let key = MemoKey {
            start: offset,
            end: offset + source.len(),
            context: format!("parse:{inline}:{paragraph}:{task}"),
            blocked: blocked.clone(),
        };
        if let Some(nodes) = self.memo.get(&key) {
            return nodes.clone();
        }
        let padded = (inline && source.ends_with([' ', '\t'])).then(|| format!("{source}x"));
        let source = padded.as_deref().unwrap_or(source);
        #[cfg(test)]
        PARSED_BYTES.with(|work| work.set(work.get() + source.len()));
        let mut callback = |link: BrokenLink<'_>| {
            let label = normalize(&link.reference);
            (!blocked.iter().any(|entry| {
                entry.key == label
                    && entry.start <= offset + link.span.start
                    && entry.end >= offset + link.span.end
            }))
            .then(|| ("".into(), "".into()))
        };
        let mut options = crate::options();
        if paragraph {
            options.remove(pulldown_cmark::Options::ENABLE_TABLES);
        }
        let mut events: Vec<_> =
            Parser::new_with_broken_link_callback(source, options, Some(&mut callback))
                .into_offset_iter()
                .map(|(event, range)| (event.into_static(), range))
                .collect();
        if task {
            events = crate::events::paragraphs(source, &events);
            events.drain(..2);
            events.truncate(events.len().saturating_sub(2));
        }
        if padded.is_some()
            && let Some((Event::Text(text), range)) = events
                .iter_mut()
                .rev()
                .find(|(event, _)| !matches!(event, Event::End(_)))
        {
            let mut content = text.to_string();
            content.pop();
            *text = content.into();
            range.end = range.end.saturating_sub(1);
        }
        if inline && matches!(events.first(), Some((Event::Start(Tag::Paragraph), _))) {
            events.remove(0);
            if matches!(events.last(), Some((Event::End(TagEnd::Paragraph), _))) {
                events.pop();
            }
        }
        // Resolving an inner link disables enclosing link brackets in CommonMark.
        // Its missing alternative therefore needs the enclosing source context,
        // rather than just the reference's own span.
        let enclosing = enclosing(source, &events);
        let nested = nested(&events, &enclosing);
        let present = self.nodes(source, offset, &events, &blocked, &enclosing);
        let nodes = if let Some((label, range)) = nested {
            let mut denied = blocked.clone();
            denied.push(Blocked {
                key: label.clone(),
                start: offset + range.start,
                end: offset + range.end,
            });
            let missing = self.parse(
                source.get(..key.end - offset).unwrap_or_default(),
                offset,
                &denied,
                (inline, paragraph, task),
            );
            self.shared(vec![Node::Choice {
                key: label,
                present,
                missing,
            }])
        } else {
            present
        };
        self.memo.insert(key, nodes.clone());
        nodes
    }

    fn nodes(
        &mut self,
        source: &str,
        offset: usize,
        events: &[(Event<'static>, std::ops::Range<usize>)],
        blocked: &[Blocked],
        enclosing: &[Option<usize>],
    ) -> Shared {
        let (Some((first, begin)), Some((last, end))) = (events.first(), events.last()) else {
            return Shared::from([]);
        };
        let blocked = relevant(blocked, offset + begin.start, offset + end.end);
        let memo = MemoKey {
            start: offset + begin.start,
            end: offset + end.end,
            context: format!(
                "nodes:{}:{}:{}",
                event_kind(first),
                event_kind(last),
                events.len()
            ),
            blocked: blocked.clone(),
        };
        if let Some(nodes) = self.memo.get(&memo) {
            return nodes.clone();
        }
        let mut result = Vec::new();
        let mut index = 0;
        while let Some((event, _)) = events.get(index) {
            if let Some((node, after)) =
                self.reference(source, offset, events, index, &blocked, enclosing)
            {
                result.push(node);
                index = after;
            } else {
                result.push(Node::Event(event.clone()));
                index += 1;
            }
        }
        let result = self.shared(result);
        self.memo.insert(memo, result.clone());
        result
    }

    fn shared(&mut self, nodes: Vec<Node>) -> Shared {
        #[cfg(test)]
        {
            self.nodes += nodes.len();
        }
        Shared::from(nodes)
    }

    fn reference(
        &mut self,
        source: &str,
        offset: usize,
        events: &[(Event<'static>, std::ops::Range<usize>)],
        index: usize,
        blocked: &[Blocked],
        enclosing: &[Option<usize>],
    ) -> Option<(Node, usize)> {
        let (event, range) = events.get(index)?;
        let key = match event {
            Event::Start(Tag::Link { link_type, id, .. } | Tag::Image { link_type, id, .. })
                if !matches!(
                    link_type,
                    LinkType::Inline | LinkType::Autolink | LinkType::Email
                ) =>
            {
                normalize(id)
            }
            _ => return None,
        };
        let end = matching_end(events, index + 1);
        let target = recovery_end(source, range.end);
        let (after, recovery) = recovery_range(events, end + 1, target, range.end);
        if target == recovery
            && enclosing.get(range.start).copied().flatten().is_none()
            && let Some(nodes) = chain::compile(
                self,
                source.get(range.start..target)?,
                events.get(index..after)?,
                range.start,
            )
        {
            return Some((Node::Sequence(nodes), after));
        }
        self.keys.insert(key.clone());
        let content = self.nodes(
            source,
            offset,
            events.get(index + 1..end).unwrap_or_default(),
            blocked,
            enclosing,
        );
        let following = self.nodes(
            source,
            offset,
            events.get(end + 1..after).unwrap_or_default(),
            blocked,
            enclosing,
        );
        let mut denied = blocked.to_vec();
        denied.push(Blocked {
            key: key.clone(),
            start: offset + range.start,
            end: offset + range.end,
        });
        let missing = self.parse(
            source.get(range.start..recovery).unwrap_or_default(),
            offset + range.start,
            &denied,
            (true, true, false),
        );
        let closing = events
            .get(end)
            .map_or(Event::End(TagEnd::Link), |(event, _)| event.clone());
        let node = Node::Reference {
            key,
            start: event.clone(),
            end: closing,
            content,
            following,
            missing,
        };
        Some((node, after))
    }
}

fn nested(
    events: &[(Event<'_>, std::ops::Range<usize>)],
    enclosing: &[Option<usize>],
) -> Option<(Key, std::ops::Range<usize>)> {
    events.iter().find_map(|(event, range)| {
        let Event::Start(Tag::Link { link_type, id, .. }) = event else {
            return None;
        };
        if matches!(
            link_type,
            LinkType::Inline | LinkType::Autolink | LinkType::Email
        ) {
            return None;
        }
        enclosing
            .get(range.start)
            .copied()
            .flatten()
            .is_some()
            .then(|| (normalize(id), range.clone()))
    })
}

fn relevant(blocked: &[Blocked], start: usize, end: usize) -> Vec<Blocked> {
    blocked
        .iter()
        .filter(|entry| entry.start < end && entry.end > start)
        .cloned()
        .collect()
}

fn opaque(events: &[(Event<'_>, std::ops::Range<usize>)]) -> Vec<std::ops::Range<usize>> {
    events
        .iter()
        .filter_map(|(event, range)| {
            matches!(
                event,
                Event::Code(_)
                    | Event::InlineHtml(_)
                    | Event::Start(Tag::Link {
                        link_type: LinkType::Autolink | LinkType::Email,
                        ..
                    })
            )
            .then(|| range.clone())
        })
        .collect()
}

fn enclosing(source: &str, events: &[(Event<'_>, std::ops::Range<usize>)]) -> Vec<Option<usize>> {
    let mut openings = Vec::new();
    let mut result = Vec::with_capacity(source.len() + 1);
    let opaque = opaque(events);
    let mut span = 0;
    let mut escaped = false;
    for (index, byte) in source.bytes().enumerate() {
        result.push(openings.last().copied());
        while opaque.get(span).is_some_and(|range| range.end <= index) {
            span += 1;
        }
        if opaque.get(span).is_some_and(|range| range.start <= index) {
            escaped = false;
            continue;
        }
        if escaped {
            escaped = false;
        } else {
            match byte {
                b'\\' => escaped = true,
                b'[' => openings.push(index),
                b']' => {
                    openings.pop();
                }
                _ => {}
            }
        }
    }
    result.push(openings.last().copied());
    result
}

fn event_kind(event: &Event<'_>) -> String {
    match event {
        Event::Start(tag) => format!("start:{:?}", tag.to_end()),
        Event::End(tag) => format!("end:{tag:?}"),
        _ => format!("{:?}", std::mem::discriminant(event)),
    }
}

fn matching_end(events: &[(Event<'static>, std::ops::Range<usize>)], mut end: usize) -> usize {
    let mut depth = 1usize;
    while let Some((event, _)) = events.get(end) {
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            break;
        }
        end += 1;
    }
    end
}

fn recovery_range(
    events: &[(Event<'static>, std::ops::Range<usize>)],
    mut after: usize,
    target: usize,
    mut recovery: usize,
) -> (usize, usize) {
    let mut depth = 0usize;
    while let Some((event, following)) = events.get(after) {
        if depth == 0 && (following.start >= target || matches!(event, Event::End(_))) {
            break;
        }
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
        recovery = recovery.max(following.end);
        after += 1;
    }
    (after, recovery)
}

fn recovery_end(source: &str, mut end: usize) -> usize {
    while let Some(opening @ (b'[' | b'(')) = source.as_bytes().get(end).copied() {
        let closing = if opening == b'[' { b']' } else { b')' };
        let mut depth = 0usize;
        let mut escaped = false;
        let mut next = None;
        for (index, byte) in source.bytes().enumerate().skip(end) {
            if escaped {
                escaped = false;
                continue;
            }
            if byte == b'\\' {
                escaped = true;
                continue;
            }
            if byte == opening {
                depth += 1;
            }
            if byte == closing {
                depth -= 1;
            }
            if depth == 0 {
                next = Some(index + 1);
                break;
            }
        }
        let Some(next) = next else {
            break;
        };
        end = next;
    }
    end
}

enum Emit<'a> {
    Node(&'a Node),
    Event(&'a Event<'static>),
}

fn emit(nodes: &[Node], references: &References, events: &mut Vec<Event<'static>>) {
    let mut pending: Vec<_> = nodes.iter().rev().map(Emit::Node).collect();
    while let Some(next) = pending.pop() {
        let node = match next {
            Emit::Event(event) => {
                events.push(event.clone());
                continue;
            }
            Emit::Node(node) => node,
        };
        match node {
            Node::Event(event) => events.push(event.clone()),
            Node::Sequence(nodes) => pending.extend(nodes.iter().rev().map(Emit::Node)),
            Node::Choice {
                key,
                present,
                missing,
            } => {
                let selected = if references.get(key).is_some() {
                    present
                } else {
                    missing
                };
                pending.extend(selected.iter().rev().map(Emit::Node));
            }
            Node::Reference {
                key,
                start,
                end,
                content,
                following,
                missing,
            } => {
                if let Some((destination, title, _)) = references.get(key) {
                    let mut start = start.clone();
                    if let Event::Start(
                        Tag::Link {
                            dest_url,
                            title: label,
                            ..
                        }
                        | Tag::Image {
                            dest_url,
                            title: label,
                            ..
                        },
                    ) = &mut start
                    {
                        *dest_url = destination.clone().into();
                        *label = title.clone().into();
                    }
                    events.push(start);
                    pending.extend(following.iter().rev().map(Emit::Node));
                    pending.push(Emit::Event(end));
                    pending.extend(content.iter().rev().map(Emit::Node));
                } else {
                    pending.extend(missing.iter().rev().map(Emit::Node));
                }
            }
        }
    }
}
