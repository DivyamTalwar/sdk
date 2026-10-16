use super::delimiters::Matched;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    Escape,
    Code,
    Emphasis,
    Strong,
    StrongEmphasis,
    Strike,
}

struct Node {
    previous: usize,
    range: Range<usize>,
    children: usize,
    stop: usize,
    kind: Kind,
}

#[derive(Default)]
pub(super) struct Forest {
    nodes: Vec<Node>,
    root: usize,
    #[cfg(test)]
    scan: bool,
    #[cfg(test)]
    stale: bool,
}

impl Forest {
    pub(super) fn snapshot(&self) -> usize {
        self.root
    }

    pub(super) fn restore(&mut self, cursor: usize) -> bool {
        if cursor > self.nodes.len() {
            return false;
        }
        self.root = cursor;
        true
    }

    fn node(&self, cursor: usize) -> Option<&Node> {
        cursor
            .checked_sub(1)
            .and_then(|index| self.nodes.get(index))
    }

    fn append(&mut self, range: Range<usize>, children: usize, stop: usize, kind: Kind) -> usize {
        #[cfg(test)]
        {
            crate::work::scan(1);
            if self.nodes.len() == self.nodes.capacity() {
                crate::work::copy(self.nodes.len() * std::mem::size_of::<Node>());
            }
        }
        self.nodes.push(Node {
            previous: self.root,
            range,
            children,
            stop,
            kind,
        });
        self.root = self.nodes.len();
        self.snapshot()
    }

    pub(super) fn text(&mut self, range: Range<usize>) -> usize {
        if range.is_empty() {
            self.root
        } else {
            self.append(range, 0, 0, Kind::Text)
        }
    }

    pub(super) fn escape(&mut self, range: Range<usize>) -> usize {
        self.append(range, 0, 0, Kind::Escape)
    }

    pub(super) fn code(&mut self, opening: Range<usize>, closing: Range<usize>) {
        self.append(
            opening.start..closing.end,
            opening.end,
            closing.start,
            Kind::Code,
        );
    }

    pub(super) fn wrap(&mut self, matched: Matched) -> Option<(usize, usize)> {
        let opening = self.node(matched.opener_cursor)?;
        let closing = self.node(matched.closer_cursor)?;
        if opening.kind != Kind::Text
            || closing.kind != Kind::Text
            || opening.range.start > matched.opener.start
            || opening.range.end < matched.opener.end
            || closing.range.start > matched.closer.start
            || closing.range.end < matched.closer.end
            || matched.opener.is_empty()
            || matched.opener.end > matched.closer.start
            || matched.opener.len() != matched.closer.len()
            || matched.opener_cursor >= matched.closer_cursor
            || matched.closer_cursor != self.root
        {
            return None;
        }
        let (prefix, start, end, children) = (
            opening.previous,
            opening.range.start,
            closing.range.end,
            closing.previous,
        );
        #[cfg(test)]
        if self.scan {
            let mut cursor = children;
            while cursor != matched.opener_cursor {
                crate::work::scan(1);
                cursor = self.node(cursor)?.previous;
            }
        }
        let width = matched.opener.len();
        let kind = match (matched.byte, width >= 2, width % 2 != 0) {
            (b'~', _, _) => Kind::Strike,
            (b'*' | b'_', true, true) => Kind::StrongEmphasis,
            (b'*' | b'_', true, false) => Kind::Strong,
            (b'*' | b'_', false, _) => Kind::Emphasis,
            _ => return None,
        };
        if !self.restore(prefix) {
            return None;
        }
        let left = self.text(start..matched.opener.start);
        self.append(
            matched.opener.start..matched.closer.end,
            children,
            matched.opener_cursor,
            kind,
        );
        let right = self.text(matched.closer.end..end);
        #[cfg(test)]
        let left = if self.stale && start < matched.opener.start {
            matched.opener_cursor
        } else {
            left
        };
        Some((left, right))
    }

    pub(super) fn scopes(&self) -> Vec<Range<usize>> {
        let mut cursor = self.root;
        let mut result = Vec::new();
        while let Some(node) = self.node(cursor) {
            #[cfg(test)]
            crate::work::scan(1);
            if node.kind != Kind::Text {
                debug_assert!(
                    matches!(node.kind, Kind::Escape | Kind::Code)
                        || node.stop <= node.children && node.children < cursor
                );
                #[cfg(test)]
                crate::work::copy(
                    std::mem::size_of::<Range<usize>>()
                        * (1 + if result.len() == result.capacity() {
                            result.len()
                        } else {
                            0
                        }),
                );
                result.push(node.range.clone());
            }
            cursor = node.previous;
        }
        #[cfg(test)]
        crate::work::copy(result.len() * std::mem::size_of::<Range<usize>>());
        result.reverse();
        result
    }

    #[cfg(test)]
    pub(super) fn retained(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>()
    }

    #[cfg(test)]
    pub(super) fn rendered(&self, source: &str) -> Option<crate::Line> {
        use crate::Style;
        enum Task {
            List(usize, usize, Style),
            Value(usize, Style),
        }
        let mut tasks = vec![Task::List(self.root, 0, Style::default())];
        let mut line = crate::Line::default();
        let end = source.trim_end_matches([' ', '\t']).len();
        while let Some(task) = tasks.pop() {
            match task {
                Task::List(cursor, stop, style) => {
                    if cursor == stop {
                        continue;
                    }
                    let node = self.node(cursor)?;
                    tasks.push(Task::Value(cursor, style));
                    tasks.push(Task::List(node.previous, stop, style));
                }
                Task::Value(cursor, mut style) => {
                    let node = self.node(cursor)?;
                    match node.kind {
                        Kind::Text => line.push(
                            source.get(node.range.start.min(end)..node.range.end.min(end))?,
                            style,
                            None,
                        ),
                        Kind::Escape => line.push(
                            source.get(node.range.start + 1..node.range.end)?,
                            style,
                            None,
                        ),
                        Kind::Code => {
                            let code = source.get(node.children..node.stop)?;
                            let code = code.replace(['\r', '\n'], " ");
                            let code = if code.starts_with(' ')
                                && code.ends_with(' ')
                                && code.bytes().any(|byte| byte != b' ')
                            {
                                code.get(1..code.len() - 1)?
                            } else {
                                &code
                            };
                            style.foreground = Some(crate::style::YELLOW);
                            line.push(code, style, None);
                        }
                        kind => {
                            style.bold |= matches!(kind, Kind::Strong | Kind::StrongEmphasis);
                            style.italic |= matches!(kind, Kind::Emphasis | Kind::StrongEmphasis);
                            style.strikethrough |= kind == Kind::Strike;
                            style.dim |= kind == Kind::Strike;
                            tasks.push(Task::List(node.children, node.stop, style));
                        }
                    }
                }
            }
        }
        Some(line)
    }
}

#[cfg(test)]
#[path = "../tests/inline_forest.rs"]
mod tests;
