use axum::{
    Form, Json,
    extract::{FromRequest, FromRequestParts},
    http::{HeaderName, HeaderValue, Request, StatusCode},
};
use axum_extra::{TypedHeader, headers::Header};
use color_eyre::eyre::Context;
use serde::de::DeserializeOwned;

use crate::{
    error::{Error, WithStatusCode},
    html_or_json::HtmlOrJsonHeader,
};

#[derive(Clone, Copy, Debug)]
pub enum JsonOrFormHeader {
    Json,
    Form,
}

impl Header for JsonOrFormHeader {
    fn name() -> &'static axum::http::HeaderName {
        static NAME: HeaderName = HeaderName::from_static("content-type");
        &NAME
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, axum_extra::headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i axum::http::HeaderValue>,
    {
        let mut result = Self::Form;
        for header in values {
            match header.to_str() {
                Ok("application/json") => result = Self::Json,
                Ok(_) => result = Self::Form,
                Err(_) => return Err(axum_extra::headers::Error::invalid()),
            }
        }
        Ok(result)
    }

    fn encode<E: Extend<axum::http::HeaderValue>>(&self, values: &mut E) {
        match *self {
            Self::Form => values.extend([HeaderValue::from_static(
                "application/x-www-form-urlencoded",
            )]),
            Self::Json => values.extend([HeaderValue::from_static("application/json")]),
        }
    }
}

#[derive(Debug)]
pub struct JsonOrForm<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for JsonOrForm<T> {
    type Rejection = Error;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let (mut parts, body) = req.into_parts();
        let json_or_form: TypedHeader<JsonOrFormHeader> =
            TypedHeader::from_request_parts(&mut parts, state)
                .await
                .unwrap_or(TypedHeader(JsonOrFormHeader::Form));

        let req = Request::from_parts(parts, body);

        let deserialized_type = match json_or_form.0 {
            JsonOrFormHeader::Json => {
                Json::from_request(req, state)
                    .await
                    .wrap_err("Failed to deserialize type to JSON")
                    .with_status_code(StatusCode::BAD_REQUEST, HtmlOrJsonHeader::Json)?
                    .0
            }
            JsonOrFormHeader::Form => {
                Form::from_request(req, state)
                    .await
                    .wrap_err("Failed to deserialize type to Form")
                    .with_status_code(StatusCode::BAD_REQUEST, HtmlOrJsonHeader::Html)?
                    .0
            }
        };

        Ok(Self(deserialized_type))
    }
}
