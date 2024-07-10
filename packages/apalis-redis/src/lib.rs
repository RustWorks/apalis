#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! apalis storage using Redis as a backend
//! ```rust,no_run
//! use apalis::prelude::*;
//! use apalis_redis::{RedisStorage, Config};
//! use email_service::send_email;
//!
//! #[tokio::main]
//! async fn main() {
//!     let conn = apalis_redis::connect("redis://127.0.0.1/").await.unwrap();
//!     let storage = RedisStorage::new(conn);
//!     Monitor::<TokioExecutor>::new()
//!        .register(
//!            WorkerBuilder::new("tasty-pear")
//!                .backend(storage.clone())
//!                .build_fn(send_email),
//!        )
//!        .run()
//!        .await
//!        .unwrap();
//! }
//! ```

mod storage;
pub use storage::connect;
pub use storage::Config;
pub use storage::RedisCodec;
pub use storage::RedisJob;
pub use storage::RedisQueueInfo;
pub use storage::RedisStorage;
