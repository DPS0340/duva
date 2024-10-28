use std::hash::{DefaultHasher, Hash, Hasher};

use super::value::Value;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Args(pub(crate) Vec<Value>);
impl Args {
    pub(crate) fn extract_command(value: Value) -> Result<(String, Self)> {
        match value {
            Value::Array(a) => Ok((
                unpack_bulk_str(a.first().unwrap().clone())?,
                Self(a.into_iter().skip(1).collect()),
            )),
            _ => Err(anyhow::anyhow!("Unexpected command format")),
        }
    }

    pub(crate) fn take_shard_key(&self, num_shards: usize) -> Result<usize> {
        let Value::BulkString(key) = self.first()? else {
            return Err(anyhow::anyhow!("Invalid arguments"));
        };
        // hasher
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash_result = hasher.finish() as usize;
        Ok(hash_result % num_shards)
    }
    pub(crate) fn first(&self) -> Result<Value> {
        self.0.first().cloned().ok_or(anyhow::anyhow!("No value"))
    }

    pub(crate) fn take_set_args(&self) -> Result<(Value, &Value, Option<&Value>)> {
        let key = self.first()?;
        let value = self.0.get(1).ok_or(anyhow::anyhow!("No value"))?;
        //expire sig must be px or PX
        match (self.0.get(2), self.0.get(3)) {
            (Some(Value::BulkString(sig)), Some(expiry)) => {
                if sig.to_lowercase() != "px" {
                    return Err(anyhow::anyhow!("Invalid arguments"));
                }
                Ok((key, value, Some(expiry)))
            }
            (None, _) => Ok((key, value, None)),
            _ => Err(anyhow::anyhow!("Invalid arguments")),
        }
    }
}

fn unpack_bulk_str(value: Value) -> Result<String> {
    match value {
        Value::BulkString(s) => Ok(s.to_lowercase()),
        _ => Err(anyhow::anyhow!("Expected command to be a bulk string")),
    }
}
