use anyhow::Result;
use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub struct RespHandler {
    stream: tokio::net::TcpStream,
    buffer: BytesMut,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    SimpleString(String),
    BulkString(String),
    Array(Vec<Value>),
}
impl Value {
    fn serialize(&self) -> String {
        match self {
            Value::SimpleString(s) => format!("+{}\r\n", s),
            Value::BulkString(s) => format!("${}\r\n{}\r\n", s.len(), s),
            Value::Array(a) => {
                println!("dsd");
                let mut result = format!("*{}\r\n", a.len());
                for v in a {
                    result.push_str(&v.serialize());
                }
                result
            }
        }
    }
}

impl RespHandler {
    pub fn new(stream: tokio::net::TcpStream) -> Self {
        RespHandler {
            stream,
            buffer: BytesMut::with_capacity(512),
        }
    }

    pub async fn read_operation(&mut self) -> Result<Option<Value>> {
        let bytes_read = self.stream.read_buf(&mut self.buffer).await?;
        if bytes_read == 0 {
            return Ok(None);
        }
        let (v, _) = parse(self.buffer.split())?;

        Ok(Some(v))
    }

    pub async fn write_value(&mut self, value: Value) -> Result<()> {
        self.stream.write(value.serialize().as_bytes()).await?;
        Ok(())
    }
}

// parse based on RESP protocol
fn parse(buffer: BytesMut) -> Result<(Value, usize)> {
    match buffer[0] as char {
        '+' => parse_simple_string(buffer),
        '*' => parse_array(buffer),
        '$' => parse_bulk_string(buffer),
        _ => Err(anyhow::anyhow!("Not a known value type {:?}", buffer)),
    }
}

fn read_until_crlf(buffer: &[u8]) -> Option<(&[u8], usize)> {
    for i in 1..buffer.len() {
        if buffer[i - 1] == b'\r' && buffer[i] == b'\n' {
            return Some((&buffer[0..(i - 1)], i + 1));
        }
    }
    return None;
}

// +PING\r\n
fn parse_simple_string(buffer: BytesMut) -> Result<(Value, usize)> {
    if let Some((line, len)) = read_until_crlf(&buffer[1..]) {
        let string = String::from_utf8(line.to_vec())?;
        Ok((Value::SimpleString(string), len + 1))
    } else {
        Err(anyhow::anyhow!("Invalid simple string"))
    }
}

fn parse_array(buffer: BytesMut) -> Result<(Value, usize)> {
    let Some((line, mut len)) = read_until_crlf(&buffer[1..]) else {
        return Err(anyhow::anyhow!("Invalid bulk string"));
    };

    len += 1;

    let len_of_array = TryInto::<usize>::try_into(ConversionWrapper(line))?;
    let mut bulk_strings = Vec::with_capacity(len_of_array);

    for _ in 0..len_of_array {
        let (value, l) = parse(BytesMut::from(&buffer[len..]))?;
        bulk_strings.push(value);
        len += l;
    }

    return Ok((Value::Array(bulk_strings), len));
}

fn parse_bulk_string(buffer: BytesMut) -> Result<(Value, usize)> {
    let Some((line, mut len)) = read_until_crlf(&buffer[1..]) else {
        return Err(anyhow::anyhow!("Invalid bulk string"));
    };

    // add 1 to len to account for the first line
    len += 1;

    let bulk_str_len = TryInto::<usize>::try_into(ConversionWrapper(line))?;

    let bulk_str = &buffer[len..bulk_str_len + len];
    Ok((
        Value::BulkString(String::from_utf8(bulk_str.to_vec())?),
        len + bulk_str_len + 2, // to account for crlf, add 2
    ))
}

struct ConversionWrapper<T>(T);

impl<'a> TryFrom<ConversionWrapper<&'a [u8]>> for usize {
    type Error = anyhow::Error;

    fn try_from(value: ConversionWrapper<&'a [u8]>) -> std::result::Result<Self, Self::Error> {
        Ok(String::from_utf8(value.0.to_vec())?.parse::<usize>()?)
    }
}

#[test]
fn test_parse_simple_string() {
    // GIVEN
    let buffer = BytesMut::from("+OK\r\n");

    // WHEN
    let (value, len) = parse_simple_string(buffer).unwrap();

    // THEN
    assert_eq!(len, 5);
    assert_eq!(value, Value::SimpleString("OK".to_string()));
}

#[test]
fn test_parse_simple_string_ping() {
    // GIVEN
    let buffer = BytesMut::from("+PING\r\n");

    // WHEN
    let (value, len) = parse(buffer).unwrap();

    // THEN
    assert_eq!(len, 7);
    assert_eq!(value, Value::SimpleString("PING".to_string()));
}

#[test]
fn test_parse_bulk_string() {
    // GIVEN
    let buffer = BytesMut::from("$5\r\nhello\r\n");

    // WHEN
    let (value, len) = parse(buffer).unwrap();

    // THEN
    assert_eq!(len, 11);
    assert_eq!(value, Value::BulkString("hello".to_string()));
}

#[test]
fn test_parse_bulk_string_empty() {
    // GIVEN
    let buffer = BytesMut::from("$0\r\n\r\n");

    // WHEN
    let (value, len) = parse(buffer).unwrap();

    // THEN
    assert_eq!(len, 6);
    assert_eq!(value, Value::BulkString("".to_string()));
}

#[test]
fn test_parse_array() {
    // GIVEN
    let buffer = BytesMut::from("*2\r\n$5\r\nhello\r\n$5\r\nworld\r\n");

    // WHEN
    let (value, len) = parse(buffer).unwrap();

    // THEN
    assert_eq!(len, 26);
    assert_eq!(
        value,
        Value::Array(vec![
            Value::BulkString("hello".to_string()),
            Value::BulkString("world".to_string()),
        ])
    );
}
