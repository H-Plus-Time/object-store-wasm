pub use object_store_s3_wasm::S3 as AmazonS3;
pub use object_store_s3_wasm::builder::S3Builder as AmazonS3Builder;
#[cfg(feature = "js_binding")]
pub mod js_binding;