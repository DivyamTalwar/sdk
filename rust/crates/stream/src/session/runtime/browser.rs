use futures::{Stream, channel::oneshot};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use wasm_bindgen::{closure::Closure, prelude::wasm_bindgen};

#[derive(Clone)]
pub(crate) struct OperationSpan {
    span: tracing::Span,
    context: tracing::Span,
    dispatch: tracing::Dispatch,
}
impl OperationSpan {
    pub(crate) fn new(span: tracing::Span) -> Self {
        let dispatch = span
            .with_subscriber(|(_, dispatch)| dispatch.clone())
            .unwrap_or_else(|| tracing::dispatcher::get_default(Clone::clone));
        let context = if span.is_disabled() {
            tracing::Span::current()
        } else {
            span.clone()
        };
        Self {
            span,
            context,
            dispatch,
        }
    }
    pub(crate) fn new_root(span: tracing::Span) -> Self {
        let dispatch = span
            .with_subscriber(|(_, dispatch)| dispatch.clone())
            .unwrap_or_else(|| tracing::dispatcher::get_default(Clone::clone));
        Self {
            context: span.clone(),
            span,
            dispatch,
        }
    }
    pub(crate) fn follows_from_same_subscriber(&self, cause: &Self) {
        if let (Some(registry), Some(origin)) = (
            self.dispatch.downcast_ref::<tracing_subscriber::Registry>(),
            cause
                .dispatch
                .downcast_ref::<tracing_subscriber::Registry>(),
        ) && std::ptr::eq(registry, origin)
        {
            self.span.follows_from(cause.context.id());
        }
    }
    pub(crate) fn in_scope<T>(&self, work: impl FnOnce() -> T) -> T {
        if self.dispatch.is::<tracing::subscriber::NoSubscriber>()
            && tracing::dispatcher::get_default(|current| {
                current.is::<tracing::subscriber::NoSubscriber>()
            })
        {
            return work();
        }
        tracing::dispatcher::with_default(&self.dispatch, || self.context.in_scope(work))
    }
    pub(crate) fn scope<T>(&self, source: T) -> Scoped<T> {
        Scoped {
            source: Some(source),
            context: self.clone(),
        }
    }
}
impl std::ops::Deref for OperationSpan {
    type Target = tracing::Span;
    fn deref(&self) -> &Self::Target {
        &self.span
    }
}
pin_project_lite::pin_project! {
    pub(crate) struct Scoped<T> {
        #[pin]
        source: Option<T>,
        context: OperationSpan,
    }
    impl<T> PinnedDrop for Scoped<T> {
        fn drop(this: Pin<&mut Self>) {
            let mut this = this.project();
            this.context.in_scope(|| this.source.set(None));
        }
    }
}
impl<T: Future> Future for Scoped<T> {
    type Output = T::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        this.context
            .in_scope(|| this.source.as_pin_mut().expect("live producer").poll(cx))
    }
}
impl<T: Stream> Stream for Scoped<T> {
    type Item = T::Item;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let result = this.context.in_scope(|| {
            this.source
                .as_mut()
                .as_pin_mut()
                .map_or(Poll::Ready(None), |source| source.poll_next(cx))
        });
        if matches!(result, Poll::Ready(None)) {
            this.context.in_scope(|| this.source.set(None));
        }
        result
    }
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(callback: &Closure<dyn FnMut()>, milliseconds: u32) -> wasm_bindgen::JsValue;
    #[wasm_bindgen(js_name = clearTimeout)]
    fn clear_timeout(timer: &wasm_bindgen::JsValue);
}
struct Timer {
    handle: wasm_bindgen::JsValue,
    _callback: Closure<dyn FnMut()>,
}
impl Drop for Timer {
    fn drop(&mut self) {
        clear_timeout(&self.handle);
    }
}
pub(crate) async fn delay(milliseconds: u32) {
    // Browsers return a number; Node/Bun may return an object. Preserve the
    // original handle so cancellation clears the actual runtime timer.
    let (sender, receiver) = oneshot::channel();
    let mut sender = Some(sender);
    let callback: Closure<dyn FnMut()> = Closure::new(move || {
        if let Some(sender) = sender.take() {
            let _ = sender.send(());
        }
    });
    let handle = set_timeout(&callback, milliseconds);
    let _timer = Timer {
        handle,
        _callback: callback,
    };
    let _ = receiver.await;
}
