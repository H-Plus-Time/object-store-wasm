// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

// NB: Replicated from object_store, modified to remove unsupported
// schemes. references adjusted where applicable.
use object_store::path::Path;
use object_store::ObjectStore;
use snafu::Snafu;
use url::Url;

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(display("Unable to convert URL \"{}\" to filesystem path", url))]
    InvalidUrl { url: Url },

    #[snafu(display("Unable to recognise URL \"{}\"", url))]
    Unrecognised { url: Url },

    #[snafu(display("Feature {scheme:?} not enabled"))]
    NotEnabled { scheme: ObjectStoreScheme },

    #[snafu(context(false))]
    Path { source: object_store::path::Error },
}

impl From<Error> for object_store::Error {
    fn from(e: Error) -> Self {
        Self::Generic {
            store: "URL",
            source: Box::new(e),
        }
    }
}

/// Recognises various URL formats, identifying the relevant [`ObjectStore`]
#[derive(Debug, Eq, PartialEq)]
enum ObjectStoreScheme {
    // /// Url corresponding to [`LocalFileSystem`]
    // Local,
    // /// Url corresponding to [`InMemory`]
    // Memory,
    /// Url corresponding to [`AmazonS3`](crate::aws::AmazonS3)
    AmazonS3,
    // /// Url corresponding to [`GoogleCloudStorage`](crate::gcp::GoogleCloudStorage)
    // GoogleCloudStorage,
    // /// Url corresponding to [`MicrosoftAzure`](crate::azure::MicrosoftAzure)
    // MicrosoftAzure,
    /// Url corresponding to [`HttpStore`](crate::http::HttpStore)
    Http,
}

impl ObjectStoreScheme {
    /// Create an [`ObjectStoreScheme`] from the provided [`Url`]
    ///
    /// Returns the [`ObjectStoreScheme`] and the remaining [`Path`]
    fn parse(url: &Url) -> Result<(Self, Path), Error> {
        let strip_bucket = || Some(url.path().strip_prefix('/')?.split_once('/')?.1);

        let (scheme, path) = match (url.scheme(), url.host_str()) {
            // ("file", None) => (Self::Local, url.path()),
            // ("memory", None) => (Self::Memory, url.path()),
            ("s3" | "s3a", Some(_)) => (Self::AmazonS3, url.path()),
            // ("gs", Some(_)) => (Self::GoogleCloudStorage, url.path()),
            // ("az" | "adl" | "azure" | "abfs" | "abfss", Some(_)) => {
            //     (Self::MicrosoftAzure, url.path())
            // }
            ("http", Some(_)) => (Self::Http, url.path()),
            ("https", Some(host)) => {
                // if host.ends_with("dfs.core.windows.net")
                //     || host.ends_with("blob.core.windows.net")
                //     || host.ends_with("dfs.fabric.microsoft.com")
                //     || host.ends_with("blob.fabric.microsoft.com")
                // {
                //     (Self::MicrosoftAzure, url.path())
                // } else
                if host.ends_with("amazonaws.com") {
                    match host.starts_with("s3") {
                        true => (Self::AmazonS3, strip_bucket().unwrap_or_default()),
                        false => (Self::AmazonS3, url.path()),
                    }
                } else if host.ends_with("r2.cloudflarestorage.com") {
                    (Self::AmazonS3, strip_bucket().unwrap_or_default())
                } else {
                    (Self::Http, url.path())
                }
            }
            _ => return Err(Error::Unrecognised { url: url.clone() }),
        };

        Ok((scheme, Path::from_url_path(path)?))
    }
}

macro_rules! builder_opts {
    ($builder:ty, $url:expr, $options:expr) => {{
        let builder = $options.into_iter().fold(
            <$builder>::new().with_url($url.to_string()),
            |builder, (key, value)| match key.as_ref().parse() {
                Ok(k) => builder.with_config(k, value),
                Err(_) => builder,
            },
        );
        Box::new(builder.build()?) as _
    }};
}

/// Create an [`ObjectStore`] based on the provided `url`
///
/// Returns
/// - An [`ObjectStore`] of the corresponding type
/// - The [`Path`] into the [`ObjectStore`] of the addressed resource
pub fn parse_url(url: &Url) -> Result<(Box<dyn ObjectStore>, Path), object_store::Error> {
    parse_url_opts(url, std::iter::empty::<(&str, &str)>())
}

/// Create an [`ObjectStore`] based on the provided `url` and options
///
/// Returns
/// - An [`ObjectStore`] of the corresponding type
/// - The [`Path`] into the [`ObjectStore`] of the addressed resource
pub fn parse_url_opts<I, K, V>(
    url: &Url,
    options: I,
) -> Result<(Box<dyn ObjectStore>, Path), object_store::Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: Into<String>,
{
    let _options = options;
    let (scheme, path) = ObjectStoreScheme::parse(url)?;
    let path = Path::parse(path)?;

    let store: Box<dyn ObjectStore> = match scheme {
        #[cfg(feature = "aws")]
        ObjectStoreScheme::AmazonS3 => {
            builder_opts!(crate::aws::AmazonS3Builder, url, _options)
        }
        #[cfg(feature = "http")]
        ObjectStoreScheme::Http => {
            let url = &url[..url::Position::BeforePath];
            let parsed_url = Url::parse(url).unwrap();
            Box::new(crate::http::HttpStore::new(parsed_url))
            // builder_opts!(crate::http::HttpBuilder, url, _options)
        }
        #[cfg(not(all(feature = "aws", feature = "http")))]
        s => {
            return Err(object_store::Error::Generic {
                store: "parse_url",
                source: format!("feature for {s:?} not enabled").into(),
            })
        }
    };

    Ok((store, path))
}