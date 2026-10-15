//! Renderer-independent presentation values.

/// An RGB foreground color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8);

/// Additive text attributes, independent of a terminal or browser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// Foreground; absent means the renderer's default.
    pub foreground: Option<Color>,
    /// Strong emphasis.
    pub bold: bool,
    /// Emphasis.
    pub italic: bool,
    /// Muted text.
    pub dim: bool,
    /// Link decoration.
    pub underline: bool,
    /// Deleted text.
    pub strikethrough: bool,
}

/// Text with a style and optional link destination.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    /// Visible text, with Markdown delimiters removed.
    pub text: String,
    /// Presentation attributes.
    pub style: Style,
    /// Link target, retained independently of the visible label.
    pub link: Option<String>,
}

/// One unwrapped display line. The renderer owns viewport layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    /// Ordered styled runs.
    pub spans: Vec<Span>,
}

impl Line {
    pub(crate) fn push(&mut self, text: &str, style: Style, link: Option<&str>) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.style == style
            && last.link.as_deref() == link
        {
            last.text.push_str(text);
        } else {
            self.spans.push(Span {
                text: text.into(),
                style,
                link: link.map(str::to_owned),
            });
        }
    }

    pub(crate) fn plain(text: &str, style: Style) -> Self {
        let mut line = Self::default();
        line.push(text, style, None);
        line
    }
}

pub(crate) const MUTED: Style = Style {
    dim: true,
    ..Style::DEFAULT
};
pub(crate) const BLUE: Color = Color(97, 175, 239);
pub(crate) const CYAN: Color = Color(86, 182, 194);
pub(crate) const YELLOW: Color = Color(229, 192, 123);
pub(crate) const MAGENTA: Color = Color(198, 120, 221);

impl Style {
    pub(crate) const DEFAULT: Self = Self {
        foreground: None,
        bold: false,
        italic: false,
        dim: false,
        underline: false,
        strikethrough: false,
    };
}
