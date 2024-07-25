use std::{fmt::Debug, marker::PhantomData, time::Duration};

use apalis::{layers::tracing::TraceLayer, prelude::*};

use apalis_redis::{self, Config, RedisCodec, RedisJob};

use apalis_core::{
    codec::json::JsonCodec,
    layers::{Ack, AckLayer},
};
use email_service::{send_email, Email};
use futures::{channel::mpsc, SinkExt};
use rsmq_async::{Rsmq, RsmqConnection, RsmqError};
use tokio::time::sleep;
use tracing::{error, info};

struct RedisMq<T> {
    conn: Rsmq,
    msg_type: PhantomData<T>,
    config: Config,
    codec: RedisCodec<T>,
}

// Manually implement Clone for RedisMq
impl<T> Clone for RedisMq<T> {
    fn clone(&self) -> Self {
        RedisMq {
            conn: self.conn.clone(),
            msg_type: PhantomData,
            config: self.config.clone(),
            codec: self.codec.clone(),
        }
    }
}

impl<M: Send + 'static, Res> Backend<Request<M>, Res> for RedisMq<M> {
    type Stream = RequestStream<Request<M>>;

    type Layer = AckLayer<Self, M, Res>;

    fn poll<Svc>(mut self, _worker_id: WorkerId) -> Poller<Self::Stream, Self::Layer> {
        let (mut tx, rx) = mpsc::channel(self.config.get_buffer_size());
        let stream: RequestStream<Request<M>> = Box::pin(rx);
        let layer = AckLayer::new(self.clone());
        let heartbeat = async move {
            loop {
                sleep(*self.config.get_poll_interval()).await;
                let msg: Option<Request<M>> = self
                    .conn
                    .receive_message("email", None)
                    .await
                    .unwrap()
                    .map(|r| {
                        let mut req: Request<_> = self.codec.decode(&r.message).unwrap().into();
                        req.insert(r.id);
                        req
                    });
                tx.send(Ok(msg)).await.unwrap();
            }
        };
        Poller::new_with_layer(stream, heartbeat, layer)
    }
}

impl<T: Send, Res: Debug + Send + Sync> Ack<T, Res> for RedisMq<T> {
    type Context = String;

    type AckError = RsmqError;

    async fn ack(
        &mut self,
        ctx: &Self::Context,
        res: &Result<Res, apalis_core::error::Error>,
    ) -> Result<(), Self::AckError> {
        println!("Attempting to ACK {:?}", res);
        self.conn.delete_message("email", &ctx).await?;
        Ok(())
    }
}

impl<Message: Send + 'static> MessageQueue<Message> for RedisMq<Message> {
    type Error = RsmqError;

    async fn enqueue(&mut self, message: Message) -> Result<(), Self::Error> {
        let bytes = self
            .codec
            .encode(&RedisJob::new(message, Default::default()))
            .unwrap();
        self.conn.send_message("email", bytes, None).await?;
        Ok(())
    }

    async fn dequeue(&mut self) -> Result<Option<Message>, Self::Error> {
        let codec = self.codec.clone();
        Ok(self.conn.receive_message("email", None).await?.map(|r| {
            let req: Request<Message> = codec.decode(&r.message).unwrap().into();
            req.take()
        }))
    }

    async fn size(&mut self) -> Result<usize, Self::Error> {
        self.conn
            .get_queue_attributes("email")
            .await?
            .msgs
            .try_into()
            .map_err(|_| RsmqError::InvalidFormat("Could not convert to usize".to_owned()))
    }
}

async fn produce_jobs(mq: &mut RedisMq<Email>) -> anyhow::Result<()> {
    for index in 0..1 {
        mq.enqueue(Email {
            to: index.to_string(),
            text: "Test background job from apalis".to_string(),
            subject: "Background email job".to_string(),
        })
        .await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    std::env::set_var("RUST_LOG", "debug");

    tracing_subscriber::fmt::init();

    let mut conn = rsmq_async::Rsmq::new(Default::default()).await?;
    let _ = conn.create_queue("email", None, None, None).await;
    let mut mq = RedisMq {
        conn,
        msg_type: PhantomData,
        codec: RedisCodec::new(Box::new(JsonCodec)),
        config: Config::default(),
    };
    produce_jobs(&mut mq).await?;

    let worker = WorkerBuilder::new("rango-tango")
        .layer(TraceLayer::new())
        .backend(mq)
        .build_fn(send_email);

    Monitor::<TokioExecutor>::new()
        .register_with_count(2, worker)
        .on_event(|e| {
            let worker_id = e.id();
            match e.inner() {
                Event::Start => {
                    info!("Worker [{worker_id}] started");
                }
                Event::Error(e) => {
                    error!("Worker [{worker_id}] encountered an error: {e}");
                }

                Event::Exit => {
                    info!("Worker [{worker_id}] exited");
                }
                _ => {}
            }
        })
        .shutdown_timeout(Duration::from_millis(5000))
        .run_with_signal(async {
            tokio::signal::ctrl_c().await?;
            info!("Monitor starting shutdown");
            Ok(())
        })
        .await?;
    info!("Monitor shutdown complete");
    Ok(())
}
