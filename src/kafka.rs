use std::{borrow::Cow, panic::AssertUnwindSafe, pin::Pin, sync::Arc, task::Poll, time::Duration};

use arc_swap::ArcSwap;
use color_eyre::eyre::{self, Context, OptionExt, eyre};
use futures_util::{FutureExt, Stream, StreamExt, future::CatchUnwind, ready};
use opentelemetry_sdk::trace::SdkTracer;
use pin_project_lite::pin_project;
use rdkafka::{
    ClientConfig, ClientContext, Message, TopicPartitionList,
    consumer::{
        BaseConsumer, Consumer, ConsumerContext, DefaultConsumerContext, MessageStream, Rebalance,
        StreamConsumer,
    },
    error::KafkaError,
    message::BorrowedMessage,
    producer::{FutureProducer, FutureRecord},
};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, Sleep};
use tokio_util::{future::FutureExt as TokioFutureExt, sync::CancellationToken, task::TaskTracker};
use tracing::Instrument;

use crate::{ServiceType, emails, telemetry};

#[derive(Serialize, Deserialize)]
pub struct RetryRecord<'a> {
    retry_service: ServiceType,
    errors: Vec<Cow<'a, str>>,
    record: Option<Cow<'a, [u8]>>,
}

pub struct JobError {
    error: eyre::Report,
    status: JobErrorStatus,
}

pub enum JobErrorStatus {
    Retryable,
    NotRetryable,
}

pub trait WithErrorStatus<T> {
    fn with_error_status(self, status: JobErrorStatus) -> Result<T, JobError>;
}

impl<T> WithErrorStatus<T> for std::result::Result<T, color_eyre::eyre::Report> {
    fn with_error_status(self, status: JobErrorStatus) -> Result<T, JobError> {
        self.map_err(|error| JobError { error, status })
    }
}

pin_project! {
    struct SafeFutureRunner<F> {
        #[pin]
        future: CatchUnwind<AssertUnwindSafe<F>>
    }
}

impl<T, F: Future<Output = Result<T, JobError>>> Future for SafeFutureRunner<F> {
    type Output = Result<T, JobError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();

        match ready!(this.future.poll(cx)) {
            Ok(res) => Poll::Ready(res),
            Err(err) => {
                let error_string = if let Some(s) = err.downcast_ref::<String>() {
                    s.as_str()
                } else if let Some(s) = err.downcast_ref::<&str>() {
                    s
                } else {
                    "Service panicked but we were unable to downcast the panic info"
                };

                let report: eyre::Report = eyre!("{}", error_string);
                Poll::Ready(Err(JobError {
                    error: report,
                    status: JobErrorStatus::NotRetryable,
                }))
            }
        }
    }
}

impl<T, F: Future<Output = Result<T, JobError>>> SafeFutureRunner<F> {
    pub fn new(future: F) -> SafeFutureRunner<F> {
        Self {
            future: AssertUnwindSafe(future).catch_unwind(),
        }
    }
}

async fn handle_failure<'a>(
    max_failures: usize,
    producer: &FutureProducer,
    msg: &BorrowedMessage<'a>,
    service_type: ServiceType,
    record: Option<RetryRecord<'a>>,
    error: JobError,
) -> eyre::Result<()> {
    tracing::error!("{}", error.error);

    let mut retry_record = match record {
        Some(r) => r,
        None => RetryRecord {
            retry_service: service_type,
            errors: Vec::new(),
            record: msg.payload().map(Cow::Borrowed),
        },
    };

    retry_record
        .errors
        .push(Cow::Owned(error.error.to_string()));

    let payload_bytes =
        postcard::to_stdvec(&retry_record).wrap_err("Failed to serialize retry record to bytes")?;

    let topic = match error.status {
        JobErrorStatus::Retryable if max_failures >= retry_record.errors.len() => {
            Cow::Owned(format!("email-queue-retry-{}", retry_record.errors.len()))
        }
        _ => {
            tracing::error!("Message made it to the DLQ");
            Cow::Borrowed("email-queue-dlq")
        }
    };

    let mut future_record = FutureRecord::to(topic.as_ref()).payload(&payload_bytes);

    if let Some(key) = msg.key() {
        future_record = future_record.key(key);
    }
    if let Some(headers) = msg.headers() {
        future_record = future_record.headers(headers.detach());
    }

    producer
        .send(future_record, Duration::from_secs(0))
        .await
        .map_err(|(e, _)| e)
        .wrap_err("Failed to publish failed kafka message")?;

    Ok(())
}

pub struct ParallelConsumerContext {
    tasks: TaskTracker,
    cancellation_token: Arc<ArcSwap<CancellationToken>>,
}

impl ClientContext for ParallelConsumerContext {}

impl ConsumerContext for ParallelConsumerContext {
    fn pre_rebalance(&self, _base_consumer: &BaseConsumer<Self>, _rebalance: &Rebalance<'_>) {
        self.cancellation_token.load().cancel();
    }

