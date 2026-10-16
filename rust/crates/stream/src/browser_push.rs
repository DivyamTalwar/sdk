//! Origin-local publication hints; durable bytes remain solely in IndexedDB.
use crate::{StreamError, query_sync::watch};
use js_sys::{Array, Function, Object, Reflect};
use std::rc::Rc;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

pub(super) struct Publications {
    channel: Object,
    published: Rc<watch::Sender<()>>,
    _callback: Closure<dyn FnMut(JsValue)>,
}

fn method(channel: &Object, name: &str) -> Result<Function, StreamError> {
    Reflect::get(channel, &JsValue::from_str(name))
        .map_err(|_| StreamError::Unavailable)?
        .dyn_into()
        .map_err(|_| StreamError::Unavailable)
}

impl Publications {
    pub(super) fn open(name: &str) -> Result<Self, StreamError> {
        let constructor: Function =
            Reflect::get(&js_sys::global(), &JsValue::from_str("BroadcastChannel"))
                .map_err(|_| StreamError::Unavailable)?
                .dyn_into()
                .map_err(|_| StreamError::Unavailable)?;
        let channel = Reflect::construct(
            &constructor,
            &Array::of1(&JsValue::from_str(&format!("acyclic.stream.v1:{name}"))),
        )
        .map_err(|_| StreamError::Unavailable)?
        .dyn_into::<Object>()
        .map_err(|_| StreamError::Unavailable)?;
        let published = Rc::new(watch::channel(()).0);
        let sender = published.clone();
        // Hints carry no data or authority: a follow always reopens its checked
        // cursor against the committed database. Coalescing cannot lose a record.
        let callback = Closure::new(move |_: JsValue| {
            sender.send_replace(());
        });
        Reflect::set(&channel, &JsValue::from_str("onmessage"), callback.as_ref())
            .map_err(|_| StreamError::Unavailable)?;
        Ok(Self {
            channel,
            published,
            _callback: callback,
        })
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<()> {
        self.published.subscribe()
    }

    fn notify(&self) {
        self.published.send_replace(());
        if let Ok(post) = method(&self.channel, "postMessage") {
            let _ = post.call1(&self.channel, &JsValue::NULL);
        }
    }

    pub(super) fn publication(&self) -> Publication<'_> {
        Publication(self)
    }
}

pub(super) struct Publication<'a>(&'a Publications);
impl Drop for Publication<'_> {
    fn drop(&mut self) {
        self.0.notify();
    }
}
impl Drop for Publications {
    fn drop(&mut self) {
        if let Ok(close) = method(&self.channel, "close") {
            let _ = close.call0(&self.channel);
        }
    }
}
