use std::panic;
use std::str::FromStr;
use std::{ops::Deref, sync::Arc, time::SystemTime};

use async_trait::async_trait;
use aws_credential_types::{
    cache::CredentialsCache, provider::SharedCredentialsProvider, Credentials,
};
use aws_sdk_s3::{
    config::{AsyncSleep, Config, Region, SharedAsyncSleep, Sleep},
    primitives::SdkBody,
    Client,
};
use aws_smithy_async::time::{SharedTimeSource, TimeSource};
use aws_smithy_http::result::ConnectorError;
use wasm_bindgen::{JsCast, JsValue};
use wasm_timer::UNIX_EPOCH;

use crate::aws::{error::Error, AmazonS3};
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};

#[derive(Debug, Snafu)]
#[allow(missing_docs)]
enum ConfigError {
    #[snafu(display("Configuration key: '{}' is not known.", key))]
    UnknownConfigurationKey { key: String },

    #[snafu(display(
        "Unknown url scheme cannot be parsed into storage location: {}",
        scheme
    ))]
    UnknownUrlScheme { scheme: String },

    #[snafu(display("URL did not match any known pattern for scheme: {}", url))]
    UrlNotRecognised { url: String },

    #[snafu(display("Unable parse source url. Url: {}, Error: {}", url, source))]
    UnableToParseUrl {
        source: url::ParseError,
        url: String,
    },
}

impl From<ConfigError> for object_store::Error {
    fn from(source: ConfigError) -> Self {
        match source {
            ConfigError::UnknownConfigurationKey { key } => Self::UnknownConfigurationKey {
                store: crate::aws::STORE,
                key,
            },
            _ => Self::Generic {
                store: crate::aws::STORE,
                source: Box::new(source),
            },
        }
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Debug, Copy, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AmazonS3ConfigKey {
    AccessKeyId,
    SecretAccessKey,
    Region,
    SessionToken,
    Bucket,
    Endpoint,
}

impl AsRef<str> for AmazonS3ConfigKey {
    fn as_ref(&self) -> &str {
        match self {
            Self::AccessKeyId => "aws_access_key_id",
            Self::SecretAccessKey => "aws_secret_access_key",
            Self::Region => "aws_region",
            Self::Bucket => "aws_bucket",
            Self::Endpoint => "aws_endpoint",
            Self::SessionToken => "aws_session_token",
        }
    }
}

impl FromStr for AmazonS3ConfigKey {
    type Err = object_store::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "aws_access_key_id" | "access_key_id" => Ok(Self::AccessKeyId),
            "aws_secret_access_key" | "secret_access_key" => Ok(Self::SecretAccessKey),
            "aws_region" | "region" => Ok(Self::Region),
            "aws_bucket" | "aws_bucket_name" | "bucket_name" | "bucket" => Ok(Self::Bucket),
            "aws_endpoint_url" | "aws_endpoint" | "endpoint_url" | "endpoint" => Ok(Self::Endpoint),
            "aws_session_token" | "aws_token" | "session_token" | "token" => Ok(Self::SessionToken),
            _ => Err(ConfigError::UnknownConfigurationKey { key: s.into() }.into()),
        }
    }
}

#[derive(Default)]
pub struct AmazonS3Builder {
    pub(crate) bucket: Option<String>,
    pub(crate) region: Option<String>,
    pub(crate) access_key_id: Option<String>,
    pub(crate) secret_access_key: Option<String>,
    pub(crate) session_token: Option<String>,
    pub(crate) endpoint: Option<String>,
    pub(crate) url: Option<String>,
}

impl AmazonS3Builder {
    pub fn new() -> AmazonS3Builder {
        Self::default()
    }
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set an option on the builder via a key - value pair.
    pub fn with_config(mut self, key: AmazonS3ConfigKey, value: impl Into<String>) -> Self {
        match key {
            AmazonS3ConfigKey::AccessKeyId => self.access_key_id = Some(value.into()),
            AmazonS3ConfigKey::SecretAccessKey => self.secret_access_key = Some(value.into()),
            AmazonS3ConfigKey::Region => self.region = Some(value.into()),
            AmazonS3ConfigKey::Bucket => self.bucket = Some(value.into()),
            AmazonS3ConfigKey::Endpoint => self.endpoint = Some(value.into()),
            AmazonS3ConfigKey::SessionToken => self.session_token = Some(value.into()),
        };
        self
    }

