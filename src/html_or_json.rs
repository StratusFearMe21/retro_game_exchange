use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::ACCEPT},
    response::{Html, IntoResponse},
};
use axum_extra::headers::Header;
use color_eyre::eyre::Context;
use sailfish::{TemplateOnce, TemplateSimple};
use serde::Serialize;

use crate::error::WithStatusCode;

#[derive(TemplateSimple)]
#[template(path = "header.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct HeaderSimple<T: TemplateSimple> {
    template: T,
}

#[derive(TemplateSimple)]
#[template(path = "header.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct HeaderOnce<T: TemplateOnce> {
    template: T,
}

#[derive(Clone, Copy, Debug)]
pub enum HtmlOrJsonHeader {
    Html,
    Json,
}

impl Header for HtmlOrJsonHeader {
    fn name() -> &'static axum::http::HeaderName {
        &ACCEPT
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, axum_extra::headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i axum::http::HeaderValue>,
    {
        let mut result = Self::Html;
        for header in values {
            match header.to_str() {
                Ok("application/json") => result = Self::Json,
                Ok(_) => result = Self::Html,
                Err(_) => return Err(axum_extra::headers::Error::invalid()),
            }
        }
        Ok(result)
    }

    fn encode<E: Extend<axum::http::HeaderValue>>(&self, values: &mut E) {
        match *self {
            Self::Html => values.extend([HeaderValue::from_static("text/html")]),
            Self::Json => values.extend([HeaderValue::from_static("application/json")]),
        }
    }
}

macro_rules! impl_for_templates {
    ($ty_name:ident,$header:ident,$trait:ident,$call:ident) => {
        pub struct $ty_name<T>(pub HtmlOrJsonHeader, pub crate::htmx::HxRequest, pub T);

        impl<T: $trait + Serialize> IntoResponse for $ty_name<T> {
            #[allow(unused_mut)]
            fn into_response(mut self) -> axum::response::Response {
                use axum::http::header::HeaderValue;
                use axum_extra::{headers, typed_header::TypedHeader};

                use crate::htmx::HxRequest;

                let vary_header = TypedHeader(
                    headers::Vary::decode(
                        &mut [
                            HeaderValue::from(HxRequest::name().clone()),
                            HeaderValue::from(ACCEPT),
                        ]
                        .iter(),
                    )
                    .unwrap(),
                );
                match self.0 {
                    HtmlOrJsonHeader::Html => {
                        let html = if self.1.0 {
                            self.2.$call()
                        } else {
                            $header { template: self.2 }.render_once()
                        }
                        .wrap_err("Failed to render template")
                        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR);
                        match html {
                            Ok(html) => (vary_header, Html(html)).into_response(),
                            Err(e) => (vary_header, e).into_response(),
                        }
                    }
                    HtmlOrJsonHeader::Json => (vary_header, Json(self.2)).into_response(),
                }
            }
        }
    };
}

// impl_for_templates!(HtmlOrJson, HeaderOnce, Template, render);
// impl_for_templates!(HtmlOrJsonMut, HeaderOnce, TemplateMut, render_mut);
impl_for_templates!(HtmlOrJsonOnce, HeaderOnce, TemplateOnce, render_once);
impl_for_templates!(HtmlOrJsonSimple, HeaderSimple, TemplateSimple, render_once);
