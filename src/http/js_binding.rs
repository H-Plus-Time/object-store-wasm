use chrono::{DateTime, Utc};
use futures::stream::StreamExt;
use object_store::{path::Path, ObjectStore};
use object_store::{GetOptions, GetRange, Result};
use url::Url;

use crate::http::HttpStore;
use wasm_bindgen::prelude::*;

#[derive(Debug, Default)]
#[wasm_bindgen]
pub struct WasmGetOptions {
    if_match: Option<String>,
    if_none_match: Option<String>,
    if_modified_since: Option<DateTime<Utc>>,
    if_unmodified_since: Option<DateTime<Utc>>,
    range: Option<GetRange>,
    version: Option<String>,
    head: bool,
}

impl From<WasmGetOptions> for GetOptions {
    fn from(value: WasmGetOptions) -> Self {
        GetOptions {
            if_match: value.if_match,
            if_none_match: value.if_none_match,
            if_modified_since: value.if_modified_since,
            if_unmodified_since: value.if_unmodified_since,
            range: value.range,
            version: value.version,
            head: value.head,
        }
    }
}

#[wasm_bindgen]
pub struct WasmHttpStore(HttpStore);

#[wasm_bindgen]
impl WasmHttpStore {
    #[wasm_bindgen(constructor)]
    pub fn new(url: String) -> Result<WasmHttpStore, wasm_bindgen::JsError> {
        let parsed_url = Url::parse(&url)?;
        // NB: query parameters are permitted here, and will be used verbatim
        // (no url encoding)
        let storage_container = HttpStore::new(parsed_url);
        Ok(WasmHttpStore(storage_container))
    }
    #[wasm_bindgen]
    pub async fn get(
        &self,
        location: &str,
        options: Option<WasmGetOptions>,
    ) -> Result<wasm_streams::readable::sys::ReadableStream, wasm_bindgen::JsError> {
        let options = options.unwrap_or_default().into();
        // query parameters will be interpreted as literal parts of the path,
        // and url encoded
        let converted_location = Path::from_url_path(location)?;
        let res = self.0.get_opts(&converted_location, options).await?;
        let intermediate_stream = res.into_stream().map(|chunk| {
            let inner_chunk = chunk.unwrap();
            let return_vec =
                js_sys::Uint8Array::new_with_length(inner_chunk.len().try_into().unwrap());
            return_vec.copy_from(&inner_chunk);
            Ok(return_vec.into())
        });
        Ok(wasm_streams::ReadableStream::from_stream(intermediate_stream).into_raw())
    }
}
