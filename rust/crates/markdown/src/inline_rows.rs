use crate::{Line, Spans, Style};
use std::sync::Arc;

#[derive(Clone, Default)]
pub(super) struct Rows {
    root: Option<Arc<Node>>,
}

struct Node {
    count: usize,
    visible: Option<usize>,
    kind: Kind,
}

enum Kind {
    Line(Spans),
    Concat(Rows, Rows),
    Mask(Rows, Style, Option<Arc<str>>),
}

#[derive(Clone, Copy, Default)]
pub(super) struct Mask<'a> {
    style: Style,
    link: Option<&'a Arc<str>>,
}

impl<'a> Mask<'a> {
    fn inside(self, style: Style, link: Option<&'a Arc<str>>) -> Self {
        Self {
            style: Style {
                foreground: style.foreground.or(self.style.foreground),
                bold: style.bold || self.style.bold,
                italic: style.italic || self.style.italic,
                dim: style.dim || self.style.dim,
                underline: style.underline || self.style.underline,
                strikethrough: style.strikethrough || self.style.strikethrough,
            },
            link: link.or(self.link),
        }
    }

    fn same(self, other: Self) -> bool {
        self.style == other.style
            && match (self.link, other.link) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
    }
}

pub(super) struct Row<'a> {
    pub(super) spans: &'a Spans,
    pub(super) mask: Mask<'a>,
}

impl Row<'_> {
    pub(super) fn owned(&self) -> Line {
        Line {
            spans: self
                .spans
                .clone()
                .masked(self.mask.style, self.mask.link.cloned()),
        }
    }
}

impl Rows {
    pub(super) fn len(&self) -> usize {
        self.root.as_ref().map_or(0, |node| node.count)
    }

    pub(super) fn line(spans: Spans) -> Self {
        #[cfg(test)]
        crate::work::scan(1);
        Self {
            root: Some(Arc::new(Node {
                count: 1,
                visible: (!spans.is_empty()).then_some(0),
                kind: Kind::Line(spans),
            })),
        }
    }

    pub(super) fn concat(self, other: Self) -> Self {
        if self.len() == 0 {
            return other;
        }
        if other.len() == 0 {
            return self;
        }
        #[cfg(test)]
        crate::work::scan(1);
        Self {
            root: Some(Arc::new(Node {
                count: self.len() + other.len(),
                visible: other
                    .last_visible()
                    .map(|index| self.len() + index)
                    .or_else(|| self.last_visible()),
                kind: Kind::Concat(self, other),
            })),
        }
    }

    pub(super) fn masked(self, style: Style, link: Option<Arc<str>>) -> Self {
        if self.len() == 0 || style == Style::default() && link.is_none() {
            return self;
        }
        #[cfg(test)]
        crate::work::scan(1);
        Self {
            root: Some(Arc::new(Node {
                count: self.len(),
                visible: self.last_visible(),
                kind: Kind::Mask(self, style, link),
            })),
        }
    }

