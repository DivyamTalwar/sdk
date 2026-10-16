use super::*;
use wasm_bindgen::closure::Closure;

pub(super) struct Publications {
    channel: web_sys::BroadcastChannel,
    pub(super) changed: crate::change::Change,
    _receive: Closure<dyn FnMut(web_sys::MessageEvent)>,
}
impl Publications {
    pub(super) fn new(name: &str) -> Result<Self, StreamError> {
        let channel = web_sys::BroadcastChannel::new(&format!("acyclic.stream.publication:{name}"))
            .map_err(backend)?;
        let changed = crate::change::Change::new();
        let notify = changed.clone();
        let receive = Closure::new(move |_: web_sys::MessageEvent| {
            let _ = notify.notify();
        });
        channel.set_onmessage(Some(receive.as_ref().unchecked_ref()));
        Ok(Self {
            channel,
            changed,
            _receive: receive,
        })
    }
    pub(super) fn publish(&self) -> Result<(), StreamError> {
        self.changed.notify()?;
        self.channel
            .post_message(&JsValue::UNDEFINED)
            .map_err(backend)
    }
}
impl Drop for Publications {
    fn drop(&mut self) {
        self.channel.set_onmessage(None);
        self.channel.close();
    }
}
impl BrowserStream {
    pub(super) async fn discover(
        &self,
        request: ChildrenRequest,
    ) -> Result<ChildStream, StreamError> {
        crate::memory::validate_limit(request.limit)?;
        // Subscribe before refreshing IndexedDB, closing the snapshot/follow race.
        let changed = self.inner.publications.changed.subscribe()?;
        Ok(stream::try_unfold(
            (self.clone(), request.parent, None, changed),
            |(provider, parent, mut previous, mut changed)| async move {
                let path = changed
                    .next(async || {
                        provider
                            .with_provider(async |memory| {
                                memory.next_child(parent.as_ref(), previous.as_ref()).await
                            })
                            .await
                    })
                    .await?;
                previous = Some(path.clone());
                Ok(Some((
                    Child { path },
                    (provider, parent, previous, changed),
                )))
            },
        )
        .boxed_local())
    }
}
