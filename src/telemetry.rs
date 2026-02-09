use std::fmt::{self, Display};

use axum::http::HeaderMap;
use diesel::connection::{Instrumentation, InstrumentationEvent};
use opentelemetry::{
    Context, KeyValue,
    propagation::TextMapPropagator,
    trace::{SpanContext, Status, TraceContextExt},
};
use opentelemetry_otlp::{Compression, WithExportConfig, WithTonicConfig};
use opentelemetry_sdk::{
    Resource,
    logs::SdkLoggerProvider,
    propagation::TraceContextPropagator,
    trace::{RandomIdGenerator, Sampler, SdkTracerProvider},
};
use opentelemetry_semantic_conventions::{
    SCHEMA_URL,
    resource::{
        DEPLOYMENT_ENVIRONMENT_NAME, SERVICE_INSTANCE_ID, SERVICE_NAME, SERVICE_NAMESPACE,
        SERVICE_VERSION,
    },
};
use tower_http::trace::{DefaultMakeSpan, MakeSpan};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::ServiceConfig;

pub struct HeaderMapCarrier<'a> {
    headers: &'a HeaderMap,
}

impl<'a> HeaderMapCarrier<'a> {
    pub fn new(headers: &'a HeaderMap) -> Self {
        Self { headers }
    }
}

impl<'a> opentelemetry::propagation::Extractor for HeaderMapCarrier<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.headers.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.headers.keys().map(|k| k.as_str()).collect()
    }

    fn get_all(&self, key: &str) -> Option<Vec<&str>> {
        let headers = self
            .headers
            .get_all(key)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();

        if headers.is_empty() {
            None
        } else {
            Some(headers)
        }
    }
}

#[derive(Clone)]
pub struct TelemetryMakeSpan(pub DefaultMakeSpan);

impl<B> MakeSpan<B> for TelemetryMakeSpan {
    fn make_span(&mut self, request: &axum::http::Request<B>) -> tracing::Span {
        let span = self.0.make_span(request);
        if request.uri().path() != "/health" {
            let propogator = TraceContextPropagator::new();

            let parent_context = propogator.extract(&HeaderMapCarrier::new(request.headers()));
            span.set_parent(parent_context).unwrap();
        } else {
            span.set_parent(Context::map_current(|cx| {
                cx.with_remote_span_context(SpanContext::NONE)
            }))
            .unwrap();
        }

        span
    }
}

struct DbQuerySanitizerWriter<W: fmt::Write>(W, bool);

impl<W: fmt::Write> fmt::Write for DbQuerySanitizerWriter<W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.1 {
            return Ok(());
        }

        if let Some((text, _)) = s.split_once("--") {
            self.0.write_str(text.trim_end())?;
            self.1 = true;
        } else {
            self.0.write_str(s)?;
        }

        Ok(())
    }
}

impl<W: fmt::Write> DbQuerySanitizerWriter<W> {
    fn new(writer: W) -> Self {
        Self(writer, false)
    }
}

struct DbQuerySanitizer<Q: Display>(Q);

impl<Q: Display> Display for DbQuerySanitizer<Q> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Write::write_fmt(
            &mut DbQuerySanitizerWriter::new(f),
            format_args!("{}", self.0),
        )
    }
}

#[derive(Default)]
pub struct DieselInstrumentation(Option<Span>);

impl Instrumentation for DieselInstrumentation {
    fn on_connection_event(&mut self, event: diesel::connection::InstrumentationEvent<'_>) {
        match event {
            InstrumentationEvent::StartQuery { query, .. } => {
                let span =
                    tracing::info_span!("postgres-query", db.query.text = %DbQuerySanitizer(query));
                self.0 = Some(span);
            }
            InstrumentationEvent::FinishQuery { error, .. } => {
                let Some(span) = self.0.take() else {
                    return;
                };
                if let Some(error) = error {
                    span.set_status(Status::Error {
                        description: format!("{}", error).into(),
                    });
                }
            }
            _ => {}
        }
    }
}

// Create a Resource that captures information about the entity for which telemetry is recorded.
fn resource(service: &ServiceConfig) -> Resource {
    Resource::builder()
        .with_service_name(format!("{}", service.service_type))
        .with_schema_url(
            [
                KeyValue::new(SERVICE_NAMESPACE, env!("CARGO_PKG_NAME")),
                KeyValue::new(SERVICE_NAME, format!("{}", service.service_type)),
                KeyValue::new(SERVICE_INSTANCE_ID, service.id.clone()),
                KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                KeyValue::new(DEPLOYMENT_ENVIRONMENT_NAME, "develop"),
            ],
            SCHEMA_URL,
        )
        .build()
}

pub fn init_tracing_provider(service: &ServiceConfig) -> SdkTracerProvider {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_compression(Compression::Gzip);

    let exporter = if let Some(endpoint) = service.otlp_endpoint.as_ref() {
        exporter.with_endpoint(endpoint).build().unwrap()
    } else {
        exporter.build().unwrap()
    };

    SdkTracerProvider::builder()
        // Customize sampling strategy
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            1.0,
        ))))
        // If export trace to AWS X-Ray, you can use XrayIdGenerator
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource(service))
        .with_batch_exporter(exporter)
        .build()
}

pub fn init_logging_provider(service: &ServiceConfig) -> SdkLoggerProvider {
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_compression(Compression::Gzip);

    let exporter = if let Some(endpoint) = service.otlp_endpoint.as_ref() {
        exporter.with_endpoint(endpoint).build().unwrap()
    } else {
        exporter.build().unwrap()
    };

    SdkLoggerProvider::builder()
        .with_resource(resource(service))
        .with_batch_exporter(exporter)
        .build()
}
