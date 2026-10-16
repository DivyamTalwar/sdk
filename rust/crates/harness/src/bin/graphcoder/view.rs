use acyclic_harness::{
    client::{self, FileClientState, HarnessClient},
    local::LocalHarness,
    tui::{
        self, InputEffect, PromptBox, PromptInput, Viewport,
        key_event::InputEvent,
        ratatui::{Terminal, backend::CrosstermBackend},
        term::{Environment, ScreenMode, Tty},
    },
    wire,
};
use futures::StreamExt;
use std::{sync::Arc, time::Instant};

pub async fn run(
    local: Arc<LocalHarness>,
    mut client: HarnessClient<FileClientState>,
    target: wire::ObserveRequest,
    inline: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let authority = target.owner.ok_or("client target has no owner")?;
    let mut tty = Tty::open()?;
    tty.negotiate(&Environment::capture())?;
    tty.setup(if inline {
        ScreenMode::Inline
    } else {
        ScreenMode::Alternate
    })?;
    let mut terminal = Terminal::new(CrosstermBackend::new(tty))?;
    // TODO(#604): consume the Tty decoder's async controlling-terminal input,
    // including negotiation-buffered keys and Windows console input handles.
    let mut events = Box::pin(tui::crossterm::event::EventStream::new().filter_map(
        |event| async move {
            use tui::crossterm::event::Event;
            match event {
                Ok(Event::Key(key)) => Some(Ok(InputEvent::Key(key))),
                Ok(Event::Mouse(mouse)) => Some(Ok(InputEvent::Mouse(mouse))),
                Ok(Event::Paste(text)) => Some(Ok(InputEvent::Paste(text))),
                Ok(Event::Resize(w, h)) => Some(Ok(InputEvent::Resize(w, h))),
                Ok(Event::FocusGained) => Some(Ok(InputEvent::Focus(true))),
                Ok(Event::FocusLost) => Some(Ok(InputEvent::Focus(false))),
                Err(error) => Some(Err(acyclic_harness::Error::Storage(error.to_string()))),
            }
        },
    ));
    let mut input = PromptInput::default();
    let mut viewport = Viewport::default();
    let mut renderer = tui::ClientRenderer::default();
    let start = Instant::now();
    client.replay().await?;
    renderer.update(client.session().views(), &authority);
    loop {
        let mut areas = tui::ScreenAreas::default();
        let now = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        terminal.draw(|frame| {
            areas = renderer
                .screen()
                .render(frame, &input, &mut viewport, 0, now);
        })?;
        let Some(event) = client::next_tui_event(&mut client, &mut events).await? else {
            renderer.update(client.session().views(), &authority);
            continue;
        };
        let effects = match event {
            InputEvent::Key(key) => {
                input.key(key, now, usize::from(areas.prompt.width.saturating_sub(2)))
            }
            InputEvent::Paste(text) => input.paste(&text, now),
            InputEvent::CommittedText(text) => input.committed_text(&text, now),
            InputEvent::Mouse(mouse) => input.mouse(
                mouse,
                PromptBox::input_area(areas.prompt),
                &mut viewport,
                now,
            ),
            InputEvent::Resize(_, _) | InputEvent::Focus(_) => Vec::new(),
        };
        for effect in effects {
            if effect == InputEffect::Quit {
                return Ok(());
            }
            client::local_input(&local, &mut client, effect).await?;
        }
        client::local_draft(&local, &input).await?;
    }
}
