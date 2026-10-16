use acyclic_harness::{
    OperationId, client,
    local::LocalHarness,
    tui::{
        self, PromptBox, PromptInput, Viewport,
        key_event::InputEvent,
        ratatui::{Terminal, backend::CrosstermBackend},
        term::{Environment, ScreenMode, Tty},
    },
};
use futures::StreamExt;
use std::{sync::Arc, time::Instant};

pub async fn run(
    local: Arc<LocalHarness>,
    operation: OperationId,
    inline: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let authority = local.client_owner();
    let mut client =
        client::TaskFileClient::observe(local.task_files(), &authority, 4096, 65_536).await?;
    let reader = local.client_reader()?;
    let mut commands = client::local_ui(&local, &operation.to_string(), &authority).await?;
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
    let mut prompt_files = client::PromptFiles::default();
    prompt_files.refresh(&commands, &mut input)?;
    let mut completions = client::local_completions(&local, &mut input).await?;
    let mut viewport = Viewport::default();
    let mut renderer = tui::ClientRenderer::default();
    let start = Instant::now();
    let mut documents = client::local_documents(&local, &authority).await?;
    renderer.update(client.views(), &authority);
    loop {
        if commands.exit_requested()? {
            return Ok(());
        }
        prompt_files.refresh(&commands, &mut input)?;
        input.busy = renderer.is_busy();
        input.refresh_completions();
        let mut areas = tui::ScreenAreas::default();
        let now = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let back = client::tui_scroll(&commands)?;
        let mut maximum_scroll = 0;
        renderer.configure(client.views(), &authority, &commands)?;
        let size = terminal.size()?;
        renderer.layout(&commands, &input, size.width, size.height)?;
        let screen = renderer.screen_for(&commands)?;
        terminal.draw(|frame| {
            maximum_scroll = screen.tail_scroll(frame.area(), &input);
            let scroll = maximum_scroll.saturating_sub(back);
            areas = screen.render(frame, &input, &mut viewport, scroll, now);
        })?;
        let Some(event) = client::next_file_tui_event(
            &mut client,
            &mut events,
            &mut completions,
            &mut input,
            &mut commands,
            &mut documents,
        )
        .await?
        else {
            renderer.update(client.views(), &authority);
            continue;
        };
        let now = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let effects = input.event(
            event,
            PromptBox::input_area(areas.prompt),
            &mut viewport,
            now,
        );
        for effect in effects {
            client::file_tui_input(
                &mut commands,
                reader.as_ref(),
                effect,
                usize::from(areas.transcript.height),
                maximum_scroll,
            )
            .await?;
        }
        prompt_files.save(&mut commands, &mut input).await?;
    }
}
