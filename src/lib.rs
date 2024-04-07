pub mod utils;

use std::default;
use std::fmt::Formatter;
use std::ops::Range;
use std::fmt::Display;

use bytes::Bytes;
use chrono::{DateTime, Utc, TimeZone};
use futures::TryFutureExt;
use futures::stream::BoxStream;
use futures::stream::TryStreamExt;
use futures::stream::StreamExt;
use futures::SinkExt;
use futures::channel::oneshot;
use wasm_bindgen_futures::spawn_local;
use object_store::{path::Path, ObjectMeta};
use url::Url;
use object_store::{
    ObjectStore, GetResult, GetResultPayload, GetOptions, Result, Error, GetRange
};
// use tracing::info;
use backon::ExponentialBuilder;
use backon::Retryable;

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode, Response, RequestBuilder, header::{
    LAST_MODIFIED, CONTENT_LENGTH, HeaderMap, ETAG
}};
use ehttp::streaming::fetch;
use async_stream::stream;
use snafu::{OptionExt, ResultExt, Snafu};
use wasm_bindgen::prelude::*;

#[derive(Debug, Copy, Clone)]
/// Configuration for header extraction
struct HeaderConfig {
    /// Whether to require an ETag header when extracting [`ObjectMeta`] from headers.
    ///
    /// Defaults to `true`
    pub etag_required: bool,
    /// Whether to require a Last-Modified header when extracting [`ObjectMeta`] from headers.
    ///
    /// Defaults to `true`
    pub last_modified_required: bool,

    /// The version header name if any
    pub version_header: Option<&'static str>,
}

#[derive(Debug, Snafu)]
enum HeaderError {
    #[snafu(display("ETag Header missing from response"))]
    MissingEtag,

    #[snafu(display("Received header containing non-ASCII data"))]
    BadHeader { source: reqwest::header::ToStrError },

    #[snafu(display("Last-Modified Header missing from response"))]
    MissingLastModified,

    #[snafu(display("Content-Length Header missing from response"))]
    MissingContentLength,

    #[snafu(display("Invalid last modified '{}': {}", last_modified, source))]
    InvalidLastModified {
        last_modified: String,
        source: chrono::ParseError,
    },

    #[snafu(display("Invalid content length '{}': {}", content_length, source))]
    InvalidContentLength {
        content_length: String,
        source: std::num::ParseIntError,
    },
}

fn get_etag(headers: &ehttp::Headers) -> Result<String, HeaderError> {
    let e_tag = headers.get(ETAG.as_str()).ok_or(HeaderError::MissingEtag)?;
    Ok(e_tag.to_string())
}
fn header_meta(
    location: &Path,
    headers: &ehttp::Headers,
    cfg: HeaderConfig,
) -> Result<ObjectMeta, HeaderError> {
    let last_modified = match headers.get(LAST_MODIFIED.as_str()) {
        Some(last_modified) => {
            DateTime::parse_from_rfc2822(last_modified)
                .context(InvalidLastModifiedSnafu { last_modified })?
                .with_timezone(&Utc)
        }
        None if cfg.last_modified_required => return Err(HeaderError::MissingLastModified),
        None => Utc.timestamp_nanos(0),
    };
    let e_tag = match get_etag(headers) {
        Ok(e_tag) => Some(e_tag),
        Err(HeaderError::MissingEtag) if !cfg.etag_required => None,
        Err(e) => return Err(e),
    };
    let content_length = headers
        .get(CONTENT_LENGTH.as_str())
        .context(MissingContentLengthSnafu)?;
    let size = content_length
        .parse()
        .context(InvalidContentLengthSnafu { content_length })?;

    let version = match cfg.version_header.and_then(|h| headers.get(h)) {
        Some(v) => Some(v.to_string()),
        None => None,
    };
    Ok(ObjectMeta {
        location: location.clone(),
        last_modified,
        version,
        size,
        e_tag,
    })
}

#[derive(Debug)]
#[wasm_bindgen]
pub struct HttpObjectStore {
    url: Url
}

impl HttpObjectStore {
    const STORE: &'static str = "HTTP";
    const HEADER_CONFIG: HeaderConfig = HeaderConfig {
        etag_required: false,
        last_modified_required: false,
        version_header: None,
    };
    pub fn new(url: Url) -> Self {
        Self { url: url.clone() }
    }
}


