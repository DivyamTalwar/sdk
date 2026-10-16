//! The browser owns no recovery or transport state outside the Rust client.
use super::{
    json::{from_js, to_js},
    query::WasmTaskVolume,
};
use crate::client::{FileClient, FileClientError, FileClientLimits};
use acyclic_stream::StreamProvider;
use std::cell::RefCell;
use wasm_bindgen::prelude::*;

type Inner = FileClient<dyn StreamProvider, dyn StreamProvider>;
fn error(value: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&value.to_string())
}
fn admission_error(value: FileClientError) -> JsValue {
    to_js(&value).unwrap_or_else(|error| error)
}

/// A Rust client with a private volume outbox and a platform task volume.
#[wasm_bindgen]
pub struct WasmFileClient {
    inner: RefCell<Option<Inner>>,
}

#[wasm_bindgen]
impl WasmFileClient {
    #[wasm_bindgen]
    pub async fn open(
        outbox: &WasmTaskVolume,
        target: &WasmTaskVolume,
        #[wasm_bindgen(unchecked_param_type = "FileClientLimits")] limits: JsValue,
    ) -> Result<WasmFileClient, JsValue> {
        let limits: FileClientLimits = from_js(limits)?;
        let inner = FileClient::open(outbox.volume.clone(), target.volume.clone(), limits)
            .await
            .map_err(admission_error)?;
        Ok(Self {
            inner: RefCell::new(Some(inner)),
        })
    }

    /// Queue an immutable task request in the caller's private volume.
    #[wasm_bindgen]
    pub async fn enqueue(
        &self,
        #[wasm_bindgen(unchecked_param_type = "FileKey")] key: JsValue,
        bytes: js_sys::Uint8Array,
    ) -> Result<(), JsValue> {
        let key = from_js(key)?;
        let mut inner = self
            .inner
            .try_borrow_mut()
            .map_err(error)?
            .take()
            .ok_or_else(|| error("client publication is already running"))?;
        let length = u64::from(bytes.length());
        let result = inner
            .enqueue_with(key, length, || {
                let bytes = bytes.to_vec();
                crate::client::validate_offline_request(&bytes)?;
                Ok(bytes.into())
            })
            .await
            .map_err(admission_error);
        *self.inner.try_borrow_mut().map_err(error)? = Some(inner);
        result
    }

    /// Drain requests in durable enqueue order. Reconnects resume the same files.
    #[wasm_bindgen]
    pub async fn flush(&self) -> Result<usize, JsValue> {
        let mut inner = self
            .inner
            .try_borrow_mut()
            .map_err(error)?
            .take()
            .ok_or_else(|| error("client publication is already running"))?;
        let result = inner.flush().await.map_err(admission_error);
        *self.inner.try_borrow_mut().map_err(error)? = Some(inner);
        result
    }

    #[wasm_bindgen]
    pub fn pending(&self) -> Result<usize, JsValue> {
        Ok(self
            .inner
            .try_borrow()
            .map_err(error)?
            .as_ref()
            .ok_or_else(|| error("client publication is already running"))?
            .pending())
    }

    #[wasm_bindgen(unchecked_return_type = "Cursor")]
    pub fn cursor(&self) -> Result<JsValue, JsValue> {
        to_js(
            self.inner
                .try_borrow()
                .map_err(error)?
                .as_ref()
                .ok_or_else(|| error("client publication is already running"))?
                .cursor(),
        )
    }
}
