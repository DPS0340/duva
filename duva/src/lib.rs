mod actor_registry;
pub mod adapters;
pub mod domains;
mod init;
pub mod macros;
pub mod presentation;
pub mod services;
use actor_registry::ActorRegistry;
use anyhow::Result;
use domains::IoError;
use domains::caches::cache_manager::CacheManager;
use domains::cluster_actors::ClusterActor;
use domains::cluster_actors::commands::ClusterCommand;
use domains::cluster_actors::replication::ReplicationRole;
use domains::cluster_actors::replication::ReplicationState;
use domains::config_actors::config_manager::ConfigManager;
use domains::operation_logs::interfaces::TWriteAheadLog;
use domains::saves::snapshot::snapshot_loader::SnapshotLoader;
pub use init::Environment;
use prelude::PeerIdentifier;
use presentation::clients::ClientController;
use presentation::clients::authenticate;

use presentation::clusters::communication_manager::ClusterCommunicationManager;

use tokio::net::TcpListener;

pub mod prelude {
    pub use crate::domains::peers::identifier::PeerIdentifier;
    pub use anyhow;
    pub use bytes;
    pub use bytes::BytesMut;
    pub use tokio;
    pub use uuid;
}

pub mod clients;

// * StartUp Facade that manages invokes subsystems
pub struct StartUpFacade {
    registry: ActorRegistry,
}
make_smart_pointer!(StartUpFacade, ActorRegistry => registry);

impl StartUpFacade {
    pub fn new(
        config_manager: ConfigManager,
        env: &mut Environment,
        wal: impl TWriteAheadLog,
    ) -> Self {
        let replication_state =
            ReplicationState::new(env.repl_id.clone(), env.role.clone(), &env.host, env.port);
        let cache_manager = CacheManager::run_cache_actors(replication_state.hwm.clone());
        let cluster_actor_handler = ClusterActor::run(
            env.ttl_mills,
            env.topology_writer.take().unwrap(),
            env.hf_mills,
            replication_state,
            cache_manager.clone(),
            wal,
        );

        let registry = ActorRegistry {
            cluster_communication_manager: ClusterCommunicationManager(cluster_actor_handler),
            config_manager,
            cache_manager,
        };

        StartUpFacade { registry }
    }

    pub async fn run(self, env: Environment) -> Result<()> {
        tokio::spawn(Self::start_accepting_peer_connections(
            self.config_manager.peer_bind_addr(),
            self.registry.clone(),
        ));

        self.initialize_with_snapshot().await?;
        self.discover_cluster(env).await?;
        self.start_receiving_client_streams().await
    }

    async fn discover_cluster(&self, env: Environment) -> Result<(), anyhow::Error> {
        if let Some(seed) = env.seed_server {
            return self.registry.cluster_communication_manager.discover_cluster(seed).await;
        }

        for peer in env.pre_connected_peers {
            if self
                .registry
                .cluster_communication_manager
                .discover_cluster(peer.bind_addr)
                .await
                .is_ok()
            {
                break;
            }
        }

        Ok(())
    }

    async fn start_accepting_peer_connections(
        peer_bind_addr: String,
        registry: ActorRegistry,
    ) -> Result<()> {
        let peer_listener = TcpListener::bind(&peer_bind_addr)
            .await
            .expect("[ERROR] Failed to bind to peer address for listening");

        println!("Starting to accept peer connections");
        println!("listening peer connection on {}...", peer_bind_addr);

        loop {
            match peer_listener.accept().await {
                // ? how do we know if incoming connection is from a peer or replica?
                Ok((peer_stream, _socket_addr)) => {
                    if let Err(err) = registry
                        .cluster_communication_manager
                        .send(ClusterCommand::AcceptPeer { stream: peer_stream })
                        .await
                    {
                        println!("[ERROR] Failed to accept peer connection: {:?}", err);
                    }
                },

                Err(err) => {
                    if Into::<IoError>::into(err.kind()).should_break() {
                        break Ok(());
                    }
                },
            }
        }
    }

    /// Run while loop accepting stream and if the sentinel is received, abort the tasks
    async fn start_receiving_client_streams(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.config_manager.bind_addr()).await?;
        println!("start listening on {}", self.config_manager.bind_addr());
        let mut handles = Vec::with_capacity(100);

        //TODO refactor: authentication should be simplified
        while let Ok((stream, _)) = listener.accept().await {
            let mut peers = self.registry.cluster_communication_manager.get_peers().await?;
            peers.push(PeerIdentifier(self.registry.config_manager.bind_addr()));

            let is_leader = self.registry.cluster_communication_manager.role().await?
                == ReplicationRole::Leader;
            let Ok((reader, writer)) = authenticate(stream, peers, is_leader).await else {
                eprintln!("[ERROR] Failed to authenticate client stream");
                continue;
            };

            let observer =
                self.registry.cluster_communication_manager.subscribe_topology_change().await?;
            let write_handler = writer.run(observer);

            handles.push(tokio::spawn(reader.handle_client_stream(
                ClientController::new(self.registry.clone()),
                write_handler.clone(),
            )));
        }

        Ok(())
    }

    async fn initialize_with_snapshot(&self) -> Result<()> {
        if let Some(filepath) = self.registry.config_manager.try_filepath().await? {
            let snapshot = SnapshotLoader::load_from_filepath(filepath).await?;
            let (repl_id, hwm) = snapshot.extract_replication_info();
            // Reconnection case - set the replication info
            self.registry
                .cluster_communication_manager
                .send(ClusterCommand::SetReplicationInfo { replid: repl_id, hwm })
                .await?;
            self.registry.cache_manager.apply_snapshot(snapshot).await?;
        }
        Ok(())
    }
}
