use super::{Compiler, Key, Node, Shared, normalize};
use pulldown_cmark::{Event, LinkType, Parser, Tag, TagEnd};
use std::ops::Range;

type Events = Vec<(Event<'static>, Range<usize>)>;

struct Link {
    key: Key,
    start: Event<'static>,
    end: Event<'static>,
    content: Shared,
}

// Each bracket has two continuations: consuming just itself, or itself and the
// following label. Compile each local interpretation once and share suffixes.
pub(super) fn compile(
    compiler: &mut Compiler,
    source: &str,
    events: &[(Event<'_>, Range<usize>)],
    origin: usize,
) -> Option<Shared> {
    let opaque: Vec<_> = super::opaque(events)
        .into_iter()
        .map(|range| range.start.saturating_sub(origin)..range.end.saturating_sub(origin))
        .collect();
    let brackets = brackets(source, &opaque)?;
    if brackets.len() < 3 {
        return None;
    }
    let missing = parse(source, false);
    if !independent(&missing, &brackets) {
        return None;
    }
    let mut suffixes = vec![Shared::from([]); brackets.len() + 2];
    for (index, bracket) in brackets.iter().enumerate().rev() {
        let raw = source.get(bracket.clone())?;
        let following = suffixes.get(index + 1)?.clone();
        let mut literal: Vec<_> = parse(raw, false)
            .into_iter()
            .map(|(event, _)| Node::Event(event))
            .collect();
        literal.push(Node::Sequence(following.clone()));
        let literal = compiler.shared(literal);
        let pair = if let Some(next) = brackets.get(index + 1) {
            let pair = source.get(bracket.start..next.end)?;
            link(compiler, &parse(pair, true), pair.len())
        } else {
            None
        };
        let node = if let Some(link) = pair {
            // An explicit but missing label suppresses shortcut fallback for
            // this bracket. The following bracket remains available to pair.
            reference(compiler, link, suffixes.get(index + 2)?.clone(), literal)
        } else {
            match link(compiler, &parse(raw, true), raw.len()) {
                Some(link) => reference(compiler, link, following, literal),
                None => literal,
            }
        };
        *suffixes.get_mut(index)? = node;
    }
    suffixes.first().cloned()
}

fn reference(compiler: &mut Compiler, link: Link, following: Shared, missing: Shared) -> Shared {
    compiler.keys.insert(link.key.clone());
    compiler.shared(vec![Node::Reference {
        key: link.key,
        start: link.start,
        end: link.end,
        content: link.content,
        following,
        missing,
    }])
}

fn link(compiler: &mut Compiler, events: &Events, length: usize) -> Option<Link> {
    let (start, range) = events.first()?;
    let key = match start {
        Event::Start(Tag::Link { link_type, id, .. } | Tag::Image { link_type, id, .. })
            if !matches!(
                link_type,
                LinkType::Inline | LinkType::Autolink | LinkType::Email
            ) && range.start == 0
                && range.end == length =>
        {
            normalize(id)
        }
        _ => return None,
    };
    let end = events.last()?.0.clone();
    let content = compiler.shared(
        events
            .get(1..events.len().saturating_sub(1))?
            .iter()
            .map(|(event, _)| Node::Event(event.clone()))
            .collect(),
    );
    Some(Link {
        key,
        start: start.clone(),
        end,
        content,
    })
}

fn parse(source: &str, resolved: bool) -> Events {
    #[cfg(test)]
    super::PARSED_BYTES.with(|work| work.set(work.get() + source.len()));
    let mut callback = |_: pulldown_cmark::BrokenLink<'_>| resolved.then(|| ("".into(), "".into()));
    let mut options = crate::options();
    options.remove(pulldown_cmark::Options::ENABLE_TABLES);
    Parser::new_with_broken_link_callback(source, options, Some(&mut callback))
        .into_offset_iter()
        .filter(|(event, _)| {
            !matches!(
                event,
                Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph)
            )
        })
        .map(|(event, range)| (event.into_static(), range))
        .collect()
}

fn brackets(source: &str, opaque: &[Range<usize>]) -> Option<Vec<Range<usize>>> {
    let mut result = Vec::new();
    let mut offset = 0;
    let mut span = 0;
    while offset < source.len() {
        let start = offset;
        if offset == 0 && source.as_bytes().get(offset) == Some(&b'!') {
            offset += 1;
        }
        if source.as_bytes().get(offset) != Some(&b'[') {
            return None;
        }
        offset += 1;
        let mut escaped = false;
        loop {
            if let Some(range) = opaque.get(span)
                && range.start == offset
            {
                offset = range.end;
                span += 1;
                continue;
            }
            let byte = *source.as_bytes().get(offset)?;
            offset += 1;
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' => escaped = true,
                b'[' => return None,
                b']' => break,
                _ => {}
            }
        }
        result.push(start..offset);
    }
    Some(result)
}

fn independent(events: &Events, brackets: &[Range<usize>]) -> bool {
    let mut index = 0;
    for (event, range) in events {
        if let Event::Start(tag) = event
            && !matches!(
                tag,
                Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Link { .. }
                    | Tag::Image { .. }
            )
        {
            return false;
        }
        if matches!(
            event,
            Event::Start(_) | Event::Code(_) | Event::InlineHtml(_)
        ) {
            while brackets
                .get(index)
                .is_some_and(|bracket| bracket.end <= range.start)
            {
                index += 1;
            }
            if brackets
                .get(index)
                .is_none_or(|bracket| range.end > bracket.end)
            {
                return false;
            }
        }
    }
    true
}