    pub(super) fn iter(&self) -> Iter<'_> {
        Iter {
            pending: self
                .root
                .as_deref()
                .map(|node| (node, Mask::default()))
                .into_iter()
                .collect(),
        }
    }

    fn last_visible(&self) -> Option<usize> {
        self.root.as_ref().and_then(|node| node.visible)
    }

    // Compare shared chunks before descending to individual rows. Appending a
    // row, or changing one scope, never scans an unchanged row prefix.
    pub(super) fn changed<'a>(&'a self, previous: &'a Self) -> Vec<(usize, Row<'a>)> {
        let mut before = previous.iter();
        let mut after = self.iter();
        let mut index = 0;
        let mut changed = Vec::new();
        while let (Some(&(old, old_mask)), Some(&(new, new_mask))) =
            (before.pending.last(), after.pending.last())
        {
            #[cfg(test)]
            crate::work::scan(1);
            let shared = std::ptr::eq(old, new) && old_mask.same(new_mask);
            #[cfg(test)]
            let shared = shared && !SCAN_SHARED.get();
            if shared {
                index += old.count;
                before.pending.pop();
                after.pending.pop();
            } else if matches!(old.kind, Kind::Mask(..)) {
                before.expand();
            } else if matches!(new.kind, Kind::Mask(..)) {
                after.expand();
            } else if old.count > new.count {
                before.expand();
            } else if new.count > old.count {
                after.expand();
            } else if matches!(old.kind, Kind::Concat(..)) || matches!(new.kind, Kind::Concat(..)) {
                before.expand();
                after.expand();
            } else {
                let Kind::Line(spans) = &new.kind else {
                    unreachable!("one row")
                };
                #[cfg(test)]
                if changed.len() == changed.capacity() {
                    crate::work::copy(changed.len() * std::mem::size_of::<(usize, Row<'_>)>());
                }
                changed.push((
                    index,
                    Row {
                        spans,
                        mask: new_mask,
                    },
                ));
                index += 1;
                before.pending.pop();
                after.pending.pop();
            }
        }
        for row in after {
            #[cfg(test)]
            if changed.len() == changed.capacity() {
                crate::work::copy(changed.len() * std::mem::size_of::<(usize, Row<'_>)>());
            }
            changed.push((index, row));
            index += 1;
        }
        changed
    }

    #[cfg(test)]
    pub(super) fn retained(
        &self,
        nodes: &mut std::collections::HashSet<usize>,
        texts: &mut std::collections::HashSet<usize>,
        links: &mut std::collections::HashSet<*const u8>,
    ) -> (usize, usize) {
        let mut pending: Vec<_> = self.root.iter().collect();
        let (mut payload, mut metadata) = (0, 0);
        while let Some(node) = pending.pop() {
            if !nodes.insert(Arc::as_ptr(node) as usize) {
                continue;
            }
            metadata += std::mem::size_of::<Node>() + 2 * std::mem::size_of::<usize>();
            match &node.kind {
                Kind::Line(spans) => {
                    let (bytes, size) = spans.retained(nodes, texts, links);
                    payload += bytes;
                    metadata += size;
                }
                Kind::Concat(left, right) => {
                    pending.extend(left.root.iter());
                    pending.extend(right.root.iter());
                }
                Kind::Mask(child, _, link) => {
                    pending.extend(child.root.iter());
                    if let Some(link) = link
                        && links.insert(Arc::as_ptr(link).cast::<u8>())
                    {
                        payload += link.len();
                    }
                }
            }
        }
        (payload, metadata)
    }
}

#[cfg(test)]
thread_local! { static SCAN_SHARED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[cfg(test)]
#[path = "../tests/inline_rows.rs"]
mod tests;

pub(super) struct Iter<'a> {
    pending: Vec<(&'a Node, Mask<'a>)>,
}

impl Iter<'_> {
    fn expand(&mut self) {
        let Some((node, mask)) = self.pending.pop() else {
            return;
        };
        #[cfg(test)]
        crate::work::scan(1);
        match &node.kind {
            Kind::Concat(left, right) => {
                self.pending
                    .extend(right.root.as_deref().map(|node| (node, mask)));
                self.pending
                    .extend(left.root.as_deref().map(|node| (node, mask)));
            }
            Kind::Mask(child, style, link) => {
                let mask = mask.inside(*style, link.as_ref());
                self.pending
                    .extend(child.root.as_deref().map(|node| (node, mask)));
            }
            Kind::Line(_) => self.pending.push((node, mask)),
        }
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = Row<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let &(node, mask) = self.pending.last()?;
            if let Kind::Line(spans) = &node.kind {
                self.pending.pop();
                return Some(Row { spans, mask });
            }
            self.expand();
        }
    }
}

impl Drop for Rows {
    fn drop(&mut self) {
        let mut current = self.root.take();
        let mut pending = Vec::new();
        while let Some(node) = current.take().or_else(|| pending.pop()) {
            let Some(mut node) = Arc::into_inner(node) else {
                continue;
            };
            match &mut node.kind {
                Kind::Line(_) => {}
                Kind::Concat(left, right) => {
                    current = left.root.take();
                    pending.extend(right.root.take());
                }
                Kind::Mask(child, _, _) => current = child.root.take(),
            }
        }
    }
}

// The first and last row remain separate, so concatenation and enclosing
// styles touch a constant number of rows. Only completed middle rows enter
// the immutable row tree.
#[derive(Clone, Default)]
pub(super) struct Output {
    pub(super) head: Spans,
    pub(super) middle: Rows,
    pub(super) tail: Option<Spans>,
    pub(super) required: bool,
}

impl Output {
    pub(super) fn visible(&self) -> usize {
        #[cfg(test)]
        if SCAN_VISIBLE.get() {
            return self
                .lines()
                .iter()
                .rposition(|line| {
                    crate::work::visibility(1);
                    !line.spans.is_empty()
                })
                .map_or(0, |index| index + 1);
        }
        if self.tail.as_ref().is_some_and(|tail| !tail.is_empty()) {
            return self.len();
        }
        if let Some(index) = self.middle.last_visible() {
            return self.head_count() + index + 1;
        }
        if self.head.is_empty() { 0 } else { 1 }
    }
    fn head_count(&self) -> usize {
        usize::from(self.tail.is_none() || self.required || !self.head.is_empty())
    }

    fn len(&self) -> usize {
        self.head_count() + self.middle.len() + usize::from(self.tail.is_some())
    }

    pub(super) fn text(head: Spans) -> Self {
        Self {
            head,
            ..Self::default()
        }
    }

    pub(super) fn newline(required: bool) -> Self {
        Self {
            tail: Some(Spans::default()),
            required,
            ..Self::default()
        }
    }

    pub(super) fn append(&mut self, other: Self) {
        let Self {
            head,
            middle,
            tail,
            required,
        } = other;
        if let Some(last) = &mut self.tail {
            last.concat(head);
            last.freeze();
            if let Some(tail) = tail {
                let joined = self.tail.take().expect("existing final row");
                let row = if required || !joined.is_empty() {
                    Rows::line(joined)
                } else {
                    Rows::default()
                };
                self.middle = std::mem::take(&mut self.middle).concat(row).concat(middle);
                self.tail = Some(tail);
            }
        } else {
            self.head.concat(head);
            self.head.freeze();
            if tail.is_some() {
                self.middle = middle;
                self.tail = tail;
                self.required = required;
            }
        }
    }

    pub(super) fn masked(self, style: Style, link: Option<Arc<str>>) -> Self {
        Self {
            head: self.head.masked(style, link.clone()),
            middle: self.middle.masked(style, link.clone()),
            tail: self.tail.map(|tail| tail.masked(style, link)),
            required: self.required,
        }
    }

    pub(super) fn lines(&self) -> Vec<Line> {
        let mut lines = Vec::new();
        if self.tail.is_none() || self.required || !self.head.is_empty() {
            lines.push(Line {
                spans: self.head.clone(),
            });
        }
        lines.extend(self.middle.iter().map(|row| row.owned()));
        lines.extend(self.tail.iter().cloned().map(|spans| Line { spans }));
        lines
    }

    pub(super) fn update(&self, previous: &Self, lines: &mut Vec<Line>, first: usize) -> usize {
        if self.head_count() != previous.head_count()
            || self.tail.is_some() != previous.tail.is_some()
            || lines.len() != first + previous.len()
        {
            let next = self.lines();
            #[cfg(test)]
            {
                crate::work::copy(next.len() * std::mem::size_of::<Line>());
                if lines.capacity() < first + next.len() {
                    crate::work::copy(first * std::mem::size_of::<Line>());
                }
            }
            lines.truncate(first);
            lines.extend(next);
            return first;
        }
        let mut changed = first + self.len();
        let end = first + self.len();
        if end > lines.len() {
            #[cfg(test)]
            {
                crate::work::copy((end - lines.len()) * std::mem::size_of::<Line>());
                if lines.capacity() < end {
                    crate::work::copy(lines.len() * std::mem::size_of::<Line>());
                }
            }
            lines.resize_with(end, Line::default);
        } else {
            lines.truncate(end);
        }
        if self.head_count() != 0 && !self.head.same_snapshot(&previous.head) {
            #[cfg(test)]
            crate::work::copy(std::mem::size_of::<Line>());
            lines[first] = Line {
                spans: self.head.clone(),
            };
            changed = first;
        }
        for (index, row) in self.middle.changed(&previous.middle) {
            let at = first + self.head_count() + index;
            #[cfg(test)]
            crate::work::copy(std::mem::size_of::<Line>());
            lines[at] = row.owned();
            changed = changed.min(at);
        }
        if let (Some(next), Some(old)) = (&self.tail, &previous.tail)
            && (self.len() != previous.len() || !next.same_snapshot(old))
        {
            let at = first + self.len() - 1;
            #[cfg(test)]
            crate::work::copy(std::mem::size_of::<Line>());
            lines[at] = Line {
                spans: next.clone(),
            };
            changed = changed.min(at);
        }
        changed
    }
}

#[cfg(test)]
pub(in crate::inline_scan) use SCAN_VISIBLE as RESCAN_VISIBLE;

#[cfg(test)]
thread_local! { pub(in crate::inline_scan) static SCAN_VISIBLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
