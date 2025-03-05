use super::response::ConnectionResponse;
use crate::domains::cluster_actors::commands::AddPeer;
use crate::domains::cluster_actors::commands::ClusterCommand;
use crate::domains::cluster_actors::replication::ReplicationInfo;
use crate::domains::peers::connected_peer_info::ConnectedPeerInfo;
use crate::domains::peers::identifier::PeerIdentifier;
use crate::domains::peers::kind::PeerKind;
use crate::domains::peers::peer::Peer;
use crate::domains::saves::snapshot::snapshot_applier::SnapshotApplier;
use crate::presentation::cluster_in::connection_manager::ClusterConnectionManager;
use crate::services::interface::TRead;
use crate::services::interface::TWrite;
use crate::{make_smart_pointer, write_array};
use anyhow::Context;
use tokio::net::TcpStream;
use tokio::sync::mpsc::Sender;

// The following is used only when the node is in follower mode
pub(crate) struct OutboundStream {
    pub(crate) stream: TcpStream,
    pub(crate) my_repl_info: ReplicationInfo,

    connected_node_info: Option<ConnectedPeerInfo>,
    connect_to: PeerIdentifier,
}
make_smart_pointer!(OutboundStream, TcpStream => stream);

impl OutboundStream {
    pub(crate) async fn new(
        connect_to: PeerIdentifier,
        my_repl_info: ReplicationInfo,
    ) -> anyhow::Result<Self> {
        Ok(OutboundStream {
            stream: TcpStream::connect(&connect_to.cluster_bind_addr()).await?,
            my_repl_info,
            connected_node_info: None,
            connect_to: connect_to.to_string().into(),
        })
    }
    pub async fn establish_connection(mut self, self_port: u16) -> anyhow::Result<Self> {
        // Trigger
        self.write(write_array!("PING")).await?;
        let mut ok_count = 0;
        let mut connection_info = ConnectedPeerInfo {
            id: Default::default(),
            leader_repl_id: Default::default(),
            offset: Default::default(),
            peer_list: Default::default(),
        };

        loop {
            let res = self.read_values().await?;
            for query in res {
                match ConnectionResponse::try_from(query)? {
                    ConnectionResponse::PONG => {
                        let msg = write_array!("REPLCONF", "listening-port", self_port.to_string());
                        self.write(msg).await?
                    },
                    ConnectionResponse::OK => {
                        ok_count += 1;
                        let msg = {
                            match ok_count {
                                1 => Ok(write_array!("REPLCONF", "capa", "psync2")),
                                // "?" here means the server is undecided about their leader. and -1 is the offset that follower is aware of
                                2 => Ok(write_array!(
                                    "PSYNC",
                                    self.my_repl_info.leader_repl_id.clone(),
                                    self.my_repl_info.hwm.to_string()
                                )),
                                _ => Err(anyhow::anyhow!("Unexpected OK count")),
                            }
                        }?;
                        self.write(msg).await?
                    },
                    ConnectionResponse::FULLRESYNC { id, repl_id, offset } => {
                        connection_info.leader_repl_id = repl_id.into();
                        connection_info.offset = offset;
                        connection_info.id = id.into();
                        println!("given id {}", connection_info.id);
                        println!("[INFO] Three-way handshake completed")
                    },
                    ConnectionResponse::PEERS(peer_list) => {
                        println!("[INFO] Received peer list: {:?}", peer_list);
                        connection_info.peer_list = peer_list;
                        self.connected_node_info = Some(connection_info);
                        return Ok(self);
                    },
                }
            }
        }
    }

    pub(crate) async fn set_replication_info(
        self,
        cluster_manager: &ClusterConnectionManager,
    ) -> anyhow::Result<Self> {
        if *self.my_repl_info.leader_repl_id == "?" {
            let connected_node_info = self
                .connected_node_info
                .as_ref()
                .context("Connected node info not found. Cannot set replication id")?;

            cluster_manager
                .send(ClusterCommand::SetReplicationInfo {
                    leader_repl_id: connected_node_info.leader_repl_id.clone(),
                    hwm: 0,
                })
                .await?;
        }
        Ok(self)
    }
    pub(crate) fn create_peer_cmd(
        self,
        cluster_actor_handler: Sender<ClusterCommand>,
        snapshot_applier: SnapshotApplier,
    ) -> anyhow::Result<(ClusterCommand, Vec<PeerIdentifier>)> {
        let mut connection_info =
            self.connected_node_info.context("Connected node info not found")?;
        let peer_list = connection_info.list_peer_binding_addrs();

        let kind =
            PeerKind::connected_peer_kind(&self.my_repl_info.leader_repl_id, connection_info);

        let peer = Peer::create(self.stream, kind.clone(), cluster_actor_handler, snapshot_applier);

        Ok((ClusterCommand::AddPeer(AddPeer { peer_id: self.connect_to, peer }), peer_list))
    }
}
