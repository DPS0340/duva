use super::{ClientController, request::ClientRequest};
use crate::{
    domains::{IoError, cluster_actors::session::SessionRequest, query_parsers::QueryIO},
    prelude::PeerIdentifier,
    services::interface::{TRead, TWrite},
};
use tokio::{
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
    sync::mpsc::Sender,
};
use uuid::Uuid;
pub struct ClientStreamReader {
    pub(crate) r: OwnedReadHalf,
    pub(crate) client_id: Uuid,
}

impl ClientStreamReader {
    pub(crate) async fn extract_query(&mut self) -> Result<Vec<ClientRequest>, IoError> {
        let query_ios = self.r.read_values().await?;

        query_ios
            .into_iter()
            .map(|query_io| match query_io {
                QueryIO::Array(value) => {
                    let req = ClientRequest::from_user_input(value, None)
                        .map_err(|e| IoError::Custom(e.to_string()))?;
                    Ok(req)
                },
                QueryIO::SessionRequest { request_id, value } => {
                    let req = ClientRequest::from_user_input(
                        value,
                        Some(SessionRequest::new(request_id, self.client_id)),
                    )
                    .map_err(|e| IoError::Custom(e.to_string()))?;
                    Ok(req)
                },
                _ => Err(IoError::Custom("Unexpected command format".to_string())),
            })
            .collect()
    }

    pub(crate) async fn handle_client_stream(
        mut self,
        handler: ClientController,
        sender: Sender<QueryIO>,
    ) {
        loop {
            //TODO check on current mode of the node for every query? or get notified when change is made?

            match self.extract_query().await {
                Ok(requests) => {
                    let results = match handler.maybe_consensus_then_execute(requests).await {
                        Ok(results) => results,

                        // ! One of the following errors can be returned:
                        // ! consensus or handler or commit
                        Err(e) => {
                            eprintln!("[ERROR] {:?}", e);
                            let _ = sender.send(QueryIO::Err(e.to_string())).await;
                            continue;
                        },
                    };

                    for res in results {
                        if sender.send(res).await.is_err() {
                            break;
                        }
                    }
                },

                Err(err) => {
                    if err.should_break() {
                        eprintln!("[INFO] {}", err);
                        return;
                    } else {
                        let _ = sender.send(QueryIO::Err(err.to_string())).await;
                    }
                },
            }
        }
    }
}

pub struct ClientStreamWriter(pub(crate) OwnedWriteHalf);
impl ClientStreamWriter {
    pub(crate) async fn write(&mut self, query_io: QueryIO) -> Result<(), IoError> {
        self.0.write(query_io).await
    }

    pub(crate) fn run(
        mut self,
        mut topology_observer: tokio::sync::broadcast::Receiver<Vec<PeerIdentifier>>,
    ) -> Sender<QueryIO> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                if let Err(e) = self.write(data).await {
                    if e.should_break() {
                        break;
                    }
                }
            }
        });

        tokio::spawn({
            let tx = tx.clone();
            async move {
                while let Ok(peers) = topology_observer.recv().await {
                    let _ = tx.send(QueryIO::TopologyChange(peers)).await;
                }
            }
        });
        tx
    }
}
