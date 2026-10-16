use super::{
    Screen,
    ratatui::{
        style::{Color, Modifier, Style},
        text::{Line, Span},
    },
};
use crate::{
    client::ClientViews,
    model::{ModelDataPart, ToolResultContent},
    wire,
};
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
    busy: bool,
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
    notice_file: Option<crate::task_volume::FileValue>,
    file_notices: Vec<Line<'static>>,
    composed_notices: Vec<Line<'static>>,
    diff_cut: Option<(
        crate::conversation::VolumeRef,
        crate::resources::GenerationRef,
        crate::resources::GenerationRef,
    )>,
    diff: Vec<Line<'static>>,
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
                        self.transcript.extend(tool_rows(&call.name, &call.output));
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
        if let Some(tools) = views.tools.get(&key) {
            for tool in tools.values().filter(|tool| tool.result.is_none()) {
                self.transcript.extend(tool_rows(&tool.name, &tool.output));
            }
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
        let diff = views.diffs.get(&key);
        let cut = diff.map(|view| (view.volume(), view.base(), view.generation()));
        if self
            .diff_cut
            .as_ref()
            .map(|(volume, base, generation)| (volume, base, generation))
            != cut
        {
            self.diff = diff.map_or_else(Vec::new, |view| super::diff_lines(view.files()));
            self.diff_cut = cut.map(|(volume, base, generation)| {
                (volume.clone(), base.clone(), generation.clone())
            });
        }
        let task = views.tasks.get(&key);
        self.busy = task.is_some_and(|task| task.status == crate::core::LifecycleState::Active);
        self.header = Line::from(vec![
            Span::raw(if self.busy { "● " } else { "○ " }),
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
        self.compose_notices();
    }

    fn compose_notices(&mut self) {
        self.composed_notices.clone_from(&self.notices);
        self.composed_notices.extend_from_slice(&self.file_notices);
    }

    /// Current followed lifecycle, used by the shared prompt's queue indicator.
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Compose the panel selected by the followed command layout file.
    /// Unknown views retain the document overview until their widget is available.
    pub fn screen_for<P: acyclic_stream::StreamProvider + ?Sized>(
        &mut self,
        files: &crate::client::CommandSession<P>,
    ) -> crate::Result<Screen<'_>> {
        use crate::client::CommandFileRoot;
        let layout = files.state(CommandFileRoot::Session, "layout.json")?;
        let feedback = files.state_file(CommandFileRoot::Session, "notifications.json")?;
        if self.notice_file.as_ref() != feedback {
            let messages: Vec<String> = feedback
                .map(|file| serde_json::from_slice(&file.bytes))
                .transpose()
                .map_err(|error| crate::Error::Invalid(error.to_string()))?
                .unwrap_or_default();
            self.file_notices = messages
                .into_iter()
                .map(|message| Line::raw(message.escape_debug().to_string()))
                .collect();
            self.notice_file = feedback.cloned();
            self.compose_notices();
        }
        let mut screen = self.screen();
        match layout
            .as_ref()
            .and_then(|value| value.get("open_view"))
            .and_then(serde_json::Value::as_str)
        {
            Some("goal") => {
                screen.diff = &[];
                screen.plans = &[];
            }
            Some("plan") => {
                screen.diff = &[];
                screen.goal = &[];
            }
            _ => {}
        }
        Ok(screen)
    }

    /// Borrow cached rows for layout; no ClientViews or transcript clone per frame.
    pub fn screen(&self) -> Screen<'_> {
        // TODO(#495): bind the captured-volume snapshot/follow to the client diff fold.
        // TODO(#548): compose split/auto diffs when that renderer is exposed.
        Screen {
            header: self.header.clone(),
            transcript: &self.transcript,
            diff: &self.diff,
            goal: &self.goal,
            plans: &self.plans,
            notifications: &self.composed_notices,
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

fn tool_rows(name: &str, output: &ToolResultContent) -> Vec<Line<'static>> {
    // TODO(#548): use specialized per-tool renderers when their API lands.
    let text = match output {
        ToolResultContent::Json { value } => value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
        ToolResultContent::Parts { parts } => parts
            .iter()
            .map(|part| match part {
                ModelDataPart::Text { text } => text.as_str(),
                ModelDataPart::File { .. } => "[attachment]",
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if text.is_empty() {
        return Vec::new();
    }
    let bounded = text.lines().take(20).collect::<Vec<_>>().join("\n");
    let mut rows = vec![Line::styled(
        name.to_owned(),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    rows.extend(markdown(&bounded).into_iter().take(20));
    rows
}
