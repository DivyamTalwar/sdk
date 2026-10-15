use pulldown_cmark::{BrokenLink, Event, LinkType, Parser, Tag, TagEnd};
use std::collections::{HashMap, HashSet};

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

// Keep both interpretations of a reference at the first parse. Later definitions
// select cached nodes; they never send a finalized block through the parser.
pub(crate) struct Template {
    nodes: Vec<Node>,
    pub(crate) keys: HashSet<Key>,
}

enum Node {
    Event(Event<'static>),
    Reference {
        key: Key,
        start: Event<'static>,
        end: Event<'static>,
        content: Vec<Node>,
        missing: Vec<Node>,
    },
}

impl Template {
    pub(crate) fn new(source: &str, quotes: usize) -> Self {
        let mut normalized = String::new();
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
        let mut keys = HashSet::new();
        let nodes = parse(&normalized, &HashSet::new(), &mut keys, false);
        Self { nodes, keys }
    }

    pub(crate) fn events(&self, references: &References) -> Vec<Event<'static>> {
        let mut events = Vec::new();
        emit(&self.nodes, references, &mut events);
        events
    }
}

fn parse(source: &str, blocked: &HashSet<Key>, keys: &mut HashSet<Key>, inline: bool) -> Vec<Node> {
    let mut callback = |link: BrokenLink<'_>| {
        (!blocked.contains(&normalize(&link.reference))).then(|| ("".into(), "".into()))
    };
    let events: Vec<_> =
        Parser::new_with_broken_link_callback(source, crate::options(), Some(&mut callback))
            .into_offset_iter()
            .map(|(event, range)| (event.into_static(), range))
            .collect();
    let mut index = 0;
    let mut nodes = nodes(source, &events, &mut index, blocked, keys);
    if inline
        && matches!(
            nodes.first(),
            Some(Node::Event(Event::Start(Tag::Paragraph)))
        )
    {
        nodes.remove(0);
        if matches!(
            nodes.last(),
            Some(Node::Event(Event::End(TagEnd::Paragraph)))
        ) {
            nodes.pop();
        }
    }
    nodes
}

fn nodes(
    source: &str,
    events: &[(Event<'static>, std::ops::Range<usize>)],
    index: &mut usize,
    blocked: &HashSet<Key>,
    keys: &mut HashSet<Key>,
) -> Vec<Node> {
    let mut result = Vec::new();
    while let Some((event, range)) = events.get(*index) {
        let reference = match event {
            Event::Start(Tag::Link { link_type, id, .. } | Tag::Image { link_type, id, .. })
                if !matches!(
                    link_type,
                    LinkType::Inline | LinkType::Autolink | LinkType::Email
                ) =>
            {
                Some(normalize(id))
            }
            _ => None,
        };
        *index += 1;
        if let Some(key) = reference {
            keys.insert(key.clone());
            let mut nesting = 1usize;
            let begin = *index;
            let mut end = begin;
            while let Some((event, _)) = events.get(end) {
                match event {
                    Event::Start(_) => nesting += 1,
                    Event::End(_) => nesting -= 1,
                    _ => {}
                }
                if nesting == 0 {
                    break;
                }
                end += 1;
            }
            let content = nodes(
                source,
                events.get(begin..end).unwrap_or_default(),
                &mut 0,
                blocked,
                keys,
            );
            let mut denied = blocked.clone();
            denied.insert(key.clone());
            let missing = parse(
                source.get(range.clone()).unwrap_or_default(),
                &denied,
                keys,
                true,
            );
            let closing = events
                .get(end)
                .map_or(Event::End(TagEnd::Link), |(event, _)| event.clone());
            result.push(Node::Reference {
                key,
                start: event.clone(),
                end: closing,
                content,
                missing,
            });
            *index = end + 1;
        } else {
            result.push(Node::Event(event.clone()));
        }
    }
    result
}

fn emit(nodes: &[Node], references: &References, events: &mut Vec<Event<'static>>) {
    for node in nodes {
        match node {
            Node::Event(event) => events.push(event.clone()),
            Node::Reference {
                key,
                start,
                end,
                content,
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
                    emit(content, references, events);
                    events.push(end.clone());
                } else {
                    emit(missing, references, events);
                }
            }
        }
    }
}