    fn post_rebalance(&self, _base_consumer: &BaseConsumer<Self>, _rebalance: &Rebalance<'_>) {
        self.tasks.close();
    }
}

impl ParallelConsumerContext {
    fn new(tasks: TaskTracker, cancellation_token: Arc<ArcSwap<CancellationToken>>) -> Self {
        Self {
            tasks,
            cancellation_token,
        }
    }
}

pub async fn run(
    topic: &str,
    service_type: ServiceType,
    kafka_config: ClientConfig,
    kafka_tracer: &SdkTracer,
    shutdown_signal: impl Future<Output = ()>,
) -> eyre::Result<()> {
    let tasks = TaskTracker::new();
    let cancellation_token = Arc::new(ArcSwap::from_pointee(CancellationToken::new()));
    let main_canceller = CancellationToken::new();

    let producer: FutureProducer = kafka_config
        .create()
        .wrap_err("Failed to create kafka producer")?;
    let consumer: Arc<StreamConsumer<ParallelConsumerContext>> = Arc::new(
        kafka_config
            .create_with_context(ParallelConsumerContext::new(
                tasks.clone(),
                Arc::clone(&cancellation_token),
            ))
            .wrap_err("Failed to create consumer")?,
    );

    consumer
        .subscribe(&[topic])
        .wrap_err("Failed to subscribe to  topic")?;

    let shutdown_signal = shutdown_signal.with_cancellation_token(&main_canceller);
    tokio::pin!(shutdown_signal);

    loop {
        tasks.reopen();
        cancellation_token.store(Arc::new(CancellationToken::new()));
        {
            let assigned = consumer
                .assignment()
                .wrap_err("Failed to get assignments for consumer")?;

            tracing::info!(num_tasks = assigned.count(), "Rebalancing");
            for assignment in assigned.elements() {
                let subconsumer = consumer
                    .split_partition_queue(assignment.topic(), assignment.partition())
                    .ok_or_eyre("Couldn't create subconsumer for topic and partition")?;

                let token = cancellation_token.load_full();
                let tracer = kafka_tracer.clone();
                let producer = producer.clone();
                let consumer = Arc::clone(&consumer);
                tasks.spawn(
                    async move {
                        let mut consumer_stream = subconsumer.stream();

                        while let Some(Some(msg)) =
                            consumer_stream.next().with_cancellation_token(&token).await
                        {
                            let msg = msg.wrap_err("Failed to receive message from kafka")?;
                            let span = telemetry::span_from_kafka_msg(&tracer, &msg);

                            let result = SafeFutureRunner::new(
                                emails::process_message(msg.payload()).instrument(span),
                            )
                            .with_cancellation_token(&token)
                            .await;

                            match result {
                                Some(Err(e)) => {
                                    handle_failure(
                                        // Only matters for retry service
                                        usize::MAX,
                                        &producer,
                                        &msg,
                                        service_type,
                                        None,
                                        e,
                                    )
                                    .await?;
                                }
                                Some(Ok(_)) => {
                                    if !token.is_cancelled() {
                                        consumer
                                            .store_offset_from_message(&msg)
                                            .wrap_err("Failed to commit message to stream")?;
                                    }
                                }
                                None => {}
                            }
                        }

                        Ok::<(), eyre::Report>(())
                    }
                    .map({
                        let token = main_canceller.clone();
                        move |r| {
                            // uuuuuuhhhh, maybe restarting will fix whatever happened?????
                            if let Err(e) = r {
                                tracing::error!("{}", e);
                                token.cancel();
                            }
                        }
                    }),
                );
            }

            let token = cancellation_token.load_full();
            let consumer = Arc::clone(&consumer);
            tasks.spawn(async move {
                let mut consumer_stream = consumer.stream();

                loop {
                    match consumer_stream.next().with_cancellation_token(&token).await {
                        Some(_) => {
                            tracing::warn!("Main consumer should not be receiving messages");
                        }
                        None => break,
                    }
                }
            });
        }

        tokio::select! {
            _ = tasks.wait() => {}
            _ = &mut shutdown_signal => {
                cancellation_token.load().cancel();
                tasks.close();
                tasks.wait().await;
                break;
            }
        }
    }

    Ok(())
}

pin_project! {
    struct RateLimitedConsumer<'a> {
        consumer: Option<MessageStream<'a, DefaultConsumerContext>>,
        #[pin]
        sleep: Sleep,
        retry_mins: u64
    }
}

impl<'a> RateLimitedConsumer<'a> {
    fn new(
        consumer: Option<MessageStream<'a, DefaultConsumerContext>>,
        sleep: Sleep,
        retry_mins: u64,
    ) -> Self {
        Self {
            consumer,
            sleep,
            retry_mins,
        }
    }
}