    fn parse_url(&mut self, url: &str) -> object_store::Result<()> {
        let parsed = url::Url::parse(url).context(UnableToParseUrlSnafu { url })?;
        let host = parsed.host_str().context(UrlNotRecognisedSnafu { url })?;
        match parsed.scheme() {
            "s3" | "s3a" => self.bucket = Some(host.to_string()),
            "https" => match host.splitn(4, '.').collect_tuple() {
                Some(("s3", region, "amazonaws", "com")) => {
                    self.region = Some(region.to_string());
                    let bucket = parsed.path_segments().into_iter().flatten().next();
                    if let Some(bucket) = bucket {
                        self.bucket = Some(bucket.into());
                    }
                }
                Some((account, "r2", "cloudflarestorage", "com")) => {
                    self.region = Some("auto".to_string());
                    let endpoint = format!("https://{account}.r2.cloudflarestorage.com");
                    self.endpoint = Some(endpoint);

                    let bucket = parsed.path_segments().into_iter().flatten().next();
                    if let Some(bucket) = bucket {
                        self.bucket = Some(bucket.into());
                    }
                }
                _ => return Err(UrlNotRecognisedSnafu { url }.build().into()),
            },
            scheme => return Err(UnknownUrlSchemeSnafu { scheme }.build().into()),
        };
        Ok(())
    }
    pub fn build(mut self) -> Result<AmazonS3, object_store::Error> {
        if let Some(url) = self.url.take() {
            self.parse_url(&url)?;
        }
        panic::set_hook(Box::new(console_error_panic_hook::hook));
        let access_key_id = self.access_key_id.ok_or(Error::Unknown)?;
        let secret_access_key = self.secret_access_key.ok_or(Error::Unknown)?;
        let session_token = self.session_token;
        let credentials = Credentials::from_keys(
            access_key_id.deref(),
            secret_access_key.deref(),
            session_token,
        );
        let mut builder = Config::builder()
            .force_path_style(true)
            .region(self.region.map(|x| Region::new(x)))
            .credentials_provider(SharedCredentialsProvider::new(credentials))
            .credentials_cache(CredentialsCache::no_caching())
            .sleep_impl(SharedAsyncSleep::new(BrowserSleep))
            .time_source(SharedTimeSource::new(BrowserNow))
            .http_connector(Adapter::new(access_key_id == "access_key"));
        builder.set_endpoint_url(self.endpoint);
        let sdk_config = builder.build();
        Ok(AmazonS3 {
            client: Arc::new(Client::from_conf(sdk_config)),
            bucket: self.bucket.ok_or(Error::Unknown)?,
        })
    }
    pub fn bucket(mut self, value: impl Into<String>) -> Self {
        self.bucket = Some(value.into());
        self
    }

    pub fn region(mut self, value: impl Into<String>) -> Self {
        self.region = Some(value.into());
        self
    }
    pub fn access_key_id(mut self, value: impl Into<String>) -> Self {
        self.access_key_id = Some(value.into());
        self
    }
    pub fn secret_access_key(mut self, value: impl Into<String>) -> Self {
        self.secret_access_key = Some(value.into());
        self
    }
    pub fn session_token(mut self, value: impl Into<String>) -> Self {
        self.session_token = Some(value.into());
        self
    }
    pub fn endpoint(mut self, value: impl Into<String>) -> Self {
        self.endpoint = Some(value.into());
        self
    }
}

#[derive(Debug)]
struct BrowserNow;
impl TimeSource for BrowserNow {
    fn now(&self) -> SystemTime {
        let offset = wasm_timer::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap();
        std::time::UNIX_EPOCH + offset
    }
}

#[derive(Debug, Clone)]
struct BrowserSleep;
impl AsyncSleep for BrowserSleep {
    fn sleep(&self, duration: std::time::Duration) -> Sleep {
        Sleep::new(Box::pin(async move {
            wasm_timer::Delay::new(duration).await.unwrap();
        }))
    }
}

