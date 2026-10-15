use crate::{
    Line, Style, code,
    style::{BLUE, CYAN, MAGENTA, MUTED, YELLOW},
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};

pub(crate) fn render(events: &[Event<'_>]) -> Vec<Line> {
    let mut out = Renderer::default();
    for event in events {
        out.event(event);
    }
    out.flush();
    out.lines
}

#[derive(Default)]
struct Renderer {
    lines: Vec<Line>,
    line: Line,
    styles: Vec<(Style, Option<String>)>,
    style: Style,
    link: Option<String>,
    lists: Vec<Option<u64>>,
    prefixes: Vec<String>,
    code: Option<code::Code>,
    code_text: String,
    table: Option<crate::table::Table>,
}

impl Renderer {
    fn flush(&mut self) {
        if !self.line.spans.is_empty() {
            self.lines.push(std::mem::take(&mut self.line));
        }
    }

    fn text(&mut self, text: &str) {
        if self.line.spans.is_empty() {
            self.line.push(&self.prefix(), MUTED, None);
        }
        self.line.push(text, self.style, self.link.as_deref());
    }

    fn prefix(&self) -> String {
        self.prefixes.concat()
    }

    fn block_line(&mut self, line: Line) {
        let mut prefixed = Line::plain(&self.prefix(), MUTED);
        prefixed.spans.extend(line.spans);
        self.lines.push(prefixed);
    }

    fn event(&mut self, event: &Event<'_>) {
        if let Some(table) = &mut self.table {
            if matches!(event, Event::End(TagEnd::Table)) {
                if let Some(table) = self.table.take() {
                    for line in table.render() {
                        self.block_line(line);
                    }
                }
                if let Some((style, link)) = self.styles.pop() {
                    self.style = style;
                    self.link = link;
                }
            } else {
                table.event(event);
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(*tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                if self.code.is_some() {
                    self.code_text.push_str(text);
                } else {
                    self.text(text);
                }
            }
            Event::Code(text) => {
                let previous = self.style;
                self.style.foreground = Some(YELLOW);
                self.text(text);
                self.style = previous;
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.block_line(Line::plain(&"─".repeat(40), MUTED));
            }
            Event::TaskListMarker(done) => {
                // Replace the bullet, retaining any nesting indentation.
                if let Some(marker) = self.line.spans.last_mut() {
                    marker.text.clear();
                }
                if let Some(indent) = self.prefixes.last_mut() {
                    indent.push_str("  ");
                }
                self.text(if *done { "[✓] " } else { "[ ] " });
            }
            Event::FootnoteReference(text) => self.text(&format!("[{text}]")),
            Event::InlineMath(text) | Event::DisplayMath(text) => self.text(text),
        }
    }

    fn start(&mut self, tag: &Tag<'_>) {
        self.styles.push((self.style, self.link.clone()));
        match tag {
            Tag::Heading { level, .. } => {
                self.flush();
                self.style.foreground =
                    Some(if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                        CYAN
                    } else {
                        BLUE
                    });
                self.style.bold = matches!(level, HeadingLevel::H1 | HeadingLevel::H5);
                self.text(match level {
                    HeadingLevel::H1 | HeadingLevel::H2 => "# ",
                    HeadingLevel::H3 | HeadingLevel::H4 => "## ",
                    _ => "### ",
                });
            }
            Tag::Emphasis => self.style.italic = true,
            Tag::Strong => self.style.bold = true,
            Tag::Strikethrough => {
                self.style.dim = true;
                self.style.strikethrough = true;
            }
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.style.foreground = Some(BLUE);
                self.style.underline = true;
                self.link = Some(dest_url.to_string());
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.prefixes.push("│ ".into());
                self.style.italic = true;
                self.style.dim = true;
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(*start);
            }
            Tag::Item => {
                self.flush();
                self.line.push(&self.prefix(), MUTED, None);
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}. ");
                        *number += 1;
                        marker
                    }
                    _ => "• ".into(),
                };
                self.line.push(
                    &marker,
                    Style {
                        foreground: Some(MAGENTA),
                        ..Style::default()
                    },
                    None,
                );
                self.prefixes.push(" ".repeat(marker.chars().count()));
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or_default()
                    }
                    CodeBlockKind::Indented => "",
                };
                self.block_line(code::header(language));
                self.code = Some(code::highlighter(language));
            }
            Tag::Table(alignments) => {
                self.flush();
                self.table = Some(crate::table::Table::new(alignments.clone()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.flush(),
            TagEnd::Item | TagEnd::BlockQuote(_) => {
                self.flush();
                self.prefixes.pop();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::CodeBlock => {
                if let Some(mut code) = self.code.take() {
                    let text = std::mem::take(&mut self.code_text);
                    for line in text.split_inclusive('\n') {
                        self.block_line(code::highlight(&mut code, line));
                    }
                }
                self.code_text.clear();
                self.block_line(code::footer());
            }
            _ => {}
        }
        if let Some((style, link)) = self.styles.pop() {
            self.style = style;
            self.link = link;
        }
    }
}
