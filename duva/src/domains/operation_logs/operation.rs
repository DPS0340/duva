use crate::domains::query_parsers::{QueryIO, deserialize};
use bytes::{Bytes, BytesMut};

#[derive(Debug, Clone, PartialEq, Eq, bincode::Encode, bincode::Decode)]
pub struct WriteOperation {
    pub(crate) request: WriteRequest,
    pub(crate) log_index: u64,
    pub(crate) term: u64,
}

/// Operations that appear in the Append-Only File (WAL).
/// Client request is converted to WriteOperation and then it turns into WriteOp when it gets offset
#[derive(Debug, Clone, PartialEq, Eq, bincode::Encode, bincode::Decode)]
pub enum WriteRequest {
    Set { key: String, value: String },
    SetWithExpiry { key: String, value: String, expires_at: u64 },
    Delete { keys: Vec<String> },
}

impl WriteOperation {
    pub(crate) fn serialize(self) -> Bytes {
        QueryIO::WriteOperation(self).serialize()
    }
}

impl WriteRequest {
    /// Deserialize `WriteOperation`s from the given bytes.
    pub(crate) fn deserialize(mut bytes: BytesMut) -> anyhow::Result<Vec<WriteOperation>> {
        let mut ops: Vec<WriteOperation> = Vec::new();

        while !bytes.is_empty() {
            let (query, consumed) = deserialize(bytes.clone())?;
            bytes = bytes.split_off(consumed);

            let QueryIO::WriteOperation(write_operation) = query else {
                return Err(anyhow::anyhow!("expected replicate"));
            };
            ops.push(write_operation);
        }
        Ok(ops)
    }
}
