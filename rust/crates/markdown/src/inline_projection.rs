use super::{Cache, Key, Output, text_output};
use crate::inline_scan::forest::{Forest, Kind, PAGE};
use crate::{Line, Range, Style};

impl Cache {
    fn entity(&mut self, source: &str, range: Range<usize>) -> Option<usize> {
        let parsed = source.get(range.clone())?;
        let (events, _) =
            crate::events::scoped(parsed, &crate::References::default(), crate::options(), 0);
        let slice = crate::inline_scan::Slice {
            original: source,
            parsed,
            base: range.start,
            start: self.input.origin?,
            sentinel: 0,
            boundary: 0,
        };
        let mut body = 0;
        for (event, part) in events {
            if let crate::Event::Text(text) = event {
                let leaf = self.value(
                    14,
                    range.start + part.start..range.start + part.end,
                    &text,
                    &slice,
                    Style::default(),
                )?;
                body = self.join(body, leaf);
            }
        }
        Some(body)
    }

    fn break_value(&mut self, source: &str, range: Range<usize>, escaped: bool) -> usize {
        let key = Key::Leaf(15, range.start, range.end);
        if let Some(&id) = self.keys.get(&key) {
            return id;
        }
        let horizontal = |index| {
            source
                .as_bytes()
                .get(index)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t' | 0x0b | 0x0c))
        };
        let hard = escaped
            || range
                .start
                .checked_sub(2)
                .is_some_and(|at| horizontal(at) && horizontal(at + 1));
        let value = if hard {
            Output::newline(false)
        } else {
            text_output(" ".into(), Style::default())
        };
        self.insert(key, value)
    }

    fn projected(&self, forest: &Forest, cursor: usize) -> Option<(usize, usize)> {
        if cursor == 0 {
            return Some((0, 0));
        }
        let (epoch, full, trimmed) = forest.node(cursor)?.output?;
        (epoch == self.epoch).then_some((full, trimmed))
    }

    fn suffix(&mut self, body: usize, prefix: usize) -> Option<usize> {
        if prefix == 0 {
            return Some(body);
        }
        let key = Key::Suffix(body, prefix);
        if let Some(&id) = self.keys.get(&key) {
            return Some(id);
        }
        let output = self
            .nodes
            .get(body)?
            .suffix_after(self.nodes.get(prefix)?)?;
        Some(self.insert(key, output))
    }

    pub(in crate::inline_scan) fn forest(
        &mut self,
        forest: &mut Forest,
        root: usize,
        source: &str,
        visible: Range<usize>,
        lines: &mut Vec<Line>,
        first: usize,
    ) -> Option<(usize, usize)> {
        #[cfg(test)]
        if super::REPLAY.get() {
            let epoch = self.epoch.checked_add(1).expect("render epoch");
            *self = Self::default();
            self.epoch = epoch;
        }
        self.input.feed(source, visible.start);
        if self.nodes.is_empty() {
            self.nodes.push(Box::default());
        }
        let mut pending = vec![root];
        while let Some(cursor) = pending.pop() {
            if self.projected(forest, cursor).is_some() {
                continue;
            }
            let node = forest.node(cursor)?;
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
                .any(|&id| self.projected(forest, id).is_none())
            {
                pending.push(cursor);
                pending.extend(
                    dependencies
                        .into_iter()
                        .filter(|&id| self.projected(forest, id).is_none()),
                );
                continue;
            }
            #[cfg(test)]
            crate::work::scan(1);
            let (mut before, mut trimmed) = self.projected(forest, previous)?;
            let content = match kind {
                Kind::Text | Kind::Escape => {
                    let range = if kind == Kind::Text {
                        self.input.literal_range(range)?
                    } else {
                        range
                    };
                    let left = range.start + usize::from(kind == Kind::Escape);
                    let key = Key::Leaf(6, left, range.end);
                    let full = if let Some(&id) = self.keys.get(&key) {
                        id
                    } else {
                        let output = self.raw(source, left..range.end, Style::default())?;
                        self.insert(key, output)
                    };
                    let end = range.end.min(visible.end);
                    let right = if range.start < end {
                        range.start
                            + source
                                .get(range.start..end)?
                                .trim_end_matches([' ', '\t', '\x0b', '\x0c'])
                                .len()
                    } else {
                        end
                    };
                    let left = if right < range.end { range.start } else { left };
                    if left < right {
                        let key = Key::Leaf(6, left, right);
                        let visible = if let Some(&id) = self.keys.get(&key) {
                            id
                        } else {
                            let output = self.raw(source, left..right, Style::default())?;
                            self.insert(key, output)
                        };
                        trimmed = self.join(before, visible);
                    }
                    full
                }
                Kind::Code => {
                    let key = Key::Leaf(1, range.start, range.end);
                    if let Some(&id) = self.keys.get(&key) {
                        id
                    } else {
                        let output = text_output(
                            self.input.code(children..stop, source)?,
                            Style {
                                foreground: Some(crate::style::YELLOW),
                                ..Style::default()
                            },
                        );
                        self.insert(key, output)
                    }
                }
                Kind::Atom(id) => id,
                Kind::Entity => self.entity(source, range)?,
                Kind::Break(escaped) => {
                    let body = self.break_value(source, range.clone(), escaped);
                    if escaped {
                        let key = Key::Leaf(6, range.start, range.start + 1);
                        let slash = if let Some(&id) = self.keys.get(&key) {
                            id
                        } else {
                            let output =
                                self.raw(source, range.start..range.start + 1, Style::default())?;
                            self.insert(key, output)
                        };
                        trimmed = self.join(before, slash);
                    } else {
                        before = trimmed;
                    }
                    body
                }
                _ => {
                    let (children, _) = self.projected(forest, children)?;
                    let (stop, _) = self.projected(forest, stop)?;
                    let body = self.suffix(children, stop)?;
                    self.mask(
                        body,
                        Style {
                            italic: matches!(kind, Kind::Emphasis | Kind::StrongEmphasis),
                            bold: matches!(kind, Kind::Strong | Kind::StrongEmphasis),
                            dim: kind == Kind::Strike,
                            strikethrough: kind == Kind::Strike,
                            ..Style::default()
                        },
                        None,
                    )
                }
            };
            let full = self.join(before, content);
            if !matches!(kind, Kind::Text | Kind::Escape | Kind::Break(_)) {
                trimmed = full;
            }
            let index = cursor.checked_sub(1)?;
            forest
                .nodes
                .get_mut(index / PAGE)?
                .get_mut(index % PAGE)?
                .output = Some((self.epoch, full, trimmed));
        }
        let (_, current) = self.projected(forest, root)?;
        let output = self.nodes.get(current)?;
        let changed = output.update(self.nodes.get(self.last)?, lines, first);
        let visible = output.visible();
        self.last = current;
        Some((changed, visible))
    }
}

#[cfg(test)]
#[path = "../tests/inline_projection.rs"]
mod tests;
