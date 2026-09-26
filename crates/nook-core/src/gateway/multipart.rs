//! A small `multipart/form-data` reader (RFC 7578) for the transcription upload, standing in for
//! Spring's multipart resolver: the parts' names, file names and bytes.

use bytes::Bytes;

/// One part of a form.
pub(crate) struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub data: Bytes,
}

impl Part {
    /// The part as text (a plain form field).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

/// The `boundary` parameter of a `multipart/form-data` Content-Type.
pub(crate) fn boundary(content_type: &str) -> Option<String> {
    let mut params = content_type.split(';');
    let kind = params.next()?.trim();
    if !kind.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    params
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("boundary"))
        .map(|(_, v)| v.trim().trim_matches('"').to_string())
        .filter(|b| !b.is_empty())
}

/// The parts of a body, or None when it is not a well-formed form with that boundary.
pub(crate) fn parse(body: &Bytes, boundary: &str) -> Option<Vec<Part>> {
    let delimiter = format!("--{boundary}");
    let delimiter = delimiter.as_bytes();
    let mut at = find(body, delimiter, 0)? + delimiter.len();
    let mut parts = Vec::new();
    loop {
        // After a delimiter: "--" closes the form, a line break opens the next part.
        if body[at..].starts_with(b"--") {
            return Some(parts);
        }
        at = skip_line_break(body, at)?;
        let head_end = find(body, b"\r\n\r\n", at)?;
        let head = String::from_utf8_lossy(&body[at..head_end]).into_owned();
        let data_start = head_end + 4;
        let mut next = Vec::with_capacity(delimiter.len() + 2);
        next.extend_from_slice(b"\r\n");
        next.extend_from_slice(delimiter);
        let data_end = find(body, &next, data_start)?;
        let (name, filename) = disposition(&head)?;
        parts.push(Part {
            name,
            filename,
            data: body.slice(data_start..data_end),
        });
        at = data_end + next.len();
    }
}

fn skip_line_break(body: &[u8], at: usize) -> Option<usize> {
    let rest = &body[at..];
    if rest.starts_with(b"\r\n") {
        Some(at + 2)
    } else {
        None
    }
}

/// The `name` and `filename` of a part's `Content-Disposition: form-data` header.
fn disposition(head: &str) -> Option<(String, Option<String>)> {
    let line = head.split("\r\n").find(|l| {
        l.split(':')
            .next()
            .is_some_and(|k| k.trim().eq_ignore_ascii_case("content-disposition"))
    })?;
    let value = line.split_once(':')?.1;
    let mut name = None;
    let mut filename = None;
    for param in value.split(';').skip(1) {
        let Some((k, v)) = param.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').to_string();
        match k.trim().to_ascii_lowercase().as_str() {
            "name" => name = Some(v),
            "filename" => filename = Some(v),
            _ => {}
        }
    }
    Some((name?, filename))
}

/// The first place `needle` occurs in `hay` at or after `from`.
fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    let mut i = from;
    while i + needle.len() <= hay.len() {
        let offset = hay[i..=hay.len() - needle.len()]
            .iter()
            .position(|b| *b == first)?;
        i += offset;
        if hay[i..].starts_with(needle) {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_reads_as_its_parts() {
        let ct = "multipart/form-data; boundary=\"----x1\"";
        let b = boundary(ct).unwrap();
        assert_eq!(b, "----x1");
        assert_eq!(boundary("application/json"), None);
        let body = Bytes::from_static(
            b"preamble\r\n------x1\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-small\r\n------x1\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\r\n--\x00\x01\r\n------x1--\r\n",
        );
        let parts = parse(&body, &b).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "model");
        assert_eq!(parts[0].text(), "whisper-small");
        assert_eq!(parts[0].filename, None);
        assert_eq!(parts[1].name, "file");
        assert_eq!(parts[1].filename.as_deref(), Some("a.wav"));
        assert_eq!(&parts[1].data[..], b"RIFF\r\n--\x00\x01");
        assert!(parse(&Bytes::from_static(b"no form here"), &b).is_none());
        assert!(parse(
            &Bytes::from_static(
                b"------x1\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\ncut off"
            ),
            &b
        )
        .is_none());
    }
}
