use super::request::HandShakeRequest;
use super::request::HandShakeRequestEnum;
use crate::domains::IoError;
use crate::domains::cluster_actors::commands::AddPeer;
use crate::domains::cluster_actors::commands::ClusterCommand;
use crate::domains::cluster_actors::replication::ReplicationId;
use crate::domains::cluster_actors::replication::ReplicationState;
use crate::domains::peers::connected_peer_info::ConnectedPeerInfo;
use crate::domains::peers::identifier::PeerIdentifier;
use crate::domains::peers::peer::Peer;
use crate::domains::peers::peer::PeerState;
use crate::domains::query_parsers::QueryIO;
use crate::presentation::clusters::communication_manager::ClusterCommunicationManager;
use crate::presentation::clusters::listeners::start_listen;
use crate::services::interface::TRead;
use crate::services::interface::TWrite;
use std::sync::atomic::Ordering;
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::tcp::OwnedWriteHalf;

// The following is used only when the node is in leader mode
pub(crate) struct InboundStream {
    r: OwnedReadHalf,
    w: OwnedWriteHalf,
    pub(crate) self_repl_info: ReplicationState,
}

impl InboundStream {
    pub(crate) fn new(stream: TcpStream, self_repl_info: ReplicationState) -> Self {
        let (read, write) = stream.into_split();
        Self { r: read, w: write, self_repl_info }
    }
    pub(crate) async fn recv_handshake(&mut self) -> anyhow::Result<ConnectedPeerInfo> {
        self.recv_ping().await?;

        let port = self.recv_replconf_listening_port().await?;

        // TODO find use of capa?
        let _capa_val_vec = self.recv_replconf_capa().await?;

        // TODO check repl_id is '?' or of mine. If not, consider incoming as peer
        let (peer_leader_repl_id, peer_hwm) = self.recv_psync().await?;

        let addr = self.r.peer_addr().map_err(|error| Into::<IoError>::into(error.kind()))?;
        Ok(ConnectedPeerInfo {
            id: PeerIdentifier::new(&addr.ip().to_string(), port),
            replid: peer_leader_repl_id,
            hwm: peer_hwm,
            peer_list: vec![],
        })
    }

    async fn recv_ping(&mut self) -> anyhow::Result<()> {
        let cmd = self.extract_cmd().await?;
        cmd.match_query(HandShakeRequestEnum::Ping)?;

        self.w.write(QueryIO::SimpleString("PONG".into())).await?;
        Ok(())
    }

    async fn recv_replconf_listening_port(&mut self) -> anyhow::Result<u16> {
        let mut cmd = self.extract_cmd().await?;

        let port = cmd.extract_listening_port()?;

        self.w.write(QueryIO::SimpleString("OK".into())).await?;

        Ok(port)
    }

    async fn recv_replconf_capa(&mut self) -> anyhow::Result<Vec<(String, String)>> {
        let mut cmd = self.extract_cmd().await?;
        let capa_val_vec = cmd.extract_capa()?;
        self.w.write(QueryIO::SimpleString("OK".into())).await?;
        Ok(capa_val_vec)
    }
    async fn recv_psync(&mut self) -> anyhow::Result<(ReplicationId, u64)> {
        let mut cmd = self.extract_cmd().await?;
        let (inbound_repl_id, offset) = cmd.extract_psync()?;

        // ! Assumption, if self replid is not set at this point but still receives inbound stream, this is leader.

        let (id, self_leader_replid, self_leader_repl_offset) = (
            self.self_repl_info.self_identifier(),
            self.self_repl_info.replid.clone(),
            self.self_repl_info.hwm.load(Ordering::Relaxed),
        );

        self.w
            .write(QueryIO::SimpleString(format!(
                "FULLRESYNC {} {} {}",
                id, self_leader_replid, self_leader_repl_offset
            )))
            .await?;
        self.recv_ok().await?;
        Ok((inbound_repl_id, offset))
    }

    async fn extract_cmd(&mut self) -> anyhow::Result<HandShakeRequest> {
        let mut query_io = self.r.read_values().await?;
        HandShakeRequest::new(query_io.swap_remove(0))
    }

    pub(crate) async fn disseminate_peers(
        &mut self,
        peers: Vec<PeerIdentifier>,
    ) -> anyhow::Result<()> {
        self.w
            .write(QueryIO::SimpleString(format!(
                "PEERS {}",
                peers.into_iter().map(|x| x.0).collect::<Vec<String>>().join(" ")
            )))
            .await?;

        self.recv_ok().await?;
        Ok(())
    }

    async fn recv_ok(&mut self) -> anyhow::Result<()> {
        let mut query_io = self.r.read_values().await?;
        let Some(query) = query_io.pop() else {
            return Err(anyhow::anyhow!("No query found"));
        };
        let QueryIO::SimpleString(val) = query else {
            return Err(anyhow::anyhow!("Invalid query"));
        };
        if val.to_lowercase() != "ok" {
            return Err(anyhow::anyhow!("Invalid response"));
        }
        Ok(())
    }

    pub(crate) fn decide_peer_kind(&self, connected_peer_info: &ConnectedPeerInfo) -> PeerState {
        PeerState::decide_peer_kind(&self.self_repl_info.replid, connected_peer_info)
    }

    pub(crate) async fn prepare_add_peer_cmd(
        mut self,
        ccm: ClusterCommunicationManager,
        connected_peer_info: ConnectedPeerInfo,
        callback: tokio::sync::oneshot::Sender<()>,
    ) -> anyhow::Result<ClusterCommand> {
        let peer_state = self.decide_peer_kind(&connected_peer_info);

        // conditional sync
        if let PeerState::Replica { .. } = &peer_state {
            if let ReplicationId::Undecided = connected_peer_info.replid {
                let logs = ccm.fetch_logs_for_sync().await?;
                self.w.write_io(logs).await?;
            }
        }

        let kill_switch = start_listen(self.r, (connected_peer_info.id).to_string(), ccm.0);
        let peer = Peer::new((connected_peer_info.id).to_string(), self.w, peer_state, kill_switch);

        Ok(ClusterCommand::AddPeer(AddPeer { peer_id: connected_peer_info.id, peer }, callback))
    }
}
