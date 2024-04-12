use std::fmt::Display;

use bytes::Bytes;
use chrono::{DateTime, TimeZone, Utc};
use futures::channel::oneshot;
use futures::stream::BoxStream;
use futures::stream::StreamExt;
use object_store::PutMode;
use object_store::PutResult;
use object_store::{path::Path, ObjectMeta};
use object_store::{Error, GetOptions, GetRange, GetResult, GetResultPayload, ObjectStore, Result};
use url::Url;
use wasm_bindgen_futures::spawn_local;
// use tracing::info;
use backon::ExponentialBuilder;
use backon::Retryable;

use async_trait::async_trait;
use reqwest::{
    header::{HeaderMap, CONTENT_LENGTH, ETAG, LAST_MODIFIED},
    Client, Method, RequestBuilder, Response, StatusCode,
};
use snafu::{OptionExt, ResultExt, Snafu};

#[cfg(feature = "js_binding")]
pub mod js_binding;

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

fn get_etag(headers: &HeaderMap) -> Result<String, HeaderError> {
    let e_tag = headers.get(ETAG).ok_or(HeaderError::MissingEtag)?;
    Ok(e_tag.to_str().context(BadHeaderSnafu)?.to_string())
}

fn header_meta(
    location: &Path,
    headers: &HeaderMap,
    cfg: HeaderConfig,
) -> Result<ObjectMeta, HeaderError> {
    let last_modified = match headers.get(LAST_MODIFIED) {
        Some(last_modified) => {
            let last_modified = last_modified.to_str().context(BadHeaderSnafu)?;
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
        .get(CONTENT_LENGTH)
        .context(MissingContentLengthSnafu)?;

    let content_length = content_length.to_str().context(BadHeaderSnafu)?;
    let size = content_length
        .parse()
        .context(InvalidContentLengthSnafu { content_length })?;

    let version = match cfg.version_header.and_then(|h| headers.get(h)) {
        Some(v) => Some(v.to_str().context(BadHeaderSnafu)?.to_string()),
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

pub trait GetOptionsExt {
    fn with_get_options(self, options: GetOptions) -> Self;
}

impl GetOptionsExt for RequestBuilder {
    fn with_get_options(mut self, options: GetOptions) -> Self {
        use reqwest::header::*;

        if let Some(range) = options.range {
            let range = match range {
                GetRange::Bounded(range) => {
                    format!("bytes={}-{}", range.start, range.end.saturating_sub(1))
                }
                GetRange::Offset(offset) => {
                    format!("bytes={}-", offset)
                }
                GetRange::Suffix(upper_limit) => format!("bytes=0-{}", upper_limit),
            };
            self = self.header(RANGE, range);
        }

        if let Some(tag) = options.if_match {
            self = self.header(IF_MATCH, tag);
        }

        if let Some(tag) = options.if_none_match {
            self = self.header(IF_NONE_MATCH, tag);
        }

        const DATE_FORMAT: &str = "%a, %d %b %Y %H:%M:%S GMT";
        if let Some(date) = options.if_unmodified_since {
            self = self.header(IF_UNMODIFIED_SINCE, date.format(DATE_FORMAT).to_string());
        }

        if let Some(date) = options.if_modified_since {
            self = self.header(IF_MODIFIED_SINCE, date.format(DATE_FORMAT).to_string());
        }

        self
    }
}

#[derive(Debug, Clone)]
struct InnerClient {
    url: Url,
    client: Client,
}

impl InnerClient {
    const STORE: &'static str = "HTTP";
    const HEADER_CONFIG: HeaderConfig = HeaderConfig {
        etag_required: false,
        last_modified_required: false,
        version_header: None,
    };
    fn new(url: Url) -> Self {
        Self {
            url,
            client: Client::new(),
        }
    }

    fn path_url(&self, location: &Path) -> Url {
        let mut url = self.url.clone();
        url.path_segments_mut().unwrap().extend(location.parts());
        url
    }

    async fn get_request(&self, path: &Path, options: GetOptions) -> Result<Response> {
        let url = self.path_url(path);
        let has_range = options.range.is_some();
        let method = match options.head {
            true => Method::HEAD,
            false => Method::GET,
        };
        let builder = self.client.request(method, url).with_get_options(options);
        let res_func = || async { builder.try_clone().unwrap().send().await };
        let res = res_func
            .retry(&ExponentialBuilder::default())
            .await
            .map_err(|source| match source.status() {
                // Some stores return METHOD_NOT_ALLOWED for get on directories
                Some(StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED) => Error::NotFound {
                    source: Box::new(source),
                    path: path.to_string(),
                },
                _ => Error::Generic {
                    store: InnerClient::STORE,
                    source: Box::new(source),
                },
            })?;

        // We expect a 206 Partial Content response if a range was requested
        // a 200 OK response would indicate the server did not fulfill the request
        if has_range && res.status() != StatusCode::PARTIAL_CONTENT {
            return Err(Error::NotSupported {
                source: Box::new(Error::NotImplemented {
                    // href: path.to_string(),
                }),
            });
        }

        Ok(res)
    }

    async fn get_opts(&self, location: &Path, options: GetOptions) -> Result<GetResult> {
        let range = options.range.clone();
        let response = self.get_request(location, options).await?;
        let meta =
            header_meta(location, response.headers(), InnerClient::HEADER_CONFIG).map_err(|e| {
                Error::Generic {
                    store: InnerClient::STORE,
                    source: Box::new(e),
                }
            })?;
        let (tx, rx) = futures::channel::mpsc::channel(1);
        spawn_local(async move {
            let stream = response.bytes_stream();
            stream
                .map(|chunk| {
                    Ok(chunk.map_err(|source| Error::Generic {
                        store: InnerClient::STORE,
                        source: Box::new(source),
                    }))
                })
                .forward(tx)
                .await
                .unwrap();
        });
        let safe_stream = rx.boxed();

        let resolved_range = match range {
            Some(GetRange::Bounded(inner_range)) => inner_range,
            Some(GetRange::Offset(lower_limit)) => lower_limit..meta.size,
            Some(GetRange::Suffix(upper_limit)) => 0..upper_limit,
            None => 0..meta.size,
        };
        Ok(GetResult {
            range: resolved_range,
            payload: GetResultPayload::Stream(safe_stream),
            meta,
        })
    }
    pub async fn delete(&self, path: &Path) -> Result<()> {
        let url = self.path_url(path);
        self.client
            .delete(url)
            .send()
            .await
            .map_err(|source| match source.status() {
                Some(StatusCode::NOT_FOUND) => Error::NotFound {
                    source: Box::new(source),
                    path: path.to_string(),
                },
                // TODO: de-genericize
                _ => Error::Generic {
                    store: InnerClient::STORE,
                    source: Box::new(source),
                },
            })?;
        Ok(())
    }

    pub async fn put(&self, _path: &Path, _payload: Bytes) -> Result<Response> {
        todo!()
    }
}

#[derive(Debug)]
pub struct HttpStore {
    client: InnerClient,
}

impl HttpStore {
    pub fn new(url: Url) -> Self {
        Self {
            client: InnerClient::new(url),
        }
    }
}

#[async_trait]
impl ObjectStore for HttpStore {
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

    async fn copy(&self, _from: &Path, _to: &Path) -> object_store::Result<()> {
        todo!()
    }
    async fn copy_if_not_exists(&self, _from: &Path, _to: &Path) -> object_store::Result<()> {
        todo!()
    }
    async fn delete(&self, _location: &Path) -> object_store::Result<()> {
        let (sender, receiver) = oneshot::channel();
        let copied_client = self.client.clone();
        let cloned_location = _location.clone();
        spawn_local(async move {
            let res = copied_client.delete(&cloned_location).await;
            sender.send(res).unwrap();
        });
        receiver.await.unwrap()
    }

    async fn get_opts(
        &self,
        location: &Path,
        options: object_store::GetOptions,
    ) -> object_store::Result<object_store::GetResult> {
        let (sender, receiver) = oneshot::channel();
        let copied_client = self.client.clone();
        let copied_location = location.clone();
        spawn_local(async move {
            let res = copied_client.get_opts(&copied_location, options).await;
            sender.send(res).unwrap();
        });

        receiver.await.unwrap()
    }
    async fn put_opts(
        &self,
        _location: &Path,
        _bytes: Bytes,
        _options: object_store::PutOptions,
    ) -> object_store::Result<object_store::PutResult> {
        if _options.mode != PutMode::Overwrite {
            // TODO: Add support for If header - https://datatracker.ietf.org/doc/html/rfc2518#section-9.4
            return Err(Error::NotImplemented);
        }

        let response = self.client.put(_location, _bytes).await?;
        let e_tag = match get_etag(response.headers()) {
            Ok(e_tag) => Some(e_tag),
            Err(HeaderError::MissingEtag) => None,
            Err(source) => {
                return Err(Error::Generic {
                    store: InnerClient::STORE,
                    source: Box::new(source),
                })
            }
        };

        Ok(PutResult {
            e_tag,
            version: None,
        })
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
impl Display for HttpStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.client)
    }
}
