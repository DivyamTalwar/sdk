use super::*;
use crate::inline_scan::Scanner;
use proptest::prelude::*;

fn view(scanner: &mut Scanner, source: &str) -> crate::Line {
    let events: Vec<_> = pulldown_cmark::Parser::new_ext(source, crate::options())
        .into_offset_iter()
        .collect();
    scanner.frontier(source, 0, source, &events, 0);
    let end = crate::inline::glyph_events(source, &events).expect("retained glyph variants");
    crate::Line {
        spans: scanner.glyphs(source, 0, end).expect("valid inline forest"),
    }
}

fn full(source: &str) -> crate::Line {
    let events: Vec<_> = pulldown_cmark::Parser::new_ext(source, crate::options()).collect();
    let mut lines = crate::render::render_styled(&events, Style::default());
    assert_eq!(lines.len(), 1);
    lines.pop().unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn retained_glyph_views_match_event_rendering(
        parts in prop::collection::vec(prop::sample::select(vec![
            "*", "**", "***", "_", "__", "~", "~~", "`", "``", "```",
            "\\*", "\\`", "\\\\", "\\", "!", "x", " ", "¡", "‐", "🦀",
        ]), 0..48),
    ) {
        let mut source = String::from("x ");
        let mut scanner = Scanner::default();
        for part in parts {
            source.push_str(part);
            let before = crate::work::rendered_bytes();
            let actual = view(&mut scanner, &source);
            prop_assert!(crate::work::rendered_bytes() - before <= source.len());
            prop_assert_eq!(actual, full(&source), "source {:?}", source);
        }
    }
}

fn measure(count: usize, replay: bool) -> (usize, usize) {
    let mut source = String::from("x ");
    let mut markdown = crate::Markdown::default();
    markdown.push(&source);
    REPLAY.set(replay);
    let before = crate::work::rendered_bytes();
    for part in ["\\", "*", "!", "*", "x", "*", "`", "x", "`"]
        .into_iter()
        .cycle()
        .take(count)
    {
        source.push_str(part);
        markdown.push(part);
    }
    let used = crate::work::rendered_bytes() - before;
    REPLAY.set(false);
    (used, source.len() - 2)
}

#[test]
fn retained_rendering_feeds_each_source_byte_once() {
    for count in [64, 128, 256, 1024] {
        let (used, bytes) = measure(count, false);
        assert_eq!(used, bytes, "{count} chunks");
    }
}

#[test]
fn rendering_counter_detects_replaying_open_blocks() {
    let small = measure(128, true);
    let large = measure(256, true);
    assert!(small.0 > small.1);
    assert!(large.0 > 2 * small.0 + 4096);
}

#[test]
fn code_and_paragraph_boundaries_match_event_rendering() {
    for body in [
        " a ",
        "   ",
        "\r\na\n",
        "a\r\nb",
        "\n \r\n",
        "\0x",
        "界\n🦀",
    ] {
        let source = format!("x `{body}` z");
        let mut one_shot = crate::Markdown::default();
        one_shot.push(&source);
        assert_eq!(
            one_shot.lines(),
            crate::tests::full_parse(&source),
            "one-shot body {body:?}"
        );
        let mut markdown = crate::Markdown::default();
        let mut consumer = Vec::new();
        for (at, character) in source.char_indices() {
            let changed = markdown.push(&source[at..at + character.len_utf8()]);
            consumer.truncate(changed);
            consumer.extend_from_slice(&markdown.lines()[changed..]);
            assert_eq!(
                consumer,
                crate::tests::full_parse(&source[..at + character.len_utf8()]),
                "body {body:?}, prefix {at}"
            );
        }
    }
}
