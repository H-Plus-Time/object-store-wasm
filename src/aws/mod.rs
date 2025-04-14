use std::{fmt::Display, num::ParseIntError, ops::Range, sync::Arc};

use async_trait::async_trait;
use aws_sdk_s3::Client;
use builder::AmazonS3Builder;
use chrono::{DateTime, TimeZone, Utc};
use error::Error;
use futures::{
    stream::{self, BoxStream},
    TryFutureExt, TryStreamExt,
};
use object_store::Attributes;
use object_store::{
    GetRange, GetResultPayload, ListResult, ObjectMeta, ObjectStore, PutOptions, PutResult,
};

pub mod builder;
mod error;
mod multipart;
const STORE: &str = "S3";

#[derive(Debug)]
pub struct AmazonS3 {
    client: Arc<Client>,
    bucket: String,
}

impl AmazonS3 {
    pub fn builder() -> AmazonS3Builder {
        AmazonS3Builder::default()
    }
}

#[async_trait]
impl ObjectStore for AmazonS3 {
    async fn copy(
        &self,
        from: &object_store::path::Path,
        to: &object_store::path::Path,
    ) -> object_store::Result<()> {
        let mut source_bucket_and_object: String = "".to_owned();
        source_bucket_and_object.push_str(&self.bucket);
        source_bucket_and_object.push('/');
        source_bucket_and_object.push_str(from.as_ref());
        self.client
            .copy_object()
            .copy_source(source_bucket_and_object)
            .bucket(self.bucket.clone())
            .key(to.to_string())
            .send()
            .await
            .map_err(Error::from)?;
        Ok(())
    }
    async fn copy_if_not_exists(
        &self,
        _from: &object_store::path::Path,
        _to: &object_store::path::Path,
    ) -> object_store::Result<()> {
        Err(object_store::Error::NotSupported {
            source: Box::new(Error::Unknown),
        })
    }
    async fn delete(&self, location: &object_store::path::Path) -> object_store::Result<()> {
        self.client
            .delete_object()
            .bucket(self.bucket.clone())
            .key(location.to_string())
            .send()
            .await
            .map_err(Error::from)?;
        Ok(())
    }
    async fn get_opts(
        &self,
        location: &object_store::path::Path,
        options: object_store::GetOptions,
    ) -> object_store::Result<object_store::GetResult> {
        let request = self
            .client
            .get_object()
            .bucket(self.bucket.clone())
            .key(location.to_string());
        let request = match options.if_match {
            Some(if_match) => request.if_match(if_match),
            None => request,
        };
        let request = match options.if_none_match {
            Some(if_none_match) => request.if_none_match(if_none_match),
            None => request,
        };
        let request = match options.if_modified_since {
            Some(if_modified_since) => {
                let date_time = aws_smithy_types::DateTime::from_millis(
                    if_modified_since
                        .signed_duration_since::<Utc>(DateTime::from_timestamp(0, 0).unwrap())
                        .num_milliseconds(),
                );
                request.if_modified_since(date_time)
            }
            None => request,
        };
        let request = match options.if_unmodified_since {
            Some(if_unmodified_since) => {
                let date_time = aws_smithy_types::DateTime::from_millis(
                    if_unmodified_since
                        .signed_duration_since::<Utc>(DateTime::from_timestamp(0, 0).unwrap())
                        .num_milliseconds(),
                );
                request.if_modified_since(date_time)
            }
            None => request,
        };
        let request = if let Some(range) = options.range {
            let range = match range {
                GetRange::Bounded(range) => {
                    format!("bytes={}-{}", range.start, range.end.saturating_sub(1))
                }
                GetRange::Offset(offset) => {
                    format!("bytes={}-", offset)
                }
                GetRange::Suffix(upper_limit) => format!("bytes=-{}", upper_limit),
            };
            request.range(range)
        } else {
            request
        };
        let response = request.send().await.map_err(Error::from)?;
        let last_modified = Utc
            .timestamp_millis_opt(
                response
                    .last_modified()
                    .ok_or(Error::Unknown)?
                    .to_millis()
                    .map_err(Error::from)?,
            )
            .unwrap();
        let size = response.content_length() as u64;
        // TODO: restore the original error handling in the case that
        // the content_range *is* present
        let range = response
            .content_range
            // .ok_or(Error::Unknown)?
            .unwrap_or(format!("0-{}", size))
            .trim_start_matches("bytes=")
            .split('-')
            .map(|x| x.parse::<u64>())
            .collect::<Result<Vec<_>, ParseIntError>>()
            .map_err(Error::from)?;
        Ok(object_store::GetResult {
            payload: GetResultPayload::Stream(Box::pin(response.body.map_err(|err| {
                object_store::Error::Generic {
                    store: "aws_smithy",
                    source: Box::new(err),
                }
            }))),
            meta: ObjectMeta {
                location: location.to_string().into(),
                last_modified,
                size,
                e_tag: response.e_tag,
                version: None,
            },
            range: Range {
                start: range[0],
                end: range[1],
            },
            attributes: Attributes::new(),
        })
    }
    async fn head(
        &self,
        location: &object_store::path::Path,
    ) -> object_store::Result<object_store::ObjectMeta> {
        let output = self
            .client
            .head_object()
            .set_bucket(Some(self.bucket.clone()))
            .set_key(Some(location.to_string()))
            .send()
            .await
            .map_err(Error::from)?;
        let last_modified = DateTime::from_timestamp_millis(
            output
                .last_modified()
                .ok_or(Error::Unknown)?
                .to_millis()
                .map_err(Error::from)?,
        )
        .unwrap();
        let meta = ObjectMeta {
            location: location.clone(),
            last_modified,
            size: output.content_length() as u64,
            e_tag: output.e_tag().map(|x| x.to_string()),
            version: None,
        };
        Ok(meta)
    }
    fn list(
        &self,
        prefix: Option<&object_store::path::Path>,
    ) -> BoxStream<'static, object_store::Result<object_store::ObjectMeta>> {
        let request = self.client.list_objects_v2().bucket(self.bucket.clone());
        let request = match prefix {
            Some(prefix) => request.prefix(prefix.to_string()),
            None => request,
        };
        Box::pin(
            request
                .send()
                .map_err(|_| object_store::Error::from(Error::Unknown))
                .and_then(|response| async {
                    match response.contents {
                        Some(contents) => {
                            Ok(Box::pin(stream::iter(contents.into_iter().map(|object| {
                                let last_modified = DateTime::from_timestamp_millis(
                                    object
                                        .last_modified()
                                        .ok_or(Error::Unknown)?
                                        .to_millis()
                                        .map_err(Error::from)?,
                                )
                                .unwrap();
                                Ok(ObjectMeta {
                                    location: object
                                        .key
                                        .ok_or(object_store::Error::Generic {
                                            store: "aws",
                                            source: Box::new(Error::Unknown),
                                        })?
                                        .into(),
                                    last_modified,
                                    size: object.size as u64,
                                    e_tag: object.e_tag,
                                    version: None,
                                })
                            }))) as BoxStream<_>)
                        }
                        None => Ok(Box::pin(stream::empty()) as BoxStream<_>),
                    }
                })
                .try_flatten_stream()
                .into_stream(),
        )
    }

    async fn list_with_delimiter(
        &self,
        prefix: Option<&object_store::path::Path>,
    ) -> object_store::Result<object_store::ListResult> {
        let request = self.client.list_objects_v2().bucket(self.bucket.clone());
        let request = match prefix {
            Some(prefix) => request.prefix(prefix.to_string()),
            None => request,
        };
        let response = request.send().await.map_err(Error::from)?;
        let objects = match response.contents {
            Some(contents) => contents
                .into_iter()
                .map(|object| {
                    let last_modified = DateTime::from_timestamp_millis(
                        object
                            .last_modified()
                            .ok_or(Error::Unknown)?
                            .to_millis()
                            .map_err(Error::from)?,
                    )
                    .unwrap();
                    Ok(ObjectMeta {
                        location: object
                            .key
                            .ok_or(object_store::Error::Generic {
                                store: "aws",
                                source: Box::new(Error::Unknown),
                            })?
                            .into(),
                        last_modified,
                        size: object.size as u64,
                        e_tag: object.e_tag,
                        version: None,
                    })
                })
                .collect::<Result<Vec<_>, object_store::Error>>()?,
            None => Vec::new(),
        };
        Ok(ListResult {
            objects,
            common_prefixes: response
                .common_prefixes
                .and_then(|prefixes| {
                    prefixes
                        .into_iter()
                        .map(|x| x.prefix.map(|y| y.into()))
                        .collect::<Option<Vec<_>>>()
                })
                .unwrap_or(Vec::new()),
        })
    }
    async fn put_opts(
        &self,
        location: &object_store::path::Path,
        payload: object_store::PutPayload,
        opts: PutOptions,
    ) -> object_store::Result<PutResult> {
        let buf = bytes::Bytes::from(payload);
        let result = self
            .client
            .put_object()
            .bucket(self.bucket.clone())
            .key(location.to_string())
            .body(buf.into())
            .tagging(opts.tags.encoded())
            .send()
            .await
            .map_err(Error::from)?;
        Ok(PutResult {
            e_tag: result.e_tag,
            version: result.version_id,
        })
    }
    async fn put_multipart(
        &self,
        _location: &object_store::path::Path,
    ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
        Err(object_store::Error::NotImplemented)

        // let response = self
        //     .client
        //     .create_multipart_upload()
        //     .bucket(self.bucket.clone())
        //     .key(location.to_string())
        //     .send()
        //     .await
        //     .map_err(Error::from)?;

        // let multipart_upload = Box::new(WriteMultiPart::new(
        //     MultiPartUpload {
        //         bucket: self.bucket.clone(),
        //         location: location.to_string(),
        //         upload_id: response.upload_id.clone().ok_or(Error::Unknown)?,
        //         client: self.client.clone(),
        //     },
        //     16,
        // ));

        // Ok((response.upload_id.ok_or(Error::Unknown)?, multipart_upload))
    }

    async fn put_multipart_opts(
        &self,
        _location: &object_store::path::Path,
        _opts: object_store::PutMultipartOpts,
    ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
        Err(object_store::Error::NotImplemented)
    }
}

impl Display for AmazonS3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.client.config())
    }
}
