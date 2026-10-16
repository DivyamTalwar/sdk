#![no_main]

use acyclic_markdown::Markdown;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let mut streamed = Markdown::default();
    let mut offset = 0;
    let mut chunk = 0;
    while offset < source.len() {
        let size = usize::from(data.get(chunk % data.len().max(1)).copied().unwrap_or(0)) % 31 + 1;
        let mut end = (offset + size).min(source.len());
        while !source.is_char_boundary(end) {
            end += 1;
        }
        streamed.push(source.get(offset..end).unwrap_or_default());
        let mut one_shot = Markdown::default();
        one_shot.push(source.get(..end).unwrap_or_default());
        assert_eq!(streamed.lines(), one_shot.lines());
        assert!(
            streamed
                .lines()
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| !span.text.contains(['\n', '\r']))
        );
        offset = end;
        chunk += 1;
    }
    let expected = streamed.lines().to_vec();
    let serialized = streamed.into_source();
    assert_eq!(serialized, source.replace("\r\n", "\n").replace('\r', "\n"));
    let mut replay = Markdown::default();
    replay.push(&serialized);
    assert_eq!(replay.lines(), expected);
    assert_eq!(replay.into_source(), serialized);
});
