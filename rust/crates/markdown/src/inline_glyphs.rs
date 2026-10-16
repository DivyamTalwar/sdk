use super::{Forest, Kind, PAGE};
use crate::{Span, Spans, Style, Text};
use std::ops::Range;

#[derive(Default)]
pub(super) struct Input {
    origin: Option<usize>,
    raw: Text,
    code: Text,
    normalized: String,
    offsets: Vec<usize>,
    nonspace: Vec<usize>,
}

pub(super) struct View {
    full: Spans,
    trimmed: Spans,
}

impl Input {
    fn feed(&mut self, source: &str, start: usize) {
        let origin = *self.origin.get_or_insert(start);
        let source = source.get(origin..).expect("inline source");
        let delta = source
            .get(self.raw.len()..)
            .expect("append-only inline source");
        if self.offsets.is_empty() {
            self.offsets.push(0);
            self.nonspace.push(0);
        }
        #[cfg(test)]
        {
            crate::work::render_feed(delta.len());
        }
        self.raw.push_str(delta);
        let from = self.normalized.len();
        let mut nonspace = *self.nonspace.last().expect("initial prefix");
        for character in delta.chars() {
            let normalized = match character {
                '\r' | '\n' => Some(' '),
                c => Some(c),
            };
            if let Some(character) = normalized {
                #[cfg(test)]
                if self.normalized.capacity() - self.normalized.len() < character.len_utf8() {
                    crate::work::copy(self.normalized.len());
                }
                self.normalized.push(character);
                nonspace += usize::from(character != ' ');
            }
            #[cfg(test)]
            crate::work::copy(character.len_utf8() * 2 * std::mem::size_of::<usize>());
            for _ in 0..character.len_utf8() {
                #[cfg(test)]
                {
                    if self.offsets.len() == self.offsets.capacity() {
                        crate::work::copy(self.offsets.len() * std::mem::size_of::<usize>());
                    }
                    if self.nonspace.len() == self.nonspace.capacity() {
                        crate::work::copy(self.nonspace.len() * std::mem::size_of::<usize>());
                    }
                }
                self.offsets.push(self.normalized.len());
                self.nonspace.push(nonspace);
            }
        }
        self.code
            .push_str(self.normalized.get(from..).expect("normalized append"));
    }

    fn text(&self, source: &str, range: Range<usize>) -> Option<Text> {
        let origin = self.origin?;
        self.raw.source_range(
            range.start.checked_sub(origin)?..range.end.checked_sub(origin)?,
            source.get(origin..)?,
        )
    }

    fn code(&self, range: Range<usize>) -> Option<Text> {
        let origin = self.origin?;
        let (left, right) = (
            range.start.checked_sub(origin)?,
            range.end.checked_sub(origin)?,
        );
        let mut start = *self.offsets.get(left)?;
        let mut end = *self.offsets.get(right)?;
        if self.normalized.as_bytes().get(start) == Some(&b' ')
            && self.normalized.as_bytes().get(end.checked_sub(1)?) == Some(&b' ')
            && self.nonspace.get(right)? > self.nonspace.get(left)?
        {
            start += 1;
            end -= 1;
        }
        self.code.source_range(start..end, &self.normalized)
    }
}

fn run(text: Text, style: Style) -> Spans {
    let mut spans = Spans::default();
    if !text.is_empty() {
        spans.push_content(
            Span {
                text,
                style,
                link: None,
            },
            false,
        );
        spans.freeze();
    }
    spans
}

impl Forest {
    #[cfg(test)]
    pub(in crate::inline_scan) fn glyph_retained(
        &self,
        nodes: &mut std::collections::HashSet<usize>,
        texts: &mut std::collections::HashSet<usize>,
        links: &mut std::collections::HashSet<*const u8>,
    ) -> (usize, usize) {
        let (raw, raw_meta) = self.input.raw.retained(texts);
        let (code, code_meta) = self.input.code.retained(texts);
        let mut payload = raw + code + self.input.normalized.capacity();
        let mut metadata = raw_meta
            + code_meta
            + (self.input.offsets.capacity() + self.input.nonspace.capacity())
                * std::mem::size_of::<usize>();
        for node in self.nodes.iter().flatten() {
            if let Some(view) = &node.view {
                metadata += std::mem::size_of::<View>();
                for spans in [&view.full, &view.trimmed] {
                    let (bytes, size) = spans.retained(nodes, texts, links);
                    payload += bytes;
                    metadata += size;
                }
            }
        }
        (payload, metadata)
    }

