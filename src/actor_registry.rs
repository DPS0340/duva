use crate::domains::cluster_actors::commands::ClusterCommand;
use crate::domains::config_actors::config_manager::ConfigManager;
use crate::domains::saves::snapshot::snapshot_applier::SnapshotApplier;
use crate::presentation::cluster_in::connection_manager::ClusterConnectionManager;

use crate::{
    CacheManager, presentation::cluster_in::communication_manager::ClusterCommunicationManager,
};
use tokio::sync::mpsc::Sender;

#[derive(Clone)]
pub struct ActorRegistry {
    pub(crate) cache_manager: CacheManager,
    pub(crate) config_manager: ConfigManager,
    pub(crate) cluster_actor_handler: Sender<ClusterCommand>,
    pub(crate) snapshot_applier: SnapshotApplier,
}

impl ActorRegistry {
    pub(crate) fn cluster_communication_manager(&self) -> ClusterCommunicationManager {
        ClusterCommunicationManager(self.cluster_actor_handler.clone())
    }

    pub(crate) fn cluster_connection_manager(&self) -> ClusterConnectionManager {
        ClusterConnectionManager(self.cluster_communication_manager())
    }
}
