use super::*;

fn full_parse(source: &str) -> Vec<Line> {
    // CommonMark normalizes CRLF and CR to LF before block parsing.
    let source = source.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::new();
    for (_, events) in blocks(Parser::new_ext(&source, options()).into_offset_iter()) {
        lines.extend(render::render(&events));
        lines.push(Line::default());
    }
    while lines.last().is_some_and(|line| line.spans.is_empty()) {
        lines.pop();
    }
    lines
}

#[test]
fn all_newline_forms_render_the_same_code_and_following_paragraph() {
    let lf = "```rust\nlet x = 1;\n```\n\nend";
    let expected = full_parse(lf);
    let text = expected
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(text, "┌─ rust\n│ let x = 1;\n└─\n\nend");
    for newline in ["\n", "\r", "\r\n"] {
        let mut markdown = Markdown::default();
        for character in lf.replace('\n', newline).chars() {
            markdown.push(&character.to_string());
        }
        assert_eq!(markdown.lines(), expected);
    }
}

#[test]
fn incremental_frontier_matches_full_commonmark_parse() {
    for source in [
        include_str!("fixtures/grammar.md"),
        include_str!("fixtures/selection.md"),
        "  ~~~~rust\n  let café = 1;\n  ~~~~\n\nend",
        "```rust\r\n/* comment\r\nstill */\r\n```\r\nend",
        "```rust\r/* comment\rstill */\r```\rend",
        "```rust\nlet x = 1;\n````\n\nend",
        "```rust\nlet x = 1;\n``` trailing\n```\nend",
        "[r]\n\n[r]: /url\n\n[r]",
        "[r]: /old\n\n# Heading\n\n[r]: /new\n\n[r]",
        "[SS]: /case\n\n# Heading\n\nparagraph\n\n[ß]",
    ] {
        let mut markdown = Markdown::default();
        let mut end = 0;
        for character in source.chars() {
            let delta = character.to_string();
            markdown.push(&delta);
            end += delta.len();
            let prefix = source.get(..end).unwrap_or_default();
            assert_eq!(markdown.lines(), full_parse(prefix), "prefix {prefix:?}");
        }
    }
}
