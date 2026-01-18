use axum::{
    Json,
    http::{HeaderName, HeaderValue, StatusCode},
    response::{Html, IntoResponse},
};
use axum_extra::headers::Header;
use color_eyre::eyre::Context;
use sailfish::{Template, TemplateMut, TemplateOnce, TemplateSimple};
use serde::Serialize;

use crate::error::WithStatusCode;

#[derive(Clone, Copy, Debug)]
pub enum HtmlOrJsonHeader {
    Html,
    Json,
}

impl Header for HtmlOrJsonHeader {
    fn name() -> &'static axum::http::HeaderName {
        static NAME: HeaderName = HeaderName::from_static("accept");
        &NAME
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
    ($ty_name:ident,$trait:ident,$call:ident) => {
        pub struct $ty_name<T>(pub HtmlOrJsonHeader, pub T);

        impl<T: $trait + Serialize> IntoResponse for $ty_name<T> {
            #[allow(unused_mut)]
            fn into_response(mut self) -> axum::response::Response {
                match self.0 {
                    HtmlOrJsonHeader::Html => {
                        use sailfish::runtime::{Buffer, SizeHint};

                        let error_div = r#"<div id="error" hx-swap-oob="true"></div>"#;
                        static SIZE_HINT: SizeHint = SizeHint::new();
                        let mut buffer = Buffer::with_capacity(SIZE_HINT.get());
                        buffer.push_str(error_div);
                        match self
                            .1
                            .$call(&mut buffer)
                            .wrap_err("Failed to render template")
                            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)
                        {
                            Ok(()) => {
                                SIZE_HINT.update(buffer.len());
                                Html(buffer.into_string()).into_response()
                            }
                            Err(e) => e.into_response(),
                        }
                    }
                    HtmlOrJsonHeader::Json => Json(self.1).into_response(),
                }
            }
        }
    };
}

impl_for_templates!(HtmlOrJson, Template, render_to);
impl_for_templates!(HtmlOrJsonMut, TemplateMut, render_mut_to);
impl_for_templates!(HtmlOrJsonOnce, TemplateOnce, render_once_to);
impl_for_templates!(HtmlOrJsonSimple, TemplateSimple, render_once_to);
