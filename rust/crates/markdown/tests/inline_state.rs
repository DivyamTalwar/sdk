use super::*;
use crate::inline_scan::tokens::Tokens;
use proptest::prelude::*;

fn feed(state: &mut State, tokens: &mut Tokens, source: &str) {
    tokens.push(source);
    while let Some(token) = tokens.ready.pop_front() {
        state.feed(token.range, token.kind, false);
    }
}

fn full(source: &str) -> crate::Line {
    let events: Vec<_> = pulldown_cmark::Parser::new_ext(source, crate::options()).collect();
    let mut lines = crate::render::render_styled(&events, crate::Style::default());
    assert_eq!(lines.len(), 1);
    lines.pop().expect("one inline paragraph")
}

fn parts() -> impl Strategy<Value = Vec<&'static str>> {
    prop::collection::vec(
        prop::sample::select(vec![
            "`", "``", "```", "\\`", "\\``", "\\```", "\\*", "\\_", "\\\\", "*", "**", "***", "_",
            "__", "~", "~~", "x", " ", ".", "!", "[", "]", "¡", "界", "🦀", "…", "\u{e}", "\u{7f}",
            "‐",
        ]),
        0..64,
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/inline_state.txt")
        ))),
        ..ProptestConfig::with_cases(64)
    })]
    #[test]
    fn retained_code_and_delimiter_state_matches_full_parse(
        parts in parts(), cuts in prop::collection::vec(any::<u8>(), 0..32),
    ) {
        let source = format!("x {} x", parts.concat());
        let mut boundaries: Vec<_> = cuts.into_iter().map(|cut| usize::from(cut) % (source.len() + 1))
            .filter(|cut| source.is_char_boundary(*cut)).collect();
        boundaries.push(source.len());
        boundaries.sort_unstable();
        boundaries.dedup();
        let mut tokens = Tokens::default();
        let mut state = State::default();
        for boundary in boundaries {
            let prefix = source.get(..boundary).expect("UTF-8 prefix");
            feed(&mut state, &mut tokens, prefix);
            prop_assert!(!state.invalid);
            // EOF tokens remain speculative. Here the lexical right flank is
            // settled, so compare the retained forest without finalizing EOF.
            if tokens.neutral() && !prefix.is_empty() {
                prop_assert_eq!(state.forest.rendered(prefix), Some(full(prefix)), "prefix {:?}", prefix);
            }
        }
        prop_assert!(tokens.neutral());
        prop_assert_eq!(state.forest.rendered(&source), Some(full(&source)));
    }
}

fn measure(count: usize, rescan: bool) -> (usize, usize, usize) {
    let mut source = String::from("x `a ");
    let mut state = State {
        rescan,
        ..State::default()
    };
    let mut tokens = Tokens::default();
    let before = crate::work::current();
    feed(&mut state, &mut tokens, &source);
    for _ in 0..count {
        source.push_str("**x**``a`` ");
        feed(&mut state, &mut tokens, &source);
    }
    source.push_str("` x");
    feed(&mut state, &mut tokens, &source);
    (
        crate::work::current() - before,
        state.retained() + tokens.retained(),
        source.len(),
    )
}

#[test]
fn retained_code_state_has_linear_work_and_storage() {
    for count in [64, 128, 256, 1024] {
        let small = measure(count, false);
        let large = measure(count * 2, false);
        assert!(
            large.0 <= 128 * large.2 + 4096,
            "{} work for {} bytes",
            large.0,
            large.2
        );
        assert!(
            large.0 <= 2 * small.0 + 4096,
            "work {} -> {}",
            small.0,
            large.0
        );
        assert!(
            large.1 <= 2 * small.1 + 4096,
            "storage {} -> {}",
            small.1,
            large.1
        );
    }
}

#[test]
fn work_property_detects_scanning_retained_scopes() {
    let small = measure(128, true);
    let large = measure(256, true);
    assert!(large.0 > 2 * small.0 + 4096);
}

#[test]
fn render_property_detects_not_restoring_the_opener() {
    let source = "x `a **b** ` z";
    let mut state = State {
        stale: true,
        ..State::default()
    };
    feed(&mut state, &mut Tokens::default(), source);
    assert_ne!(state.forest.rendered(source), Some(full(source)));
}
