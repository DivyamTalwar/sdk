use super::{
    Screen,
    ratatui::{
        style::{Color, Modifier, Style},
        text::{Line, Span},
    },
};
use crate::{client::ClientViews, wire};
use acyclic_markdown::Markdown;

#[derive(Default)]
struct StreamText {
    markdown: Markdown,
    fed: usize,
}

impl StreamText {
    fn push(&mut self, text: &str) -> usize {
        if text.len() < self.fed {
            *self = Self::default();
        }
        let delta = text.get(self.fed..).unwrap_or_default();
        self.fed = text.len();
        self.markdown.push(delta)
    }
}

/// Disposable incremental render cache. Authoritative state is always ClientViews
/// and the followed files; this cache is never serialized or written back.
#[derive(Default)]
pub struct ClientRenderer {
    key: String,
    seen: usize,
    stable: usize,
    turn: Option<crate::OperationId>,
    stream: StreamText,
    transcript: Vec<Line<'static>>,
    goal_source: String,
    goal: Vec<Line<'static>>,
    plan_source: String,
    plans: Vec<Line<'static>>,
    header: Line<'static>,
    status: Line<'static>,
    notices: Vec<Line<'static>>,
}

impl ClientRenderer {
    /// Update only the active agent. Stable transcript rows survive text deltas;
    /// the Markdown parser supplies the first changed open-block row.
    pub fn update(&mut self, views: &ClientViews, authority: &wire::Authority) {
        let key = crate::client::views::authority_key(authority);
        if self.key != key {
            *self = Self {
                key: key.clone(),
                ..Self::default()
            };
        }
        if let Some(transcript) = views.transcripts.get(&key) {
            if transcript.messages.len() < self.seen {
                self.seen = 0;
                self.stable = 0;
            }
            if transcript.messages.len() != self.seen {
                self.transcript.truncate(self.stable);
                for message in &transcript.messages[self.seen..] {
                    self.transcript.push(Line::styled(
                        format!("{:?}", message.kind),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                    if let Some(text) = transcript.text.get(&message.id) {
                        self.transcript.extend(markdown(text));
                    } else if let Some(call) = message
                        .tool_call_id
                        .as_ref()
                        .and_then(|id| views.tools.get(&key)?.get(id))
                    {
                        self.transcript
                            .extend(markdown(&format!("{}\n{:?}", call.name, call.output)));
                    }
                }
                self.seen = transcript.messages.len();
                self.stable = self.transcript.len();
                self.stream = StreamText::default();
            }
            if self.turn != transcript.streaming_turn {
                self.turn = transcript.streaming_turn;
                self.stream = StreamText::default();
                self.transcript.truncate(self.stable);
            }
            let changed = self.stream.push(&transcript.streaming_text);
            self.transcript.truncate(self.stable + changed);
            self.transcript
                .extend(self.stream.markdown.lines().iter().skip(changed).map(line));
        } else {
            self.seen = 0;
            self.stable = 0;
            self.turn = None;
            self.stream = StreamText::default();
            self.transcript.clear();
        }
        if let Some(documents) = views.documents.get(&key) {
            let goal = documents.goal.as_deref().unwrap_or_default();
            if self.goal_source != goal {
                self.goal = markdown(goal);
                self.goal_source = goal.into();
            }
            let plans = documents
                .plans
                .iter()
                .map(|(name, text)| format!("# {}\n{text}", name.as_str()))
                .collect::<Vec<_>>()
                .join("\n\n");
            if self.plan_source != plans {
                self.plans = markdown(&plans);
                self.plan_source = plans;
            }
        } else {
            self.goal_source.clear();
            self.goal.clear();
            self.plan_source.clear();
            self.plans.clear();
        }
        let task = views.tasks.get(&key);
        let busy = task.is_some_and(|task| task.status == crate::core::LifecycleState::Active);
        self.header = Line::from(vec![
            Span::raw(if busy { "● " } else { "○ " }),
            Span::styled("GraphCoder", Style::default().add_modifier(Modifier::BOLD)),
        ]);
        self.status = Line::raw(format!(
            "Esc · Ctrl+D quit · {}",
            views
                .usage
                .get(&key)
                .map_or(String::new(), ToString::to_string)
        ));
        self.notices = task
            .and_then(|task| task.reason.as_deref())
            .map(|text| vec![Line::raw(text.to_owned())])
            .unwrap_or_default();
    }

    /// Borrow cached rows for layout; no ClientViews or transcript clone per frame.
    pub fn screen(&self) -> Screen<'_> {
        // TODO(#562): bind the captured-volume diff widget.
        Screen {
            header: self.header.clone(),
            transcript: &self.transcript,
            goal: &self.goal,
            plans: &self.plans,
            notifications: &self.notices,
            status: self.status.clone(),
            ..Screen::default()
        }
    }
}

fn markdown(text: &str) -> Vec<Line<'static>> {
    let mut markdown = Markdown::default();
    markdown.push(text);
    markdown.lines().iter().map(line).collect()
}

fn line(line: &acyclic_markdown::Line) -> Line<'static> {
    Line::from(
        line.spans
            .iter()
            .map(|span| {
                let mut style = Style::default();
                if let Some(acyclic_markdown::Color(r, g, b)) = span.style.foreground {
                    style = style.fg(Color::Rgb(r, g, b));
                }
                for (set, modifier) in [
                    (span.style.bold, Modifier::BOLD),
                    (span.style.italic, Modifier::ITALIC),
                    (span.style.dim, Modifier::DIM),
                    (span.style.underline, Modifier::UNDERLINED),
                    (span.style.strikethrough, Modifier::CROSSED_OUT),
                ] {
                    if set {
                        style = style.add_modifier(modifier);
                    }
                }
                // TODO(#604): emit queried OSC 8 link metadata through the terminal backend.
                Span::styled(span.text.clone(), style)
            })
            .collect::<Vec<_>>(),
    )
}
