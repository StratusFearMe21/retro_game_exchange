use std::borrow::Cow;

use axum::{
    body::Bytes,
    extract::{OptionalFromRequestParts, Query},
    http::{HeaderName, HeaderValue, Uri},
};
use axum_extra::{TypedHeader, headers::Header};
use color_eyre::eyre::Context;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::error::{self, WithStatusCode};

#[derive(Clone, Copy, Debug)]
pub struct HxRefresh(pub bool);

impl Header for HxRefresh {
    fn name() -> &'static axum::http::HeaderName {
        static NAME: HeaderName = HeaderName::from_static("hx-refresh");
        &NAME
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, axum_extra::headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i axum::http::HeaderValue>,
    {
        let mut refresh = false;
        for value in values {
            refresh = value == "true";
        }

        Ok(HxRefresh(refresh))
    }

    fn encode<E: Extend<axum::http::HeaderValue>>(&self, values: &mut E) {
        if self.0 {
            values.extend([HeaderValue::from_static("true")]);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HxRequest(pub bool);

impl Header for HxRequest {
    fn name() -> &'static axum::http::HeaderName {
        static NAME: HeaderName = HeaderName::from_static("hx-request");
        &NAME
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, axum_extra::headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i axum::http::HeaderValue>,
    {
        let mut hx_request = false;
        for value in values {
            hx_request = value == "true";
        }

        Ok(HxRequest(hx_request))
    }

    fn encode<E: Extend<axum::http::HeaderValue>>(&self, values: &mut E) {
        if self.0 {
            values.extend([HeaderValue::from_static("true")]);
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct HxLocation {
    pub path: Cow<'static, str>,
    pub target: Cow<'static, str>,
}

impl HxLocation {
    pub fn new(path: impl Into<Cow<'static, str>>) -> Self {
        Self {
            path: path.into(),
            target: Cow::Borrowed("#content"),
        }
    }
}

impl Header for HxLocation {
    fn name() -> &'static HeaderName {
        static NAME: HeaderName = HeaderName::from_static("hx-location");
        &NAME
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, axum_extra::headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i HeaderValue>,
    {
        let location = values.next().unwrap();
        let bytes = location.as_bytes();
        if bytes[0] == b'{' {
            serde_json::from_slice(bytes).map_err(|_| axum_extra::headers::Error::invalid())
        } else {
            Ok(HxLocation {
                path: Cow::Owned(
                    location
                        .to_str()
                        .map_err(|_| axum_extra::headers::Error::invalid())?
                        .to_owned(),
                ),
                ..Default::default()
            })
        }
    }

    fn encode<E: Extend<HeaderValue>>(&self, values: &mut E) {
        let json = Bytes::from(serde_json::to_vec(self).unwrap());
        values.extend([HeaderValue::from_maybe_shared(json).unwrap()]);
    }
}

pub struct HxCurrentUrl(pub HeaderValue);

impl Default for HxCurrentUrl {
    fn default() -> Self {
        Self(HeaderValue::from_static(""))
    }
}

impl Header for HxCurrentUrl {
    fn name() -> &'static HeaderName {
        static NAME: HeaderName = HeaderName::from_static("hx-current-url");
        &NAME
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, axum_extra::headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i HeaderValue>,
    {
        Ok(HxCurrentUrl(
            values
                .next()
                .cloned()
                .unwrap_or(HeaderValue::from_static("")),
        ))
    }

    fn encode<E: Extend<HeaderValue>>(&self, values: &mut E) {
        values.extend([self.0.clone()]);
    }
}

#[derive(Debug)]
pub struct HxQuery<T: DeserializeOwned>(pub T);

impl<S, T: DeserializeOwned> OptionalFromRequestParts<S> for HxQuery<T>
where
    S: Send + Sync,
{
    type Rejection = error::Error;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        let Some(TypedHeader(HxCurrentUrl(hx_current_url))): Option<TypedHeader<HxCurrentUrl>> =
            TypedHeader::from_request_parts(parts, state)
                .await
                .wrap_err("Failed to parse value in HX-Current-URL")
                .with_status_code(StatusCode::BAD_REQUEST)?
        else {
            return Ok(None);
        };

        let uri = Uri::from_maybe_shared(hx_current_url)
            .wrap_err("Failed to parse HX-Current-URL")
            .with_status_code(StatusCode::BAD_REQUEST)?;

        let Query(result) = Query::try_from_uri(&uri)
            .wrap_err("Failed to deserialize type with HX-Current-URL query string")
            .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

        Ok(Some(HxQuery(result)))
    }
}
