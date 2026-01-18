//! # yaaxum-error
//! Yet Another Axum Error Handler
//!
//! This crate uses `eyre` to capture the error,
//! the error is then returned to the browser or
//! whatever it is, it's then nicely formatted to
//! a webpage
use std::fmt::Debug;

use axum::{Json, body::Body, http::StatusCode, response::IntoResponse};
use color_eyre::eyre::eyre;
use sailfish::Template;
use serde::{
    Serialize,
    ser::{SerializeSeq, SerializeStruct},
};
use tower_http::catch_panic::ResponseForPanic;
use tracing::instrument;
use tracing_error::SpanTrace;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{Array, Object, Ref, RefOr, Schema, Type},
};

use crate::Placeholder;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Serialize, ToSchema)]
struct Frame<'a> {
    module_path: Option<&'a str>,
    name: &'a str,
    file: Option<&'a str>,
    line: Option<u32>,
    fields: &'a str,
}

#[derive(Serialize, ToSchema)]
pub struct Actions {
    #[serde(skip_serializing_if = "Option::is_none")]
    sign_out: Option<&'static str>,
    ok: bool,
}

impl Default for Actions {
    fn default() -> Self {
        Self {
            sign_out: None,
            ok: true,
        }
    }
}

impl Actions {
    pub fn sign_out() -> Self {
        Self {
            sign_out: Some("Sign out"),
            ok: true,
        }
    }
}

#[derive(Template)]
#[template(path = "error.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct Error {
    status_code: StatusCode,
    error: color_eyre::eyre::Report,
    actions: Actions,
}

impl Placeholder for Error {
    #[instrument]
    fn placeholder() -> Self {
        Self {
            status_code: StatusCode::INTERNAL_SERVER_ERROR,
            error: eyre!("Example error"),
            actions: Actions::default(),
        }
    }
}

impl Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.handler().debug(self.error.as_ref(), f)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        (self.status_code, Json(self)).into_response()
    }
}

impl PartialSchema for Error {
    fn schema() -> RefOr<Schema> {
        let mut obj = Object::new();

        macro_rules! string {
            () => {
                RefOr::T(Schema::Object(Object::with_type(Type::String)))
            };
        }

        macro_rules! array {
            ($type:expr) => {
                RefOr::T(Schema::Array(Array::new($type)))
            };
        }

        obj.required = vec![
            "title".to_owned(),
            "text".to_owned(),
            "icon".to_owned(),
            "chain".to_owned(),
            "spantrace".to_owned(),
            "buttons".to_owned(),
            "content".to_owned(),
        ];

        obj.properties.insert("title".to_owned(), string!());
        obj.properties.insert("text".to_owned(), string!());
        obj.properties.insert("icon".to_owned(), string!());
        obj.properties.insert("chain".to_owned(), array!(string!()));
        obj.properties.insert(
            "spantrace".to_owned(),
            array!(RefOr::Ref(Ref::new("#/components/schemas/Frame"))),
        );
        obj.properties.insert(
            "buttons".to_owned(),
            RefOr::Ref(Ref::new("#/components/schemas/Actions")),
        );
        obj.properties.insert("content".to_owned(), string!());

        RefOr::T(Schema::Object(obj))
    }
}

impl ToSchema for Error {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Error")
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schemas.push((Frame::name().into_owned(), Frame::schema()));
        schemas.push((Actions::name().into_owned(), Actions::schema()));
    }
}

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut obj = serializer.serialize_struct("Error", 7)?;

        obj.serialize_field("title", "Error")?;
        obj.serialize_field(
            "text",
            self.status_code
                .canonical_reason()
                .unwrap_or_else(|| self.status_code.as_str()),
        )?;
        obj.serialize_field("icon", "error")?;
        obj.serialize_field("chain", &SerializeChain(&self.error))?;
        let handler: &color_eyre::Handler = self.error.handler().downcast_ref().unwrap();
        obj.serialize_field(
            "spantrace",
            &SerializeSpantrace(handler.span_trace().unwrap()),
        )?;
        obj.serialize_field("buttons", &self.actions)?;
        obj.serialize_field(
            "content",
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

impl From<Error> for Box<dyn std::error::Error + Sync + Send + 'static> {
    fn from(value: Error) -> Self {
        value.error.into()
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

        Error {
            status_code: StatusCode::INTERNAL_SERVER_ERROR,
            error: eyre!("{}", error_string),
            actions: Actions::default(),
        }
        .into_response()
    }
}

pub trait WithStatusCode<T> {
    fn with_status_code(self, status_code: StatusCode) -> Result<T>;
    fn with_status_code_and_actions(self, status_code: StatusCode, actions: Actions) -> Result<T>;
}

impl<T> WithStatusCode<T> for std::result::Result<T, color_eyre::eyre::Report> {
    fn with_status_code(self, status_code: StatusCode) -> Result<T> {
        self.map_err(|error| Error {
            status_code,
            error,
            actions: Actions::default(),
        })
    }

    fn with_status_code_and_actions(self, status_code: StatusCode, actions: Actions) -> Result<T> {
        self.map_err(|error| Error {
            status_code,
            error,
            actions,
        })
    }
}
