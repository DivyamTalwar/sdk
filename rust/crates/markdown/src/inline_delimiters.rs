use std::ops::Range;
#[path = "inline_punctuation.rs"]
mod punctuation;
use punctuation::punctuation;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Snapshot {
    heads: [usize; 3],
    tilde_floor: usize,
    depth: usize,
}

#[derive(Clone, Debug)]
pub(super) struct Run {
    pub(super) range: Range<usize>,
    pub(super) byte: u8,
    pub(super) open: bool,
    pub(super) close: bool,
}

impl Run {
    pub(super) fn new(range: Range<usize>, byte: u8, before: char, after: char) -> Self {
        let intraword = byte == b'*' || byte == b'~' && range.len() == 2;
        let open = !after.is_whitespace()
            && (byte == b'~' && range.len() == 2
                || intraword && !punctuation(after)
                || before.is_whitespace()
                || punctuation(before));
        let close = !before.is_whitespace()
            && (intraword && !punctuation(before)
                || byte == b'~' && before == '~'
                || after.is_whitespace()
                || punctuation(after));
        Self {
            range,
            byte,
            open,
            close,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Matched {
    pub(super) opener: Range<usize>,
    pub(super) closer: Range<usize>,
    pub(super) byte: u8,
}

#[derive(Clone)]
struct Entry {
    before: Snapshot,
    candidates: [usize; 4],
    range: Range<usize>,
    original: usize,
    first: usize,
    both: bool,
    #[cfg(test)]
    byte: u8,
}

#[derive(Default)]
pub(super) struct Delimiters {
    entries: Vec<Entry>,
    heads: Snapshot,
    #[cfg(test)]
    scan: bool,
}

fn group(byte: u8) -> usize {
    match byte {
        b'*' => 0,
        b'_' => 1,
        _ => 2,
    }
}

impl Delimiters {
    pub(super) fn snapshot(&self) -> Snapshot {
        self.heads
    }

    pub(super) fn restore(&mut self, snapshot: Snapshot) {
        self.heads = snapshot;
    }

    pub(super) fn first(&self) -> Option<usize> {
        self.heads
            .heads
            .into_iter()
            .max()
            .and_then(|id| self.entry(id))
            .map(|entry| entry.first)
    }

    pub(super) fn neutral(&self) -> bool {
        self.heads == Snapshot::default()
    }

    fn entry(&self, id: usize) -> Option<&Entry> {
        id.checked_sub(1).and_then(|index| self.entries.get(index))
    }

    fn push(&mut self, range: Range<usize>, original: usize, byte: u8, both: bool) {
        let class = group(byte);
        let head = self.heads.heads.get(class).copied().unwrap_or_default();
        let mut candidates = self.entry(head).map_or([0; 4], |entry| entry.candidates);
        let id = self.entries.len() + 1;
        if byte == b'~' {
            if let Some(slot) = candidates.get_mut(original.saturating_sub(1)) {
                *slot = id;
            }
        } else {
            if let Some(slot) = candidates.get_mut(original % 3) {
                *slot = id;
            }
            if !both && let Some(slot) = candidates.last_mut() {
                *slot = id;
            }
        }
        let first = self.first().unwrap_or(range.start);
        #[cfg(test)]
        {
            crate::work::scan(1);
            crate::work::copy(std::mem::size_of::<Snapshot>() + std::mem::size_of::<[usize; 4]>());
            if self.entries.len() == self.entries.capacity() {
                crate::work::copy(self.entries.len() * std::mem::size_of::<Entry>());
            }
        }
        self.entries.push(Entry {
            before: self.snapshot(),
            candidates,
            range,
            original,
            first,
            both,
            #[cfg(test)]
            byte,
        });
        if let Some(head) = self.heads.heads.get_mut(class) {
            *head = id;
        }
        self.heads.depth += 1;
    }

    fn candidate(&self, byte: u8, count: usize, both: bool) -> Option<usize> {
        #[cfg(test)]
        if self.scan {
            let mut cursor = self.heads.heads.into_iter().max().unwrap_or_default();
            while let Some(entry) = self.entry(cursor) {
                crate::work::scan(1);
                if entry.byte == byte
                    && (byte != b'~'
                        || entry.original == count && entry.before.depth >= self.heads.tilde_floor)
                    && (!both && !entry.both || (entry.original + count) % 3 != 0 || count % 3 == 0)
                {
                    return Some(cursor);
                }
                cursor = entry.before.heads.into_iter().max().unwrap_or_default();
            }
            return None;
        }
        let head = self.heads.heads.get(group(byte)).copied()?;
        let candidates = &self.entry(head)?.candidates;
        if byte == b'~' {
            #[cfg(test)]
            crate::work::scan(1);
            return candidates.get(count.checked_sub(1)?).copied().filter(|id| {
                self.entry(*id)
                    .is_some_and(|entry| entry.before.depth >= self.heads.tilde_floor)
            });
        }
        let mut latest = 0;
        for (residue, id) in candidates.iter().copied().take(3).enumerate() {
            #[cfg(test)]
            crate::work::scan(1);
            if count % 3 == 0 || (count + residue) % 3 != 0 {
                latest = latest.max(id);
            }
        }
        if !both {
            latest = latest.max(candidates.last().copied().unwrap_or_default());
        }
        (latest != 0).then_some(latest)
    }

    pub(super) fn feed(&mut self, run: Run, mut matched: impl FnMut(Matched)) {
        let original = run.range.len();
        if original == 0 || run.byte == b'~' && original > 2 {
            return;
        }
        let both = run.open && run.close;
        let mut remaining = run.range;
        while run.close && !remaining.is_empty() {
            let Some(id) = self.candidate(run.byte, original, both) else {
                // The pinned parser shares this floor across both tilde lengths.
                if run.byte == b'~' {
                    self.heads.tilde_floor = self.heads.depth;
                }
                break;
            };
            let Some(entry) = self.entry(id) else {
                break;
            };
            let (before, opener, opener_length, opener_both) = (
                entry.before,
                entry.range.clone(),
                entry.original,
                entry.both,
            );
            let count = opener.len().min(remaining.len());
            let open = opener.end - count..opener.end;
            let close = remaining.start..remaining.start + count;
            let floor = self.heads.tilde_floor.min(before.depth);
            self.restore(before);
            self.heads.tilde_floor = floor;
            if opener.start < open.start {
                self.push(
                    opener.start..open.start,
                    opener_length,
                    run.byte,
                    opener_both,
                );
            }
            remaining.start += count;
            matched(Matched {
                opener: open,
                closer: close,
                byte: run.byte,
            });
        }
        if run.open && !remaining.is_empty() {
            self.push(remaining, original, run.byte, both);
        }
    }

    #[cfg(test)]
    pub(super) fn retained(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<Entry>()
    }
}

#[cfg(test)]
#[path = "../tests/inline_delimiters.rs"]
mod tests;
