use super::*;
use proptest::prelude::*;

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
        "```rust\nlet x = 1;\n```\u{a0}\nstill code\n```\nend",
        "```rust\nlet x = 1;\n```\t\nend",
        "[r]\n\n[r]: /url\n\n[r]",
        "[r]: /old\n\n# Heading\n\n[r]: /new\n\n[r]",
        "[SS]: /case\n\n# Heading\n\nparagraph\n\n[ß]",
        "[a b]: /one\n\n[a\u{a0}b]: /two\n\n[a b] [a\u{a0}b]",
        "<div>\nhello\n</div>\n\nend",
        "[a][b]\n\n[a]: /one\n\n[b]: /two\n\nend",
        "- ```rust\n  let x = 1;\n  ```\n- second\n",
        "*a [r] b*\n\n[r]: /url\n\nend",
        "> [r]\n> extra\n>\n\n[r]: /url\n\nend",
        "- [r]\n- other\n\n[r]: /url\n\nend",
        "| [r] | b |\n| - | - |\n| 1 | 2 |\n\n[r]: /url\n\nend",
        "[a][b  \nc]\n\nStable\n\n[b c]: /url\n\nend",
        "- ```text\n  code\n  ```\nend",
        "- - ```text\n    code\n    ```\n    after\n",
        "- > ```text\n  > code\n  > ```\n  > after\n",
        "[r]\n\n- parent\n  - child\n\n    [r]: /nested\n\n    more\n",
        "[r]\n\n> quote\n>\n> [r]: /quoted\n>\n> more\n",
        "```rust\n**bold****bold**    indented\n\n  ",
        "```\n| a | b |\n    ",
        "    indented\n\n    more\n\nend",
        "\tindented\n  \n\tmore\nend",
        "    indented\n      \n    more\nend",
        "- parent\n\n      indented\n\n      more\nend",
        ">     indented\n>\n>     more\nend",
        "paragraph &copy; &copyleft; &amp; word; (word) #hash",
        "paragraph [word](url) (word); [r] (word); [r]: /url",
        "# paragraph word # word #\n\ntext\n1. next\n+ next",
        "[r][x](url)[r]: url\ntext 🦀> quote\n- item\n- item\ntext 🦀\n| a | b |\n| - | - |\n[r]: url\n\n[r]: u",
        "[r][x][y](url) more\n\n[r]: /r\n\n[y]: /y\n\n[x]: /x\n\nend",
        "- item\n<div>\nhello\n</div>\n",
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

#[test]
fn unused_definitions_never_parse_or_highlight_finalized_fences() {
    let mut markdown = Markdown::default();
    markdown.push(&format!("```rust\n{}```\n\n", "let x = 1;\n".repeat(1000)));
    let first = markdown
        .lines()
        .get(1)
        .and_then(|line| line.spans.first())
        .map(|span| span.text.as_ptr());
    let highlighted = code::HIGHLIGHTED.get();
    let before = markdown.parsed_bytes;
    for index in 0..100 {
        markdown.push(&format!("[unused{index}]: /url\n\n"));
    }
    assert_eq!(code::HIGHLIGHTED.get(), highlighted);
    assert!(markdown.parsed_bytes - before < 4096);
    assert_eq!(
        first,
        markdown
            .lines()
            .get(1)
            .and_then(|line| line.spans.first())
            .map(|span| span.text.as_ptr())
    );
}

#[test]
fn completed_nested_fences_have_parser_and_lexer_checkpoints() {
    for lines in [100, 10_000] {
        let source = format!(
            "- parent\n\n  ```rust\n{}  ```\n",
            "  let x = 1;\n".repeat(lines)
        );
        let mut markdown = Markdown::default();
        markdown.push(&source);
        let highlighted = code::HIGHLIGHTED.get();
        let before = markdown.parsed_bytes;
        markdown.push("- next\n");
        assert_eq!(code::HIGHLIGHTED.get(), highlighted);
        assert!(markdown.parsed_bytes - before < 64);
        assert_eq!(markdown.lines(), full_parse(&format!("{source}- next\n")));
    }
}

#[test]
fn long_plain_paragraph_appends_only_visit_the_delta() {
    for length in [1000, 100_000] {
        let mut markdown = Markdown::default();
        markdown.push(&"x".repeat(length));
        let before = markdown.parsed_bytes;
        for delta in ["a", " ", " ", "b", "c"] {
            markdown.push(delta);
        }
        assert_eq!(markdown.parsed_bytes, before);
        assert_eq!(
            markdown.lines(),
            full_parse(&format!("{}a  bc", "x".repeat(length)))
        );
    }
}