impl HttpObjectStore {
    fn path_url(&self, location: &Path) -> Url {
        let mut url = self.url.clone();
        url.path_segments_mut().unwrap().extend(location.parts());
        url
    }

    fn get_headers(&self, options: &GetOptions) -> ehttp::Headers {
        let mut headers = ehttp::Headers::default();
        if let Some(range) = &options.range {
            let range = match range {
                GetRange::Bounded(range) => {
                    format!("bytes={}-{}", range.start, range.end.saturating_sub(1))
                }
                GetRange::Offset(offset) => {
                    format!("bytes={}-", offset)
                }
                GetRange::Suffix(upper_limit) => format!("bytes=0-{}", upper_limit)
            };
            headers.insert("range", range);
        }
        
        headers

    }
}

#[async_trait]
impl ObjectStore for HttpObjectStore {
    async fn abort_multipart(
        &self,
        _location: &Path,
        _multipart_id: &object_store::MultipartId,
    ) -> object_store::Result<()> {
        Err(Error::NotImplemented)
    }

    async fn put_multipart(
        &self,
        _location: &Path,
    ) -> object_store::Result<(
        object_store::MultipartId,
        Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    )> {
        Err(Error::NotImplemented)
    }

    async fn copy(
        &self,
        _from: &Path,
        _to: &Path,
    ) -> object_store::Result<()> {
        todo!()
    }
    async fn copy_if_not_exists(
        &self,
        _from: &Path,
        _to: &Path,
    ) -> object_store::Result<()> {
        todo!()
    }
    async fn delete(&self, _location: &Path) -> object_store::Result<()> {
        todo!()
    }
    async fn get_opts(
        &self,
        location: &Path,
        options: object_store::GetOptions,
    ) -> object_store::Result<object_store::GetResult> {
        #[cfg(target_arch = "wasm32")]
        let req = ehttp::Request {
            headers: self.get_headers(&options),
            ..(match options.head {
                false => ehttp::Request::get(self.path_url(location).to_string()),
                true => ehttp::Request::head(self.path_url(location).to_string()),
            })
        };

        let (mut tx, rx) = futures::channel::mpsc::channel(2);
        let (mut tx_headers, rx_headers) = futures::channel::mpsc::channel(1);
        #[cfg(target_arch = "wasm32")]
        spawn_local(async move {

            let mut stream_instance = ehttp::streaming::fetch_async_streaming(&req).await.unwrap();
            while let Some(chunk) = stream_instance.next().await {
                let part = match chunk {
                    Ok(part) => part,
                    Err(err) => {
                        crate::log!("Error: {:?}", err);
                        break;
                    }
                };
                match part {
                    ehttp::streaming::Part::Response(response) => {
                        if response.ok {
                            tx_headers.send(response.headers).await;
                            tx_headers.close_channel();
                            continue;
                        } else {
                            break;
                        }
                    },
                    ehttp::streaming::Part::Chunk(chunk) => {
                        if chunk.is_empty() {
                            tx.close_channel();
                            break;
                        }
                        tx.send(Ok(chunk.into())).await;
                    }
                }
            };
        });
        let safe_stream = rx.boxed();

        let headers_vec = rx_headers.collect::<Vec<ehttp::Headers>>().await;
        let headers = headers_vec.first().unwrap();
        let range = options.range.clone();
        let meta = header_meta(location, &headers, HttpObjectStore::HEADER_CONFIG).unwrap();
        let resolved_range = match range {
            Some(GetRange::Bounded(inner_range)) => inner_range,
            Some(GetRange::Offset(lower_limit)) => (lower_limit..meta.size),
            Some(GetRange::Suffix(upper_limit)) => (0..upper_limit),
            None => 0..meta.size
        };
        Ok(GetResult {
            range: resolved_range,
            payload: GetResultPayload::Stream(safe_stream),
            meta: meta
        })
    }
    async fn put_opts(
        &self,
        _location: &Path,
        _bytes: Bytes,
        _options: object_store::PutOptions,
    ) -> object_store::Result<object_store::PutResult> {
        todo!()
    }
    fn list(&self, _prefix: Option<&Path>) -> BoxStream<'_, object_store::Result<ObjectMeta>> {
        todo!()
    }
    async fn list_with_delimiter(
        &self,
        _prefix: Option<&Path>,
    ) -> object_store::Result<object_store::ListResult> {
        todo!()
    }
    
}
impl Display for HttpObjectStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.url)
    }
}