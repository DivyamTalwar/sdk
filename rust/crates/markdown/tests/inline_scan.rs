use super::*;
use proptest::prelude::*;

fn advance(scanner: &mut Scanner, original: &str, base: usize) -> usize {
    let context = if base == 0 {
        String::new()
    } else {
        format!(
            "x{}",
            original
                .get(..base)
                .and_then(|s| s.chars().next_back())
                .unwrap()
        )
    };
    let source = format!("{context}{}", original.get(base..).unwrap());
    let events: Vec<_> = pulldown_cmark::Parser::new_ext(&source, options())
        .into_offset_iter()
        .collect();
    scanner.frontier(original, base, &source, &events, context.len())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn retained_scanner_matches_replay(
        checkpoints in any::<bool>(),
        parts in prop::collection::vec(prop_oneof![
            any::<char>().prop_map(|c| c.to_string()),
            prop::sample::select(vec![
                "\\", "*", "**", "`", "!", "[", "]", "(", ")", "&", ";", "\n",
                "[x](u)", "![a](u)", "<i>", "</i>", "<https://x>", "<!--x-->",
            ]).prop_map(str::to_owned),
        ], 0..48),
    ) {
        let mut retained = Scanner::default();
        let mut replay = Scanner { replay: true, ..Scanner::default() };
        let mut source = String::new();
        let mut base = 0;
        for part in parts {
            source.push_str(&part);
            let frontier = advance(&mut retained, &source, base);
            prop_assert_eq!(frontier, advance(&mut replay, &source, base),
                "source {:?}", source);
            prop_assert_eq!(&retained.atomic, &replay.atomic, "source {:?}", source);
            let left = retained.state.projection(retained.tokens.preview().as_ref());
            let right = replay.state.projection(replay.tokens.preview().as_ref());
            if !checkpoints {
                prop_assert_eq!(retained.state.forest.rendered_at(left, &source),
                    replay.state.forest.rendered_at(right, &source), "source {:?}", source);
            } else {
                prop_assert_eq!(retained.state.forest.scopes_since(left, base),
                    replay.state.forest.scopes_since(right, base), "source {:?}", source);
                base += frontier.saturating_sub(if base == 0 { 0 } else {
                    1 + source.get(..base).and_then(|s| s.chars().next_back()).unwrap().len_utf8()
                });
                retained.discard_before(base);
                replay.discard_before(base);
            }
        }
    }
}

fn fed_bytes(count: usize, replay: bool) -> (usize, usize) {
    let mut scanner = Scanner {
        replay,
        ..Scanner::default()
    };
    let mut source = String::new();
    for part in ["*", "x", "\\", "*", "`", "x", "`", "!", "&", "amp;", "\n"]
        .into_iter()
        .cycle()
        .take(count)
    {
        source.push_str(part);
        // No link/HTML syntax can override the lexical classifications here.
        scanner.frontier(&source, 0, &source, &[], 0);
    }
    (scanner.fed_bytes, source.len())
}

#[test]
fn unchanged_syntax_feeds_each_byte_once() {
    for count in [64, 128, 256, 1024] {
        let (fed, bytes) = fed_bytes(count, false);
        assert!(
            fed <= bytes,
            "{count} pushes: {fed} fed bytes for {bytes} input bytes"
        );
    }
}

#[test]
fn work_property_detects_replaying_settled_tokens() {
    let small = fed_bytes(64, true);
    let large = fed_bytes(128, true);
    assert!(small.0 > small.1);
    assert!(large.0 > 2 * small.0 + 4096);
}