#[test]
fn styled_paragraph_text_reuses_the_inline_checkpoint() {
    for prefix in ["**bold** text ", "*unfinished text ", "# heading text "] {
        let mut markdown = Markdown::default();
        markdown.push(&format!("{prefix}{}", "x".repeat(100_000)));
        let before = markdown.parsed_bytes;
        for delta in ["a", " ", " ", "b", "c"] {
            markdown.push(delta);
        }
        assert_eq!(markdown.parsed_bytes, before);
        assert_eq!(
            markdown.lines(),
            full_parse(&format!("{prefix}{}a  bc", "x".repeat(100_000)))
        );
    }
}

#[test]
fn ordinary_append_does_not_visit_committed_reference_definitions() {
    for definitions in [100, 10_000] {
        let mut markdown = Markdown::default();
        for index in 0..definitions {
            markdown.push(&format!("[r{index}]: /url\n\n"));
        }
        markdown.push("unfinished ");
        let before = markdown.parsed_bytes;
        for _ in 0..100 {
            markdown.push("x");
        }
        assert_eq!(markdown.parsed_bytes, before);
    }
    let source: String = (0..10_000)
        .map(|index| format!("[r{index}]: /url\n\n"))
        .collect();
    let mut markdown = Markdown::default();
    markdown.push(&format!("{source}a *unfinished "));
    let before = markdown.parsed_bytes;
    for _ in 0..100 {
        markdown.push("x");
    }
    assert_eq!(markdown.parsed_bytes, before);
    let work = markdown.references.work;
    markdown.push("!");
    assert!(markdown.parsed_bytes - before < 128);
    assert_eq!(markdown.references.work, work);
}

#[test]
fn late_definitions_select_cached_reference_nodes_only() {
    for source in [
        "[r]\n\n",
        "[**bold**][r]\n\n",
        "[a][r]\n\n",
        "![image][r]\n\n",
    ] {
        let mut markdown = Markdown::default();
        markdown.push(source);
        let before = markdown.parsed_bytes;
        let delta = "[r]: /url\n\n";
        markdown.push(delta);
        assert!(markdown.parsed_bytes - before <= delta.len());
        assert_eq!(markdown.lines(), full_parse(&format!("{source}{delta}")));
    }
}

#[test]
fn html_spans_are_physical_display_lines() {
    let mut markdown = Markdown::default();
    markdown.push("<div>\nhello\n</div>\n");
    assert_eq!(markdown.lines().len(), 3);
    assert!(
        markdown
            .lines()
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| !span.text.contains(['\r', '\n']))
    );
    markdown.clear();
    // Script HTML blocks retain interior blank lines.
    markdown.push("<script>\n\nhello\n</script>\n");
    assert_eq!(markdown.lines().len(), 4);
}

#[test]
fn nested_paragraph_checkpoint_context() {
    let source = "- parent\n  - child\n\n    pa";
    let mut markdown = Markdown::default();
    let mut end = 0;
    for character in source.chars() {
        let delta = character.to_string();
        markdown.push(&delta);
        end += delta.len();
        assert_eq!(
            markdown.lines(),
            full_parse(source.get(..end).unwrap_or_default())
        );
    }
    assert_eq!(markdown.lines(), full_parse(source));
}

const LAZY_LIST: &str =
    "> quote\n- item\n- item\ntext 🦀\n| a | b |\n| - | - |\n[r]: url\n\n[r]: u";

fn check_chunks(source: &str, cuts: &[usize]) {
    let mut streamed = Markdown::default();
    let mut offset = 0;
    for size in cuts.iter().cycle() {
        if offset == source.len() {
            break;
        }
        let mut end = (offset + size).min(source.len());
        while !source.is_char_boundary(end) {
            end += 1;
        }
        streamed.push(source.get(offset..end).unwrap_or_default());
        assert_eq!(
            streamed.lines(),
            full_parse(source.get(..end).unwrap_or_default()),
            "prefix {:?}, checkpoint {:?}",
            source.get(..end),
            source.get(..streamed.offset)
        );
        offset = end;
    }
}

#[test]
fn review_regressions_match_every_chunk_boundary() {
    check_chunks(LAZY_LIST, &[2, 2, 1, 1]);
    for source in [
        "    indented\n\n    more",
        "text word, word. yes! x: y - z + 2 = 4? (ok)",
    ] {
        check_chunks(source, &[1]);
    }
}

