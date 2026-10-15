use super::*;

fn update(source: &str, definition: &str) -> (usize, usize) {
    let mut markdown = Markdown::default();
    markdown.push(source);
    let cached = markdown.cached.len();
    let lines = markdown.lines.len();
    let visits = markdown.reference_cache_visits;
    let moved = markdown.reference_moved_lines;
    markdown.push(definition);
    let visits = markdown.reference_cache_visits - visits;
    let moved = markdown.reference_moved_lines - moved;
    assert!(
        visits <= 2 * cached && moved <= lines,
        "{cached} caches / {lines} lines: {visits} cache visits, {moved} moved lines"
    );
    assert_eq!(
        markdown.lines(),
        full_parse(&format!("{source}{definition}"))
    );
    for pair in markdown.cached.windows(2) {
        if let [before, after] = pair {
            assert!(before.lines.end <= after.lines.start);
        }
    }
    (visits, moved)
}

#[test]
fn batch_maintenance_scales_linearly() {
    let mut previous = None;
    for count in [2048, 4096] {
        let source = "[x][a&#10;b]\n\n".repeat(count);
        let work = update(&source, "[a&#10;b]: /url\n\n");
        println!(
            "{count} blocks: {} cache visits, {} moved lines",
            work.0, work.1
        );
        if let Some((visits, moved)) = previous {
            assert!(work.0 * 10 <= visits * 22);
            assert!(work.1 * 10 <= moved * 22);
        }
        previous = Some(work);
    }
}

#[test]
fn simultaneous_replacements_preserve_unaffected_caches_and_task_context() {
    let source = "[x][a&#10;b]\n\n[untouched]\n\n- [x] [x][c&#10;d]\n\n[x][a&#10;b]\n\n";
    let definitions = "[a&#10;b]: /a\n\n[c&#10;d]: /c\n\n[untouched]: /u\n\n";
    check_chunks(&format!("{source}{definitions}"), &[1, 7, 31]);
    update(source, definitions);
    update(
        "![][a&#10;b]\n\n[unaffected]\n\n![][a&#10;b]\n\n",
        "[a&#10;b]: /empty\n\n",
    );
}

#[test]
fn batch_replacements_preserve_unchanged_line_allocations() {
    let mut markdown = Markdown::default();
    markdown.push("[x][a&#10;b]\n\n**unaffected**\n\n[x][a&#10;b]\n\n");
    let pointer = |markdown: &Markdown| {
        markdown
            .lines()
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.text == "unaffected")
            .map(|span| span.text.as_ptr())
    };
    let before = pointer(&markdown);
    assert!(before.is_some());
    markdown.push("[a&#10;b]: /url\n\n");
    assert_eq!(pointer(&markdown), before);
    let visits = markdown.reference_cache_visits;
    markdown.push("[unused]: /url\n\n");
    assert_eq!(markdown.reference_cache_visits, visits);
    assert_eq!(pointer(&markdown), before);
}

#[test]
fn one_batch_can_expand_and_shrink_different_blocks() {
    let source = "[outer [x][ref]](u&#10;v)\n\n[x][a&#10;b]\n\n[keep]\n\n";
    let definitions = "[ref]: /r\n\n[a&#10;b]: /a\n\n[keep]: /k\n\n";
    let mut markdown = Markdown::default();
    markdown.push(source);
    let old = markdown.cached.first().map(|cache| cache.lines.len());
    let lines = markdown.lines.len();
    markdown.push(definitions);
    assert!(markdown.cached.first().map(|cache| cache.lines.len()) > old);
    assert_eq!(markdown.lines.len(), lines);
    assert_eq!(
        markdown.lines(),
        full_parse(&format!("{source}{definitions}"))
    );
    check_chunks(&format!("{source}{definitions}"), &[1, 3, 7]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn reference_resize_work_is_linear(
        count in 1usize..64,
        affected in prop::collection::vec(any::<bool>(), 1..8),
        entity in prop_oneof![Just("&#10;"), Just("&#13;"), Just("&NewLine;")],
        image in any::<bool>(),
    ) {
        let mut previous = None;
        for copies in [1, 2] {
            let mut source = String::new();
            for index in 0..count * copies {
                if index == 0 || affected.get(index % affected.len()).copied().unwrap_or(false) {
                    source.push_str(&format!("{}[x][a{entity}b]\n\n", if image { "!" } else { "" }));
                } else { source.push_str("[unaffected]\n\n"); }
            }
            let work = update(&source, &format!("[a{entity}b]: /url\n\n"));
            if let Some((visits, moved)) = previous {
                prop_assert!(work.0 * 10 <= visits * 22, "{visits} -> {} cache visits", work.0);
                prop_assert!(work.1 * 10 <= moved * 22, "{moved} -> {} moved lines", work.1);
            }
            previous = Some(work);
        }
    }
}