impl<'a> Stream for RateLimitedConsumer<'a> {
    type Item = Result<BorrowedMessage<'a>, KafkaError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        ready!(this.sleep.as_mut().poll(cx));
        if let Some(consumer) = this.consumer {
            let next = ready!(consumer.poll_next_unpin(cx));
            this.sleep
                .as_mut()
                .reset(Instant::now() + Duration::from_mins(*this.retry_mins));
            Poll::Ready(next)
        } else {
            Poll::Pending
        }
    }
}

pin_project! {
    struct SelectAllStreams<S> {
        streams: Box<[S]>,
    }
}

impl<S: Stream> SelectAllStreams<S> {
    fn new(streams: Vec<S>) -> Self {
        Self {
            streams: streams.into_boxed_slice(),
        }
    }
}

impl<S: Stream> Stream for SelectAllStreams<S> {
    type Item = S::Item;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.project();
        if let Some(next_item) = this.streams.iter_mut().find_map(|s| {
            let s = unsafe { Pin::new_unchecked(s) };

            match s.poll_next(cx) {
                Poll::Ready(Some(i)) => Some(Some(i)),
                Poll::Pending | Poll::Ready(None) => None,
            }
        }) {
            Poll::Ready(next_item)
        } else {
            Poll::Pending
        }
    }
}

async fn decode_retry_record(payload: Option<&[u8]>) -> Result<RetryRecord<'_>, JobError> {
    postcard::from_bytes(
        payload
            .ok_or_eyre("Message contained no payload")
            .with_error_status(JobErrorStatus::NotRetryable)?,
    )
    .wrap_err("Failed to deserialize retry record")
    .with_error_status(JobErrorStatus::NotRetryable)
}

pub async fn retrier(
    topic: &str,
    service_type: ServiceType,
    retry_wait_mins: &[u64],
    kafka_config: ClientConfig,
    kafka_tracer: &SdkTracer,
    shutdown_signal: impl Future<Output = ()>,
) -> eyre::Result<()> {
    let producer: FutureProducer = kafka_config
        .create()
        .wrap_err("Failed to create kafka producer")?;
    let consumer: Arc<StreamConsumer> = Arc::new(
        kafka_config
            .create()
            .wrap_err("Failed to create consumer")?,
    );

    {
        let topics = (0..retry_wait_mins.len())
            .map(|retry| format!("{}-retry-{}", topic, retry + 1))
            .collect::<Vec<_>>();

        let topics = topics.iter().map(|t| t.as_str()).collect::<Vec<_>>();

        consumer
            .subscribe(&topics)
            .wrap_err("Failed to subscribe to  topic")?;

        let mut topic_partitions_to_assign = TopicPartitionList::new();

        for topic in topics {
            topic_partitions_to_assign.add_partition(topic, 0);
        }

        consumer
            .assign(&topic_partitions_to_assign)
            .wrap_err("Failed to assign retry partitions to consumer")?;
    }

    let consumer_partition_queues = retry_wait_mins
        .iter()
        .enumerate()
        .map(|(retry, _)| {
            consumer.split_partition_queue(&format!("{}-retry-{}", topic, retry + 1), 0)
        })
        .collect::<Vec<_>>();

    let mut consumers = vec![RateLimitedConsumer::new(
        Some(consumer.stream()),
        tokio::time::sleep(Duration::from_secs(0)),
        0,
    )];

    consumers.extend(
        retry_wait_mins
            .iter()
            .zip(consumer_partition_queues.iter())
            .map(|(mins, queue)| {
                RateLimitedConsumer::new(
                    queue.as_ref().map(|q| q.stream()),
                    tokio::time::sleep(Duration::from_mins(*mins)),
                    *mins,
                )
            }),
    );

    let mut consumers = SelectAllStreams::new(consumers);

    tokio::pin!(shutdown_signal);

    while let Some(msg) = consumers.next().await {
        let msg = msg.wrap_err("Failed to receive message from kafka")?;
        let span = telemetry::span_from_kafka_msg(&kafka_tracer, &msg);

        let retry_record = match SafeFutureRunner::new(
            decode_retry_record(msg.payload()).instrument(span.clone()),
        )
        .await
        {
            Ok(record) => record,
            Err(e) => {
                handle_failure(
                    retry_wait_mins.len(),
                    &producer,
                    &msg,
                    service_type,
                    None,
                    e,
                )
                .await?;
                continue;
            }
        };

        if retry_record.retry_service != service_type {
            continue;
        }

        let payload_ref = retry_record.record.as_ref().map(|p| p.as_ref());

        match SafeFutureRunner::new(emails::process_message(payload_ref).instrument(span)).await {
            Err(e) => {
                handle_failure(
                    retry_wait_mins.len(),
                    &producer,
                    &msg,
                    service_type,
                    Some(retry_record),
                    e,
                )
                .await?;
            }
            Ok(_) => {
                consumer
                    .store_offset_from_message(&msg)
                    .wrap_err("Failed to commit message to stream")?;
            }
        }
    }

    Ok(())
}
