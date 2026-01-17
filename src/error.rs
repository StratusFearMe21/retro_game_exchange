//! # yaaxum-error
//! Yet Another Axum Error Handler
//!
//! This crate uses `eyre` to capture the error,
//! the error is then returned to the browser or
//! whatever it is, it's then nicely formatted to
//! a webpage
use std::fmt::Debug;

use axum::{
    Json,
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse},
};
use color_eyre::eyre::eyre;
use sailfish::Template;
use serde::{
    Serialize,
    ser::{SerializeSeq, SerializeStruct},
};
use tower_http::catch_panic::ResponseForPanic;
use tracing::instrument;
use tracing_error::SpanTrace;

use crate::{Placeholder, html_or_json::HtmlOrJsonHeader, openapi_template};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Template)]
#[template(path = "error.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct ErrorTemplate {
    error: color_eyre::eyre::Report,
}

impl Placeholder for ErrorTemplate {
    #[instrument(fields(example_field = 1))]
    fn placeholder() -> Self {
        let error = eyre!("Example error");
        Self { error }
    }
}

openapi_template!(ErrorTemplate);

pub struct Error(
    pub StatusCode,
    pub color_eyre::eyre::Report,
    pub HtmlOrJsonHeader,
);

impl Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.1.handler().debug(self.1.as_ref(), f)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        let error_template = ErrorTemplate { error: self.1 };
        match self.2 {
            HtmlOrJsonHeader::Html => (
                self.0,
                {
                    let mut map = HeaderMap::new();
                    map.append("hx-reswap", HeaderValue::from_static("beforeend"));
                    map
                },
                Html(
                    error_template
                        .render()
                        .unwrap_or_else(|e| format!("Failed to render error?: {}", e)),
                ),
            )
                .into_response(),
            HtmlOrJsonHeader::Json => (self.0, Json(error_template)).into_response(),
        }
    }
}

impl Serialize for ErrorTemplate {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut obj = serializer.serialize_struct("Error", 3)?;

        obj.serialize_field("chain", &SerializeChain(&self.error))?;
        let handler: &color_eyre::Handler = self.error.handler().downcast_ref().unwrap();
        obj.serialize_field(
            "spantrace",
            &SerializeSpantrace(handler.span_trace().unwrap()),
        )?;
        obj.serialize_field(
            "html",
            &self
                .render()
                .unwrap_or_else(|e| format!("Failed to render template: `{}`", e)),
        )?;

        obj.end()
    }
}

struct SerializeChain<'a>(&'a color_eyre::eyre::Report);

impl Serialize for SerializeChain<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let chain = self.0.chain();
        let mut seq = serializer.serialize_seq(Some(chain.len()))?;
        for error in chain {
            seq.serialize_element(&format!("{}", error))?;
        }
        seq.end()
    }
}

struct SerializeSpantrace<'a>(&'a SpanTrace);

impl Serialize for SerializeSpantrace<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut result = Ok(());
        let mut seq = serializer.serialize_seq(None)?;
        self.0.with_spans(|span, fields| {
            #[derive(Serialize)]
            struct Frame<'a> {
                module_path: Option<&'a str>,
                name: &'a str,
                file: Option<&'a str>,
                line: Option<u32>,
                fields: &'a str,
            }

            if let Err(e) = seq.serialize_element(&Frame {
                module_path: span.module_path(),
                name: span.name(),
                file: span.file(),
                line: span.line(),
                fields,
            }) {
                result = Err(e);
                false
            } else {
                true
            }
        });
        result?;
        seq.end()
    }
}

impl From<Error> for Box<dyn std::error::Error + Sync + Send> {
    fn from(value: Error) -> Self {
        value.1.into()
    }
}

#[derive(Clone, Copy)]
pub struct PanicHandler;

impl ResponseForPanic for PanicHandler {
    type ResponseBody = Body;

    fn response_for_panic(
        &mut self,
        err: Box<dyn std::any::Any + Send + 'static>,
    ) -> axum::http::Response<Self::ResponseBody> {
        let error_string = if let Some(s) = err.downcast_ref::<String>() {
            tracing::error!("Service panicked: {}", s);
            s.as_str()
        } else if let Some(s) = err.downcast_ref::<&str>() {
            tracing::error!("Service panicked: {}", s);
            s
        } else {
            let s = "Service panicked but `CatchPanic` was unable to downcast the panic info";
            tracing::error!("{}", s);
            s
        };

        Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            eyre!("{}", error_string),
            HtmlOrJsonHeader::Json,
        )
        .into_response()
    }
}

pub trait WithStatusCode<T> {
    fn with_status_code(self, code: StatusCode, html_or_json: HtmlOrJsonHeader) -> Result<T>;
}

impl<T> WithStatusCode<T> for std::result::Result<T, color_eyre::eyre::Report> {
    fn with_status_code(self, code: StatusCode, html_or_json: HtmlOrJsonHeader) -> Result<T> {
        self.map_err(|e| Error(code, e, html_or_json))
    }
}
