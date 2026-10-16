use std::cell::Cell;
thread_local! {
    static SOURCE_GROWTH: Cell<usize> = const { Cell::new(0) };
    static SCAN: Cell<usize> = const { Cell::new(0) };
    static COPY: Cell<usize> = const { Cell::new(0) };
    static VISIBILITY: Cell<usize> = const { Cell::new(0) };
    static RENDER_FEED: Cell<usize> = const { Cell::new(0) };
    static RENDER_WORK: Cell<usize> = const { Cell::new(0) };
    static RENDERING: Cell<bool> = const { Cell::new(false) };
}
pub(crate) fn copy(bytes: usize) {
    COPY.set(COPY.get() + bytes);
    render_work(bytes);
}
pub(crate) fn visibility(count: usize) {
    VISIBILITY.set(VISIBILITY.get() + count);
    render_work(count);
}
pub(crate) fn visibility_current() -> usize {
    VISIBILITY.get()
}
pub(crate) fn current() -> usize {
    SCAN.get()
        + COPY.get()
        + VISIBILITY.get()
        + RENDER_FEED.get()
        + crate::references::PARSED_BYTES.get()
}

pub(crate) fn render_feed(bytes: usize) {
    RENDER_FEED.set(RENDER_FEED.get() + bytes);
    render_work(bytes);
}

pub(crate) fn rendered_bytes() -> usize {
    RENDER_FEED.get()
}

fn render_work(work: usize) {
    if RENDERING.get() {
        RENDER_WORK.set(RENDER_WORK.get() + work);
    }
}

pub(crate) fn rendering(active: bool) -> bool {
    RENDERING.replace(active)
}

pub(crate) fn rendering_work() -> usize {
    RENDER_WORK.get()
}

pub(crate) fn event(event: &pulldown_cmark::Event<'_>) {
    use pulldown_cmark::{CodeBlockKind, Event, Tag};
    copy(match event {
        Event::Text(text)
        | Event::Code(text)
        | Event::Html(text)
        | Event::InlineHtml(text)
        | Event::InlineMath(text)
        | Event::DisplayMath(text)
        | Event::FootnoteReference(text) => text.len(),
        Event::Start(
            Tag::Link {
                dest_url,
                title,
                id,
                ..
            }
            | Tag::Image {
                dest_url,
                title,
                id,
                ..
            },
        ) => dest_url.len() + title.len() + id.len(),
        Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => info.len(),
        _ => 0,
    });
}

pub(crate) fn source_growth() -> usize {
    SOURCE_GROWTH.get()
}
pub(crate) fn grow_source(bytes: usize) {
    copy(bytes);
    SOURCE_GROWTH.set(SOURCE_GROWTH.get() + bytes);
}

pub(crate) fn scan(bytes: usize) {
    SCAN.set(SCAN.get() + bytes);
    render_work(bytes);
}