    fn cached(&self, cursor: usize) -> Option<&View> {
        self.node(cursor)?.view.as_deref()
    }

    pub(in crate::inline_scan) fn glyphs(
        &mut self,
        root: usize,
        source: &str,
        start: usize,
        end: usize,
    ) -> Option<Spans> {
        #[cfg(test)]
        if REPLAY.get() {
            self.input = Input::default();
            for node in self.nodes.iter_mut().flatten().take(self.length) {
                node.view = None;
            }
        }
        self.input.feed(source, start);
        let mut pending = vec![root];
        while let Some(cursor) = pending.pop() {
            if cursor == 0 || self.cached(cursor).is_some() {
                continue;
            }
            let node = self.node(cursor)?;
            let (previous, children, stop, kind, range) = (
                node.previous,
                node.children,
                node.stop,
                node.kind,
                node.range.clone(),
            );
            let scoped = matches!(
                kind,
                Kind::Emphasis | Kind::Strong | Kind::StrongEmphasis | Kind::Strike
            );
            let dependencies = [
                previous,
                if scoped { children } else { 0 },
                if scoped { stop } else { 0 },
            ];
            if dependencies
                .iter()
                .any(|&id| id != 0 && self.cached(id).is_none())
            {
                pending.push(cursor);
                pending.extend(
                    dependencies
                        .into_iter()
                        .filter(|&id| id != 0 && self.cached(id).is_none()),
                );
                continue;
            }
            #[cfg(test)]
            crate::work::scan(1);
            let mut full = self
                .cached(previous)
                .map_or_else(Spans::default, |view| view.full.clone());
            let mut trimmed = self
                .cached(previous)
                .map_or_else(Spans::default, |view| view.trimmed.clone());
            let content = match kind {
                Kind::Text | Kind::Escape => {
                    let left = range.start + usize::from(kind == Kind::Escape);
                    let raw = source.get(left..range.end)?;
                    let visible = raw.trim_end_matches([' ', '\t', '\r', '\n']);
                    if !visible.is_empty() {
                        trimmed = full.clone();
                        trimmed.concat(run(
                            self.input.text(source, left..left + visible.len())?,
                            Style::default(),
                        ));
                        trimmed.freeze();
                    }
                    run(self.input.text(source, left..range.end)?, Style::default())
                }
                Kind::Code => run(
                    self.input.code(children..stop)?,
                    Style {
                        foreground: Some(crate::style::YELLOW),
                        ..Style::default()
                    },
                ),
                _ => {
                    let body = self
                        .cached(children)
                        .map_or_else(Spans::default, |view| view.full.clone());
                    let prefix = self
                        .cached(stop)
                        .map_or_else(Spans::default, |view| view.full.clone());
                    body.suffix_after(&prefix).styled(Style {
                        italic: matches!(kind, Kind::Emphasis | Kind::StrongEmphasis),
                        bold: matches!(kind, Kind::Strong | Kind::StrongEmphasis),
                        dim: kind == Kind::Strike,
                        strikethrough: kind == Kind::Strike,
                        ..Style::default()
                    })
                }
            };
            full.concat(content);
            full.freeze();
            if !matches!(kind, Kind::Text | Kind::Escape) {
                trimmed = full.clone();
            }
            let index = cursor.checked_sub(1)?;
            self.nodes
                .get_mut(index / PAGE)?
                .get_mut(index % PAGE)?
                .view = Some(Box::new(View { full, trimmed }));
        }
        // A parser-visible prefix can end inside a physical token. Render
        // its covered source without committing the token's EOF preview.
        if let Some(node) = self.node(root)
            && node.range.start < end
            && end < node.range.end
            && node.kind == Kind::Escape
        {
            let mut spans = self
                .cached(node.previous)
                .map_or_else(Spans::default, |view| view.full.clone());
            spans.concat(run(
                self.input.text(source, node.range.start..end)?,
                Style::default(),
            ));
            return Some(spans);
        }
        Some(
            self.cached(root)
                .map_or_else(Spans::default, |view| view.trimmed.clone()),
        )
    }
}

#[cfg(test)]
thread_local! { static REPLAY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[cfg(test)]
#[path = "../tests/inline_glyphs.rs"]
mod tests;
