use chrono::{DateTime, Utc};
use object_store::{GetOptions, GetRange};
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