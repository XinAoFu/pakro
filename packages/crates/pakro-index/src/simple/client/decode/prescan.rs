//! Bounded HTML encoding prescan, separate from the full HTML5 file parser.
//!
//! Implements byte/attribute contexts from the WHATWG prescan algorithm over at
//! most 1024 bytes. This never scans a full response or creates HTML file facts.

use super::{Encoding, meta_encoding};

fn space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}
fn prefix(bytes: &[u8], value: &[u8]) -> bool {
    bytes
        .get(..value.len())
        .is_some_and(|bytes| bytes.eq_ignore_ascii_case(value))
}
fn attribute(bytes: &[u8], position: &mut usize) -> Option<(Vec<u8>, Vec<u8>)> {
    while bytes
        .get(*position)
        .is_some_and(|byte| space(*byte) || *byte == b'/')
    {
        *position += 1;
    }
    if *bytes.get(*position)? == b'>' {
        return None;
    }
    let mut name = Vec::new();
    while let Some(&byte) = bytes.get(*position) {
        if space(byte) || matches!(byte, b'=' | b'/' | b'>') {
            break;
        }
        name.push(byte.to_ascii_lowercase());
        *position += 1;
    }
    if name.is_empty() {
        return None;
    }
    while bytes.get(*position).is_some_and(|byte| space(*byte)) {
        *position += 1;
    }
    if bytes.get(*position) != Some(&b'=') {
        return Some((name, Vec::new()));
    }
    *position += 1;
    while bytes.get(*position).is_some_and(|byte| space(*byte)) {
        *position += 1;
    }
    let first = *bytes.get(*position)?;
    let mut value = Vec::new();
    if matches!(first, b'\'' | b'"') {
        *position += 1;
        loop {
            let byte = *bytes.get(*position)?;
            *position += 1;
            if byte == first {
                return Some((name, value));
            }
            value.push(byte.to_ascii_lowercase());
        }
    }
    while let Some(&byte) = bytes.get(*position) {
        if space(byte) || byte == b'>' {
            return Some((name, value));
        }
        value.push(byte.to_ascii_lowercase());
        *position += 1;
    }
    None
}
fn content_encoding(value: &[u8]) -> Option<&'static Encoding> {
    let mut position = 0;
    while position + 7 <= value.len() {
        if !prefix(&value[position..], b"charset") {
            position += 1;
            continue;
        }
        position += 7;
        while value.get(position).is_some_and(|byte| space(*byte)) {
            position += 1;
        }
        if value.get(position) != Some(&b'=') {
            continue;
        }
        position += 1;
        while value.get(position).is_some_and(|byte| space(*byte)) {
            position += 1;
        }
        let first = *value.get(position)?;
        let label = if matches!(first, b'\'' | b'"') {
            position += 1;
            let end = value[position..].iter().position(|byte| *byte == first)? + position;
            &value[position..end]
        } else {
            let end = value[position..]
                .iter()
                .position(|byte| space(*byte) || *byte == b';')
                .map_or(value.len(), |offset| position + offset);
            &value[position..end]
        };
        return meta_encoding(label);
    }
    None
}

pub(super) fn encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position..].starts_with(b"<!--") {
            let end = bytes[position + 4..]
                .windows(3)
                .position(|part| part == b"-->")?;
            position += 4 + end + 3;
            continue;
        }
        if prefix(&bytes[position..], b"<meta")
            && bytes
                .get(position + 5)
                .is_some_and(|byte| space(*byte) || matches!(*byte, b'/' | b'>'))
        {
            position += 5;
            let mut attributes = Vec::<(Vec<u8>, Vec<u8>)>::new();
            while let Some((name, value)) = attribute(bytes, &mut position) {
                if !attributes.iter().any(|(old, _)| *old == name) {
                    attributes.push((name, value));
                }
            }
            // A declaration cut off at the prescan boundary is not complete.
            if bytes.get(position) != Some(&b'>') {
                return None;
            }
            let pragma = attributes
                .iter()
                .any(|(name, value)| name == b"http-equiv" && value == b"content-type");
            let mut candidate = None;
            let mut need_pragma = false;
            for (name, value) in &attributes {
                if name == b"charset" {
                    candidate = meta_encoding(value);
                    need_pragma = false;
                } else if name == b"content" && candidate.is_none() {
                    candidate = content_encoding(value);
                    need_pragma = true;
                }
            }
            if candidate.is_some() && (!need_pragma || pragma) {
                return candidate;
            }
        } else if bytes[position] == b'<'
            && bytes.get(position + 1).is_some_and(u8::is_ascii_alphabetic)
        {
            position += 2;
            while bytes
                .get(position)
                .is_some_and(|byte| !space(*byte) && !matches!(*byte, b'>' | b'/'))
            {
                position += 1;
            }
            while attribute(bytes, &mut position).is_some() {}
        } else if bytes[position] == b'<'
            && bytes
                .get(position + 1)
                .is_some_and(|byte| matches!(*byte, b'!' | b'/' | b'?'))
        {
            position += 2;
            while bytes.get(position).is_some_and(|byte| *byte != b'>') {
                position += 1;
            }
        }
        position += 1;
    }
    None
}
