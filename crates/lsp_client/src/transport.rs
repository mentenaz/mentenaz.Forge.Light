//! `Content-Length` framing for JSON-RPC messages.

use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Reads one framed message body. `Ok(None)` means clean EOF before a header.
pub async fn read_message<R: AsyncBufRead + AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<Option<Vec<u8>>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    let mut saw_header = false;
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return if saw_header {
                Err(io::ErrorKind::UnexpectedEof.into())
            } else {
                Ok(None)
            };
        }
        saw_header = true;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = Some(value.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length")
            })?);
        }
    }
    let length = length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(body))
}

pub async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn round_trip_with_split_chunks() {
        let (mut a, b) = tokio::io::duplex(8);
        let writer = tokio::spawn(async move {
            write_message(&mut a, b"{\"a\":1}").await.unwrap();
            write_message(&mut a, b"{\"b\":22}").await.unwrap();
        });
        let mut r = BufReader::new(b);
        assert_eq!(read_message(&mut r).await.unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(read_message(&mut r).await.unwrap().unwrap(), b"{\"b\":22}");
        writer.await.unwrap();
        assert!(read_message(&mut r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ignores_other_headers_and_rejects_bad_length() {
        let mut ok = BufReader::new(&b"Content-Type: x\r\ncontent-length: 2\r\n\r\n{}"[..]);
        assert_eq!(read_message(&mut ok).await.unwrap().unwrap(), b"{}");
        let mut bad = BufReader::new(&b"Content-Length: abc\r\n\r\n"[..]);
        assert!(read_message(&mut bad).await.is_err());
        let mut missing = BufReader::new(&b"X: y\r\n\r\n"[..]);
        assert!(read_message(&mut missing).await.is_err());
    }
}
