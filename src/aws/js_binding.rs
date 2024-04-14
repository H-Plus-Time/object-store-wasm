use futures::stream::StreamExt;
use js_sys::Object;
use object_store::{path::Path, ObjectStore};
use object_store::Result;
use object_store_s3_wasm::builder::S3Builder;
use crate::js_binding::WasmGetOptions;
use serde::{Serialize, Deserialize};
use crate::aws::AmazonS3;
use wasm_bindgen::prelude::*;

#[derive(Debug, Default, Serialize, Deserialize)]
#[wasm_bindgen]
pub struct WasmS3BuilderOptions {
    bucket: Option<String>,
    region: Option<String>,
    access_key_id: Option<String>,
    secret_access_key: Option<String>,
    session_token: Option<String>,
    endpoint: Option<String>,
    url: Option<String>
}
#[wasm_bindgen]
impl WasmS3BuilderOptions {
    pub fn from_object(obj: Object) -> Result<WasmS3BuilderOptions, serde_wasm_bindgen::Error> {
        use serde_wasm_bindgen::from_value;
        from_value(obj.into())
    }
}

#[wasm_bindgen]
pub struct WasmAmazonS3(AmazonS3);

#[wasm_bindgen]
impl WasmAmazonS3 {
    #[wasm_bindgen(constructor)]
    pub fn new(url: String, options: WasmS3BuilderOptions) -> Result<WasmAmazonS3, wasm_bindgen::JsError> {
        use object_store_s3_wasm::builder::AmazonS3ConfigKey;
        let builder = S3Builder::new().with_url(url);
        let storage_container = builder.with_config(
            AmazonS3ConfigKey::AccessKeyId, options.access_key_id.unwrap()
        ).with_config(
            AmazonS3ConfigKey::SecretAccessKey, options.secret_access_key.unwrap()
        ).with_config(
            AmazonS3ConfigKey::Region, options.region.unwrap()
        ).build().unwrap();
        Ok(WasmAmazonS3(storage_container))
    }
    #[wasm_bindgen]
    pub async fn get(
        &self,
        location: &str,
        options: Option<WasmGetOptions>,
    ) -> Result<wasm_streams::readable::sys::ReadableStream, wasm_bindgen::JsError> {
        let options = options.unwrap_or_default().into();
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
