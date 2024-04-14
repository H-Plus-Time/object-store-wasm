#[cfg(feature = "http")]
pub mod http;
#[cfg(all(target_arch="wasm32", feature = "js_binding"))]
pub mod js_binding;
pub mod parse;
pub mod utils;
#[cfg(feature = "http")]
pub use http::HttpStore;
#[cfg(feature = "aws")]
pub mod aws;
#[cfg(feature = "aws")]
pub use aws::AmazonS3;
