use std::{collections::HashMap, sync::Arc};

use crate::parse::parse_url_opts as _parse_url_opts;
use chrono::{DateTime, Utc};
use futures::stream::StreamExt;
use js_sys::Object;
use object_store::path::Path;
use object_store::{GetOptions, GetRange, ObjectStore};
use url::Url;
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
            extensions: Default::default(),
        }
    }
}

#[derive(Debug)]
#[wasm_bindgen(getter_with_clone, inspectable)]
pub struct WasmObjectMeta {
    /// The full path to the object
    pub location: String,
    /// The last modified time
    pub last_modified: js_sys::Date,
    /// The size in bytes of the object
    pub size: u64,
    /// The unique identifier for the object
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc9110#name-etag>
    pub e_tag: Option<String>,
    /// A version indicator for this object
    pub version: Option<String>,
}

impl From<object_store::ObjectMeta> for WasmObjectMeta {
    fn from(value: object_store::ObjectMeta) -> Self {
        Self {
            location: value.location.to_string(),
            last_modified: value.last_modified.into(),
            size: value.size,
            e_tag: value.e_tag,
            version: value.version,
        }
    }
}

#[wasm_bindgen]
pub struct WasmObjectStore {
    inner: Arc<dyn ObjectStore>,
    base_path: Option<object_store::path::Path>,
}

#[wasm_bindgen]
impl WasmObjectStore {
    #[wasm_bindgen(constructor)]
    pub fn new(
        url: String,
        options: Option<Object>,
    ) -> Result<WasmObjectStore, wasm_bindgen::JsError> {
        let parsed_url = Url::parse(&url)?;
        let (storage_container, path) = match options {
            Some(options) => {
                let deserialized_options: HashMap<String, String> =
                    serde_wasm_bindgen::from_value(options.into())?;
                _parse_url_opts(&parsed_url, deserialized_options.iter())?
            }
            None => _parse_url_opts(&parsed_url, std::iter::empty::<(String, String)>())?,
        };
        Ok(Self {
            inner: storage_container.into(),
            base_path: Some(path),
        })
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
        let converted_path = Path::from_url_path(location)?;
        let synthesised_location = match &self.base_path {
            Some(path) => Path::from_url_path(format!("{}/{}", path, converted_path))?,
            None => converted_path,
        };
        let res = self.inner.get_opts(&synthesised_location, options).await?;
        let intermediate_stream = res.into_stream().map(|chunk| {
            let inner_chunk = chunk.unwrap();
            let return_vec =
                js_sys::Uint8Array::new_with_length(inner_chunk.len().try_into().unwrap());
            return_vec.copy_from(&inner_chunk);
            Ok(return_vec.into())
        });
        Ok(wasm_streams::ReadableStream::from_stream(intermediate_stream).into_raw())
    }
    #[wasm_bindgen]
    pub async fn list(
        &self,
        prefix: Option<String>,
    ) -> Result<wasm_streams::readable::sys::ReadableStream, wasm_bindgen::JsError> {
        let prefix = match prefix {
            Some(_prefix) => Some(Path::parse(_prefix)?),
            None => None,
        };
        let initial_stream = self.inner.list_with_delimiter(prefix.as_ref()).await?;
        let intermediate_stream = futures::stream::iter(initial_stream.objects).map(|element| {
            let inner: WasmObjectMeta = element.into();
            Ok(inner.into())
        });
        Ok(wasm_streams::ReadableStream::from_stream(intermediate_stream).into_raw())
    }
}
