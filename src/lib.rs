pub mod utils;
#[cfg(all(target_arch = "wasm32", feature = "http"))]
pub mod http;
#[cfg(all(target_arch = "wasm32", feature = "http"))]
pub use http::HttpStore;
#[cfg(all(target_arch = "wasm32", feature = "js_binding"))]
pub use http::js_binding::WasmHttpStore;