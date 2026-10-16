use super::*;
use crate::Line;
use proptest::prelude::*;

#[derive(Clone)]
struct Expected {
    text: String,
    style: Style,
    link: Option<Arc<str>>,
    forced: bool,
}

fn canonical(entries: &[Expected]) -> Vec<Expected> {
    let mut result: Vec<Expected> = Vec::new();
    for entry in entries {
        if !entry.forced
            && let Some(last) = result.last_mut()
            && last.style == entry.style
            && match (&last.link, &entry.link) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
        {
            last.text.push_str(&entry.text);
        } else {
            result.push(entry.clone());
        }
    }
    result
}

fn value(entries: &[Expected]) -> Vec<Span> {
    canonical(entries)
        .into_iter()
        .map(|entry| Span {
            text: entry.text.into(),
            style: entry.style,
            link: entry.link,
        })
        .collect()
}

fn attributes(bits: u8) -> Style {
    Style {
        foreground: (bits & 32 != 0).then_some(crate::Color(1, 2, 3)),
        bold: bits & 1 != 0,
        italic: bits & 2 != 0,
        dim: bits & 4 != 0,
        underline: bits & 8 != 0,
        strikethrough: bits & 16 != 0,
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/spans.txt")
        ))),
        ..ProptestConfig::with_cases(64)
    })]
    #[test]
    fn shared_styled_snapshots_match_owned_runs(
        actions in prop::collection::vec((0u8..8, prop::collection::vec(any::<char>(), 0..8), 0u8..64, any::<u8>()), 0..64),
    ) {
        let targets: [Arc<str>; 3] = [Arc::from("a"), Arc::from("b"), Arc::from("a")];
        let mut actual = Spans::default();
        let mut expected: Vec<Expected> = Vec::new();
        let mut saved = vec![(actual.clone(), expected.clone())];
        for (action, chars, flags, index) in actions {
            let text: String = chars.into_iter().collect();
            let style = attributes(flags);
            let link = targets.get(usize::from(index) % 4).cloned();
            match action {
                0 => {
                    actual.push(Span { text: text.as_str().into(), style, link: link.clone() });
                    expected.push(Expected { text, style, link, forced: true });
                }
                1 => {
                    let mut line = Line { spans: actual };
                    line.push(&text, style, link.as_ref());
                    actual = line.spans;
                    if !text.is_empty() {
                        expected = canonical(&expected);
                        let same = expected.last().is_some_and(|last| last.style == style
                            && match (&last.link, &link) {
                                (None, None) => true,
                                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                                _ => false,
                            });
                        if same { expected.last_mut().expect("last run").text.push_str(&text); }
                        else { expected.push(Expected { text, style, link, forced: false }); }
                    }
                }
                2 => saved.push((actual.clone(), expected.clone())),
                3 => {
                    let (snapshot, model) = &saved[usize::from(index) % saved.len()];
                    actual = snapshot.clone(); expected = model.clone();
                }
                4 => {
                    expected = canonical(&expected);
                    let length = usize::from(index) % (expected.len() + 2);
                    actual.truncate(length); expected.truncate(length);
                }
                5 => {
                    actual = actual.styled(style);
                    for entry in &mut expected {
                        if entry.style.foreground.is_none() { entry.style.foreground = style.foreground; }
                        entry.style.bold |= style.bold; entry.style.italic |= style.italic;
                        entry.style.dim |= style.dim; entry.style.underline |= style.underline;
                        entry.style.strikethrough |= style.strikethrough;
                    }
                }
                6 => { actual.clear(); expected.clear(); }
                _ => {
                    let (snapshot, model) = &saved[usize::from(index) % saved.len()];
                    actual.concat(snapshot.clone()); expected.extend(model.iter().cloned());
                }
            }
            prop_assert_eq!(actual.len(), value(&expected).len());
            prop_assert_eq!(actual.to_vec(), value(&expected));
            let joined: String = actual.iter().map(|span| span.text.as_str()).collect();
            prop_assert_eq!(joined, expected.iter().map(|entry| entry.text.as_str()).collect::<String>());
            for (snapshot, model) in &saved { prop_assert_eq!(snapshot.to_vec(), value(model)); }
        }
    }
}

fn measure(count: usize, copy: bool) -> (usize, usize, usize) {
    COPY_PREFIX.set(copy);
    let mut line = Line::default();
    let mut snapshots = Vec::new();
    let before = crate::work::current();
    for index in 0..count {
        line.push("x", attributes((index % 2) as u8), None);
        if snapshots.len() == snapshots.capacity() {
            crate::work::copy(snapshots.len() * std::mem::size_of::<Spans>());
        }
        snapshots.push(line.spans.clone());
    }
    let work = crate::work::current() - before;
    COPY_PREFIX.set(false);
    let (mut nodes, mut texts, mut links) = (HashSet::new(), HashSet::new(), HashSet::new());
    let mut payload = 0;
    let mut retained = snapshots.capacity() * std::mem::size_of::<Spans>();
    for snapshot in snapshots {
        let (bytes, size) = snapshot.retained(&mut nodes, &mut texts, &mut links);
        payload += bytes;
        retained += bytes + size;
    }
    (work, retained, payload)
}

#[test]
fn span_snapshot_work_and_retained_bytes_are_linear() {
    for count in [64, 128, 256, 1024] {
        let small = measure(count, false);
        let large = measure(2 * count, false);
        assert!(
            large.0 <= 128 * (2 * count) + 4096,
            "{} work for {} bytes",
            large.0,
            2 * count
        );
        assert!(
            large.0 <= 2 * small.0 + 4096,
            "work {} -> {}",
            small.0,
            large.0
        );
        assert!(
            large.1 <= 2 * small.1 + 4096,
            "retained {} -> {}",
            small.1,
            large.1
        );
        assert_eq!(large.2, 2 * count);
    }
}

#[test]
fn work_property_detects_copying_prefix_runs() {
    let small = measure(64, true);
    let large = measure(128, true);
    assert!(large.0 > 2 * small.0 + 4096);
    assert!(large.1 > 2 * small.1 + 4096);
}

#[test]
fn dropping_deep_span_snapshots_does_not_recurse() {
    let mut spans = Spans::default();
    for _ in 0..20_000 {
        spans.push(Span {
            text: "x".into(),
            ..Span::default()
        });
    }
    drop(spans);
}
