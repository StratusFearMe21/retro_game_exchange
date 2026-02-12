use std::{
    fmt::{self, Display},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::http::HeaderMap;
use diesel::connection::{Instrumentation, InstrumentationEvent};
use opentelemetry::{
    Context, KeyValue, global,
    propagation::{Extractor, Injector},
    trace::{Span, SpanBuilder, SpanContext, Status, TraceContextExt, Tracer},
};
use opentelemetry_otlp::{Compression, WithExportConfig, WithTonicConfig};
use opentelemetry_sdk::{
    Resource,
    logs::SdkLoggerProvider,
    propagation::TraceContextPropagator,
    trace::{RandomIdGenerator, Sampler, SdkTracer, SdkTracerProvider},
};
use opentelemetry_semantic_conventions::{
    SCHEMA_URL,
    resource::{
        DEPLOYMENT_ENVIRONMENT_NAME, SERVICE_INSTANCE_ID, SERVICE_NAME, SERVICE_NAMESPACE,
        SERVICE_VERSION,
    },
};
use rdkafka::{
    Message,
    message::{BorrowedHeaders, BorrowedMessage, Headers, OwnedHeaders},
};
use tower_http::trace::{DefaultMakeSpan, MakeSpan};
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

impl<'a> Extractor for HeaderMapCarrier<'a> {
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

pub struct KafkaOwnedHeaderCarrier<'a> {
    headers: &'a mut OwnedHeaders,
}

impl<'a> KafkaOwnedHeaderCarrier<'a> {
    pub fn new(headers: &'a mut OwnedHeaders) -> Self {
        Self { headers }
    }
}

impl<'a> Injector for KafkaOwnedHeaderCarrier<'a> {
    fn set(&mut self, key: &str, value: String) {
        let headers = std::mem::replace(self.headers, OwnedHeaders::new_with_capacity(0));
        *self.headers = headers.insert(rdkafka::message::Header {
            key,
            value: Some(&value),
        });
    }
}

pub struct KafkaBorrowedHeaderCarrier<'a> {
    headers: &'a BorrowedHeaders,
}

impl<'a> KafkaBorrowedHeaderCarrier<'a> {
    pub fn new(headers: &'a BorrowedHeaders) -> Self {
        Self { headers }
    }

    fn get(&self, key: &str) -> impl Iterator<Item = &str> {
        self.headers
            .iter()
            .filter(move |h| h.key == key)
            .filter_map(|h| h.value.and_then(|v| std::str::from_utf8(v).ok()))
    }
}

impl<'a> Extractor for KafkaBorrowedHeaderCarrier<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.get(key).next()
    }

    fn keys(&self) -> Vec<&str> {
        self.headers.iter().map(|h| h.key).collect()
    }

    fn get_all(&self, key: &str) -> Option<Vec<&str>> {
        let values = self.get(key).collect::<Vec<_>>();

        if values.is_empty() {
            None
        } else {
            Some(values)
        }
    }
}

#[derive(Clone)]
pub struct TelemetryMakeSpan(pub DefaultMakeSpan);

impl<B> MakeSpan<B> for TelemetryMakeSpan {
    fn make_span(&mut self, request: &axum::http::Request<B>) -> tracing::Span {
        let span = self.0.make_span(request);
        if request.uri().path() != "/health" {
            let parent_context = global::get_text_map_propagator(|propagator| {
                propagator.extract(&HeaderMapCarrier::new(request.headers()))
            });
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
pub struct DieselInstrumentation(Option<tracing::Span>);

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

pub fn span_from_kafka_msg(tracer: &SdkTracer, msg: &BorrowedMessage<'_>) -> tracing::Span {
    let time = SystemTime::now();

    let span_builder = SpanBuilder {
        name: std::borrow::Cow::Borrowed("kafka-message-in-queue"),
        start_time: msg
            .timestamp()
            .to_millis()
            .map(|ts| UNIX_EPOCH + Duration::from_millis(ts as _)),
        ..Default::default()
    };

    let mut kafka_msg_span = if let Some(headers) = msg.headers() {
        let parent_context = global::get_text_map_propagator(|propagator| {
            propagator.extract(&KafkaBorrowedHeaderCarrier::new(headers))
        });

        tracer.build_with_context(span_builder, &parent_context)
    } else {
        tracer.build(span_builder)
    };

    kafka_msg_span.set_attributes([
        KeyValue::new("topic", msg.topic().to_owned()),
        KeyValue::new("partition", msg.partition() as i64),
    ]);

    kafka_msg_span.end_with_timestamp(time);

    let span = tracing::info_span!(
        "kafka-message",
        topic = msg.topic(),
        partition = msg.partition()
    );
    span.set_parent(opentelemetry::Context::map_current(|cx| {
        cx.with_remote_span_context(kafka_msg_span.span_context().clone())
    }))
    .unwrap();

    span
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

fn kafka_resource() -> Resource {
    Resource::builder().with_service_name("kafka").build()
}

pub fn init_kafka_tracing_provider(service: &ServiceConfig) -> SdkTracerProvider {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_compression(Compression::Gzip);

    let exporter = if let Some(endpoint) = service.kafka_otlp_endpoint.as_ref() {
        exporter.with_endpoint(endpoint).build().unwrap()
    } else {
        exporter.build().unwrap()
    };

    // global::set_text_map_propagator(TraceContextPropagator::new());

    SdkTracerProvider::builder()
        // Customize sampling strategy
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            1.0,
        ))))
        // If export trace to AWS X-Ray, you can use XrayIdGenerator
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(kafka_resource())
        .with_batch_exporter(exporter)
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

    global::set_text_map_propagator(TraceContextPropagator::new());

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