#[test]
fn punctuation_appends_have_linear_parser_work() {
    for length in [1024, 100 * 1024] {
        let mut markdown = Markdown::default();
        markdown.push(&"x".repeat(length));
        let before = markdown.parsed_bytes;
        for _ in 0..100 {
            markdown.push("word, (word); #yes! ");
        }
        assert_eq!(markdown.parsed_bytes, before);
        assert_eq!(
            markdown.lines(),
            full_parse(&format!(
                "{}{}",
                "x".repeat(length),
                "word, (word); #yes! ".repeat(100)
            ))
        );
    }
}

#[test]
fn lazy_list_reference_updates_never_reparse_finalized_paragraphs() {
    for count in [100, 10_000] {
        let mut source = "- earlier\n".repeat(count);
        source.push_str(
            LAZY_LIST
                .split_once("\n\n")
                .map_or(LAZY_LIST, |(first, _)| first),
        );
        source.push_str("\n\n");
        let mut markdown = Markdown::default();
        markdown.push(&source);
        let before = markdown.parsed_bytes;
        markdown.push("[r]: u\n\n");
        assert!(markdown.parsed_bytes - before <= 16);
        source.push_str("[r]: u\n\n");
        assert_eq!(markdown.lines(), full_parse(&source));
    }
}

#[test]
fn indented_code_retains_lines_and_defers_blank_lines() {
    for count in [100, 10_000] {
        let mut source = "    indented\n".repeat(count);
        let mut markdown = Markdown::default();
        markdown.push(&source);
        let pointer = |markdown: &Markdown| {
            markdown
                .lines()
                .get(1)
                .and_then(|line| line.spans.first())
                .map(|span| span.text.as_ptr())
        };
        let first = pointer(&markdown);
        assert!(first.is_some());
        let before = markdown.parsed_bytes;
        for delta in ["\n", " ", " ", " ", " ", "m", "o", "r", "e", "\n"] {
            let highlighted = code::HIGHLIGHTED.get();
            markdown.push(delta);
            assert!(code::HIGHLIGHTED.get() - highlighted <= 2);
            source.push_str(delta);
            assert_eq!(markdown.lines(), full_parse(&source));
        }
        assert!(markdown.parsed_bytes - before < 512);
        assert_eq!(pointer(&markdown), first);
        let highlighted = code::HIGHLIGHTED.get();
        markdown.push("\n    next\n");
        assert_eq!(code::HIGHLIGHTED.get() - highlighted, 2);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn random_chunks_match_the_full_parser(parts in prop::collection::vec(0usize..20, 1..20), cuts in prop::collection::vec(1usize..20, 1..12)) {
        let vocabulary = ["text 🦀", "**bold**", "\n", "\n\n", "- item\n", "```rust\n", "let x = 1;\n", "```\n", "[x](url)", "| a | b |\n", "| - | - |\n", "[r]: url\n", "[r]", "<div>\nhello\n</div>\n", "> quote\n", "[a\u{a0}b]: /two\n", LAZY_LIST, "    indented\n\n    more", "word, ", "word. yes! x: y - z + 2 = 4? "];
        let source: String = parts.iter().filter_map(|index| vocabulary.get(*index)).copied().collect();
        let mut streamed = Markdown::default();
        let mut offset = 0;
        for size in cuts.iter().cycle() {
            if offset == source.len() { break; }
            let mut end = (offset + size).min(source.len());
            while !source.is_char_boundary(end) { end += 1; }
            streamed.push(source.get(offset..end).unwrap_or_default());
            prop_assert_eq!(streamed.lines(), full_parse(source.get(..end).unwrap_or_default()), "prefix {:?}", source.get(..end));
            offset = end;
        }
    }

    #[test]
    fn review_cases_match_random_chunkings(case in 0usize..3, cuts in prop::collection::vec(1usize..20, 1..12)) {
        let cases = [LAZY_LIST, "    indented\n\n    more", "paragraph word, word. yes! x: y - z + 2 = 4? "];
        check_chunks(cases.get(case).copied().unwrap_or_default(), &cuts);
    }

    #[test]
    fn punctuation_matches_random_chunkings(parts in prop::collection::vec(0usize..12, 1..30), cuts in prop::collection::vec(1usize..12, 1..8)) {
        let punctuation = ["word, ", "(word); ", "#word! ", "'word' : ", "&copy", "; ", "[word]", "(url)", "*word", "* ", "\\", "[r]: /url\n"];
        let mut source = String::from("paragraph ");
        for index in parts { source.push_str(punctuation.get(index).copied().unwrap_or_default()); }
        check_chunks(&source, &cuts);
    }
}