#[async_trait(?Send)]
trait MakeRequestBrowser {
    async fn send(
        parts: http::request::Parts,
        body: SdkBody,
    ) -> Result<http::Response<SdkBody>, JsValue>;
}

pub struct BrowserHttpClient {}

#[async_trait(?Send)]
impl MakeRequestBrowser for BrowserHttpClient {
    async fn send(
        parts: http::request::Parts,
        body: SdkBody,
    ) -> Result<http::Response<SdkBody>, JsValue> {
        use js_sys::{Array, ArrayBuffer, Reflect, Uint8Array};
        use wasm_bindgen_futures::JsFuture;

        let mut opts = web_sys::RequestInit::new();
        opts.method(parts.method.as_str());
        opts.mode(web_sys::RequestMode::Cors);

        let body_pinned = std::pin::Pin::new(body.bytes().unwrap());
        if body_pinned.len() > 0 {
            let uint_8_array = unsafe { Uint8Array::view(&body_pinned) };
            opts.body(Some(&uint_8_array));
        }

        let request = web_sys::Request::new_with_str_and_init(&parts.uri.to_string(), &opts)?;

        for (name, value) in parts
            .headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.to_str().unwrap()))
        {
            request.headers().set(name, value)?;
        }

        let window = web_sys::window().ok_or("could not get window")?;
        let promise = window.fetch_with_request(&request);
        let res_web = JsFuture::from(promise).await?;
        let res_web: web_sys::Response = res_web.dyn_into().unwrap();

        let promise_array = res_web.array_buffer()?;
        let array = JsFuture::from(promise_array).await?;
        let buf: ArrayBuffer = array.dyn_into().unwrap();
        let slice = Uint8Array::new(&buf);
        let body = slice.to_vec();

        let mut builder = http::Response::builder().status(res_web.status());
        for i in js_sys::try_iter(&res_web.headers())?.unwrap() {
            let array: Array = i?.into();
            let values = array.values();

            let prop = String::from("value").into();
            let key = Reflect::get(values.next()?.as_ref(), &prop)?
                .as_string()
                .unwrap();
            let value = Reflect::get(values.next()?.as_ref(), &prop)?
                .as_string()
                .unwrap();
            builder = builder.header(&key, &value);
        }
        let res_body = SdkBody::from(body);
        let res = builder.body(res_body).unwrap();
        Ok(res)
    }
}

pub struct MockedHttpClient {}

#[async_trait(?Send)]
impl MakeRequestBrowser for MockedHttpClient {
    async fn send(
        _parts: http::request::Parts,
        _body: SdkBody,
    ) -> Result<http::Response<SdkBody>, JsValue> {
        let body = "{
            \"Functions\": [
                {
                    \"FunctionName\": \"function-name-1\"
                },
                {
                    \"FunctionName\": \"function-name-2\"
                }
            ],
            \"NextMarker\": null
        }";
        let builder = http::Response::builder().status(200);
        let res = builder.body(SdkBody::from(body)).unwrap();
        Ok(res)
    }
}

#[derive(Debug, Clone)]
struct Adapter {
    use_mock: bool,
}

impl Adapter {
    fn new(use_mock: bool) -> Self {
        Self { use_mock }
    }
}

impl tower::Service<http::Request<SdkBody>> for Adapter {
    type Response = http::Response<SdkBody>;

    type Error = ConnectorError;

    #[allow(clippy::type_complexity)]
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<SdkBody>) -> Self::Future {
        let (parts, body) = req.into_parts();
        let uri = parts.uri.to_string();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let use_mock = self.use_mock;
        wasm_bindgen_futures::spawn_local(async move {
            let fut = if use_mock {
                MockedHttpClient::send(parts, body)
            } else {
                BrowserHttpClient::send(parts, body)
            };
            let _ = tx.send(
                fut.await
                    .unwrap_or_else(|_| panic!("failure while making request to: {}", uri)),
            );
        });

        Box::pin(async move {
            let response = rx.await.map_err(|e| ConnectorError::user(Box::new(e)))?;
            Ok(response)
        })
    }
}
