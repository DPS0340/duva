pub mod adapters;
pub mod macros;
pub mod services;
use anyhow::Result;
use services::actor_registry::ActorRegistry;
use services::client::manager::ClientManager;
use services::cluster::command::cluster_command::ClusterCommand;
use services::cluster::inbound::stream::InboundStream;
use services::cluster::manager::ClusterManager;
use services::cluster::replications::replication::IS_MASTER_MODE;
use services::config::init::get_env;
use services::config::manager::ConfigManager;
use services::error::IoError;
use services::statefuls::cache::manager::CacheManager;
use services::statefuls::snapshot::dump_loader::DumpLoader;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;
use tokio::net::TcpListener;

pub mod client_utils;

// * StartUp Facade that manages invokes subsystems
pub struct StartUpFacade {
    client_manager: ClientManager,
    registry: ActorRegistry,
    mode_change_watcher: tokio::sync::watch::Receiver<bool>,
}
make_smart_pointer!(StartUpFacade, ActorRegistry => registry);

impl StartUpFacade {
    pub fn new(config_manager: ConfigManager) -> Self {
        let _ = get_env();

        let (notifier, mode_change_watcher) =
            tokio::sync::watch::channel(IS_MASTER_MODE.load(Ordering::Acquire));
        let cluster_manager = ClusterManager::run(notifier);

        let registry = ActorRegistry::new(config_manager.clone(), cluster_manager.clone());
        let client_manager = ClientManager::new(registry.clone());
        StartUpFacade { client_manager, registry, mode_change_watcher }
    }

    pub async fn run(self, startup_notifier: impl TNotifyStartUp) -> Result<()> {
        tokio::spawn(Self::start_accepting_peer_connections(
            self.config_manager.peer_bind_addr(),
            self.registry.clone(),
        ));

        tokio::spawn(Self::initialize_with_dump(self.registry.clone(), startup_notifier));

        self.start_mode_specific_connection_handling().await
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
                    tokio::spawn({
                        let cluster_m = registry.cluster_manager.clone();
                        let repl_info = cluster_m.replication_info().await?;
                        let cache_m = registry.cache_manager.clone();
                        async move {
                            if let Err(err) = cluster_m
                                .accept_inbound_stream(
                                    InboundStream::new(peer_stream, repl_info),
                                    cache_m,
                                )
                                .await
                            {
                                println!("[ERROR] Failed to accept peer connection: {:?}", err);
                            }
                        }
                    });
                }

                Err(err) => {
                    if Into::<IoError>::into(err.kind()).should_break() {
                        break Ok(());
                    }
                }
            }
        }
    }

    async fn start_mode_specific_connection_handling(mut self) -> anyhow::Result<()> {
        let mut is_master_mode = self.cluster_mode();

        loop {
            let (stop_sentinel_tx, stop_sentinel_recv) = tokio::sync::oneshot::channel::<()>();

            if is_master_mode {
                let client_stream_listener =
                    TcpListener::bind(&self.config_manager.bind_addr()).await?;

                tokio::spawn(
                    self.client_manager
                        .clone()
                        .accept_client_connections(stop_sentinel_recv, client_stream_listener),
                );

                sleep(Duration::from_millis(2));
            } else {
                // Cancel all client connections only IF the cluster mode has changes to slave
                let _ = stop_sentinel_tx.send(());

                tokio::spawn({
                    self.cluster_manager.clone().discover_cluster(
                        self.config_manager.port,
                        self.cluster_manager.replication_info().await?.master_bind_addr(),
                    )
                });
            }

            self.wait_until_cluster_mode_changed().await?;

            is_master_mode = self.cluster_mode();
        }
    }

    // Park the task until the cluster mode changes - error means notifier has been dropped
    async fn wait_until_cluster_mode_changed(&mut self) -> anyhow::Result<()> {
        self.mode_change_watcher.changed().await?;
        Ok(())
    }
    fn cluster_mode(&mut self) -> bool {
        *self.mode_change_watcher.borrow_and_update()
    }

    async fn initialize_with_dump(
        registry: ActorRegistry,
        startup_notifier: impl TNotifyStartUp,
    ) -> Result<()> {
        if let Some(filepath) = registry.config_manager.try_filepath().await? {
            let dump = DumpLoader::load(filepath).await?;
            if let Some((repl_id, offset)) = dump.extract_replication_info() {
                //  TODO reconnect! - echo
                registry
                    .cluster_manager
                    .send(ClusterCommand::SetReplicationInfo { master_repl_id: repl_id, offset })
                    .await?;
            };
            registry
                .cache_manager
                .dump_cache(dump, registry.ttl_manager, registry.config_manager.startup_time)
                .await?;
        }

        startup_notifier.notify_startup();
        Ok(())
    }
}

pub trait TNotifyStartUp: Send + 'static {
    fn notify_startup(&self);
}

impl TNotifyStartUp for () {
    fn notify_startup(&self) {}
}
