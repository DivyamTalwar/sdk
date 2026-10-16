use crate::{
    Line, Style, code,
    style::{BLUE, CYAN, MAGENTA, MUTED, YELLOW},
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};
use std::sync::Arc;

pub(crate) fn render_styled(events: &[Event<'_>], style: Style) -> Vec<Line> {
    let mut out = Renderer {
        style,
        ..Renderer::default()
    };
    for event in events {
        out.event(event);
    }
    out.flush();
    out.lines
}

#[derive(Default)]
pub(crate) struct Renderer {
    pub(crate) lines: Vec<Line>,
    line: Line,
    styles: Vec<(Style, Option<Arc<str>>)>,
    style: Style,
    link: Option<Arc<str>>,
    lists: Vec<Option<u64>>,
    prefixes: Vec<String>,
    code: Option<code::Code>,
    code_text: String,
    table: Option<crate::table::Table>,
}

impl Renderer {
    pub(crate) fn checkpoint(&self) -> Self {
        Self {
            line: self.line.clone(),
            styles: self.styles.clone(),
            style: self.style,
            link: self.link.clone(),
            lists: self.lists.clone(),
            prefixes: self.prefixes.clone(),
            ..Self::default()
        }
    }

    pub(crate) fn flush(&mut self) {
        if !self.line.spans.is_empty() {
            self.lines.push(std::mem::take(&mut self.line));
        }
    }

    fn text(&mut self, text: &str) {
        for part in text.split_inclusive(['\r', '\n']) {
            if self.line.spans.is_empty() {
                self.line.push(&self.prefix(), MUTED, None);
            }
            self.line.push(
                part.trim_end_matches(['\r', '\n']),
                self.style,
                self.link.as_ref(),
            );
            if part.ends_with(['\r', '\n']) {
                if self.line.spans.is_empty() {
                    self.lines.push(Line::default());
                } else {
                    self.flush();
                }
            }
        }
    }

    fn prefix(&self) -> String {
        self.prefixes.concat()
    }

    pub(crate) fn block_line(&mut self, line: Line) {
        let mut prefixed = Line::plain(&self.prefix(), MUTED);
        prefixed.spans.extend(line.spans);
        self.lines.push(prefixed);
    }

    pub(crate) fn event(&mut self, event: &Event<'_>) {
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
            Event::Text(text) => {
                if self.code.is_some() {
                    self.code_text.push_str(text);
                } else {
                    self.text(text);
                }
            }
            Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => self.text(text),
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
                    *indent = "    ".into();
                }
                self.text(if *done { "[✓] " } else { "[ ] " });
            }
            Event::FootnoteReference(text) => self.text(&format!("[{text}]")),
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
                self.link = Some(Arc::from(dest_url.as_ref()));
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
                self.table = Some(crate::table::Table::new(alignments.clone(), self.style));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock => self.flush(),
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
