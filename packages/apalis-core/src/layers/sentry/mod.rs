use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use sentry_core::protocol;
use tower::Layer;
use tower::Service;

use crate::error::Error;
use crate::request::Request;
use crate::storage::job::{Job, JobId};

/// Tower Layer that logs Job Details.
///
/// The Service created by this Layer can also optionally start a new
/// performance monitoring transaction for each incoming request,
/// continuing the trace based on incoming distributed tracing headers.
///
/// The created transaction will automatically use `J::NAME` as its name.
/// This is sometimes not desirable, In which case, users should manually override the transaction name
/// in the request handler using the [`Scope::set_transaction`](sentry_core::Scope::set_transaction)
/// method.
#[derive(Clone, Default, Debug)]
pub struct SentryJobLayer;

impl SentryJobLayer {
    /// Creates a new Layer that only logs Job details.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Tower Service that logs Job details.
///
/// The Service can also optionally start a new performance monitoring transaction
/// for each incoming request, continuing the trace based on J::NAME
#[derive(Clone, Debug)]
pub struct SentryJobService<S> {
    service: S,
}

impl<S> Layer<S> for SentryJobLayer {
    type Service = SentryJobService<S>;

    fn layer(&self, service: S) -> Self::Service {
        Self::Service { service }
    }
}

struct JobDetails {
    job_id: JobId,
    current_attempt: i32,
    job_type: String,
}

pin_project_lite::pin_project! {
    /// The Future returned from [`SentryJobService`].
    pub struct SentryHttpFuture<F> {
        on_first_poll: Option<(
            JobDetails,
            sentry_core::TransactionContext
        )>,
        transaction: Option<(
            sentry_core::TransactionOrSpan,
            Option<sentry_core::TransactionOrSpan>,
        )>,
        #[pin]
        future: F,
    }
}

impl<F, Res> Future for SentryHttpFuture<F>
where
    F: Future<Output = Result<Res, Error>> + 'static,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let slf = self.project();
        if let Some((job_details, trx_ctx)) = slf.on_first_poll.take() {
            let jid = job_details.job_id.clone();
            sentry_core::configure_scope(|scope| {
                scope.add_event_processor(move |mut event| {
                    event.event_id = uuid::Uuid::from_u128(jid.inner().0);
                    Some(event)
                });
                scope.set_tag("job_type", job_details.job_type.to_string());
                let mut details = std::collections::BTreeMap::new();
                details.insert(
                    String::from("job_id"),
                    job_details.job_id.to_string().into(),
                );
                details.insert(
                    String::from("current_attempt"),
                    job_details.current_attempt.into(),
                );
                scope.set_context("job", sentry_core::protocol::Context::Other(details));

                let transaction: sentry_core::TransactionOrSpan =
                    sentry_core::start_transaction(trx_ctx).into();
                let parent_span = scope.get_span();
                scope.set_span(Some(transaction.clone()));
                *slf.transaction = Some((transaction, parent_span));
            });
        }
        match slf.future.poll(cx) {
            Poll::Ready(res) => {
                if let Some((transaction, parent_span)) = slf.transaction.take() {
                    if transaction.get_status().is_none() {
                        let status = match &res {
                            Ok(_) => protocol::SpanStatus::Ok,
                            Err(err) => {
                                sentry_core::capture_error(err);
                                protocol::SpanStatus::InternalError
                            }
                        };
                        transaction.set_status(status);
                    }
                    transaction.finish();
                    sentry_core::configure_scope(|scope| scope.set_span(parent_span));
                }
                Poll::Ready(res)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S, J, F, Res> Service<Request<J>> for SentryJobService<S>
where
    S: Service<Request<J>, Response = Res, Error = Error, Future = F>,
    F: Future<Output = Result<Res, Error>> + 'static,
    J: Job,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = SentryHttpFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: Request<J>) -> Self::Future {
        let op = J::NAME;
        let trx_ctx = sentry_core::TransactionContext::new(op, "apalis.job");
        let job_type = std::any::type_name::<J>().to_string();
        let ctx = request.get::<crate::storage::context::Context>().expect("Missing context");
        let job_details = JobDetails {
            job_id: ctx.id().clone(),
            current_attempt: ctx.attempts(),
            job_type,
        };

        SentryHttpFuture {
            on_first_poll: Some((job_details, trx_ctx)),
            transaction: None,
            future: self.service.call(request),
        }
    }
}
