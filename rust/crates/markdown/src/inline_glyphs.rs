use crate::Text;
use std::ops::Range;

#[derive(Default)]
pub(super) struct Input {
    origin: Option<usize>,
    raw: Text,
    code: Text,
    normalized: String,
    offsets: Vec<usize>,
    nonspace: Vec<usize>,
    breaks: Vec<usize>,
    break_counts: Vec<usize>,
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
            self.break_counts.push(0);
        }
        #[cfg(test)]
        {
            crate::work::render_feed(delta.len());
        }
        let raw_start = self.raw.len();
        self.raw.push_str(delta);
        let from = self.normalized.len();
        let mut nonspace = *self.nonspace.last().expect("initial prefix");
        for (offset, character) in delta.char_indices() {
            if matches!(character, '\r' | '\n') {
                #[cfg(test)]
                if self.breaks.len() == self.breaks.capacity() {
                    crate::work::copy(self.breaks.len() * std::mem::size_of::<usize>());
                }
                self.breaks.push(origin + raw_start + offset);
            }
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
            crate::work::copy(character.len_utf8() * 3 * std::mem::size_of::<usize>());
            for _ in 0..character.len_utf8() {
                #[cfg(test)]
                {
                    if self.offsets.len() == self.offsets.capacity() {
                        crate::work::copy(self.offsets.len() * std::mem::size_of::<usize>());
                    }
                    if self.nonspace.len() == self.nonspace.capacity() {
                        crate::work::copy(self.nonspace.len() * std::mem::size_of::<usize>());
                    }
                    if self.break_counts.len() == self.break_counts.capacity() {
                        crate::work::copy(self.break_counts.len() * std::mem::size_of::<usize>());
                    }
                }
                self.offsets.push(self.normalized.len());
                self.nonspace.push(nonspace);
                self.break_counts.push(self.breaks.len());
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

    fn newlines(&self, range: Range<usize>) -> Option<impl Iterator<Item = usize> + '_> {
        let origin = self.origin?;
        let first = *self.break_counts.get(range.start.checked_sub(origin)?)?;
        let end = *self.break_counts.get(range.end.checked_sub(origin)?)?;
        Some(self.breaks.get(first..end)?.iter().copied())
    }

    #[cfg(test)]
    fn retained(&self, texts: &mut std::collections::HashSet<usize>) -> (usize, usize) {
        let (raw, raw_meta) = self.raw.retained(texts);
        let (code, code_meta) = self.code.retained(texts);
        (
            raw + code + self.normalized.capacity(),
            raw_meta
                + code_meta
                + (self.offsets.capacity()
                    + self.nonspace.capacity()
                    + self.breaks.capacity()
                    + self.break_counts.capacity())
                    * std::mem::size_of::<usize>(),
        )
    }
}

#[path = "inline_events.rs"]
mod events;
pub(in crate::inline_scan) use events::Cache;

#[cfg(test)]
thread_local! { static REPLAY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[cfg(test)]
#[path = "../tests/inline_glyphs.rs"]
mod tests;
