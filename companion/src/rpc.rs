//! LSP base-protocol framing (`Content-Length` headers + JSON body).

use std::io::{BufRead, Error, ErrorKind, Result, Write};

/// Reads one framed message. Returns `None` on clean EOF.
pub fn read_message(r: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            content_length = v.trim().parse().ok();
        }
    }
    let len = content_length
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "missing Content-Length header"))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

pub fn write_message(w: &mut impl Write, body: &[u8]) -> Result<()> {
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(body)?;
    w.flush()
}
