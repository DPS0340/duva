use tokio::sync::{mpsc, oneshot};

use crate::{
    domains::ttl::manager::TtlSchedulerManager,
    services::{query_io::QueryIO, statefuls::snapshot::save::command::SaveCommand},
};

use super::cache_objects::CacheEntry;

pub enum CacheCommand {
    Set { cache_entry: CacheEntry, ttl_sender: TtlSchedulerManager },
    Save { outbox: mpsc::Sender<SaveCommand> },
    Get { key: String, sender: oneshot::Sender<QueryIO> },
    Keys { pattern: Option<String>, sender: oneshot::Sender<QueryIO> },
    Delete(String),
    StopSentinel,
}
