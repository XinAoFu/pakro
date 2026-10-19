//! Explicit media/charset selection with independently bounded UTF-8 output.

use super::*;
use encoding_rs::{
    DecoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED,
};

mod prescan;

pub(super) fn validate_media_type(value: &str) -> SimpleClientResult<mime::Mime> {
    let media = value
        .parse::<mime::Mime>()
        .map_err(|_| SimpleClientError::UnsupportedMediaType)?;
    if !matches!(
        media.essence_str(),
        "application/vnd.pypi.simple.v1+json" | "application/vnd.pypi.simple.v1+html" | "text/html"
    ) {
        return Err(SimpleClientError::UnsupportedMediaType);
    }
    Ok(media)
}

pub(super) fn decode_response(
    body: &[u8],
    content_type: &str,
    context: super::super::SimpleParseContext<'_>,
    limits: SimpleClientLimits,
    document: &http::EffectiveSimpleDocument,
) -> SimpleClientResult<SimpleProject> {
    let media = validate_media_type(content_type)?;
    if media.essence_str() == "application/vnd.pypi.simple.v1+json" {
        // RFC8259: charset parameters do not alter network JSON's UTF-8.
        let body = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
        std::str::from_utf8(body).map_err(|_| SimpleClientError::InvalidEncoding)?;
        if body.len() as u64 > limits.max_decoded_bytes {
            return Err(SimpleClientError::LimitExceeded(
                SimpleClientLimitKind::DecodedBytes,
            ));
        }
        return super::super::json::parse_for_document(body, context, document)
            .map_err(SimpleClientError::Parse);
    }
    let (encoding, skip) = html_encoding(body, &media)?;
    let text = decode_utf8(&body[skip..], encoding, limits.max_decoded_bytes)?;
    super::super::html::parse_for_document(&text, context, limits.html, document)
        .map_err(SimpleClientError::Parse)
}

fn html_encoding(
    body: &[u8],
    media: &mime::Mime,
) -> SimpleClientResult<(&'static Encoding, usize)> {
    // BOM is authoritative even when an HTTP charset claims something else.
    if let Some(value) = Encoding::for_bom(body) {
        return Ok(value);
    }
    let mut charset = None;
    for (name, value) in media.params() {
        if name == mime::CHARSET {
            if charset.is_some() {
                return Err(SimpleClientError::UnsupportedEncoding);
            }
            charset = Some(value.as_str());
        }
    }
    if let Some(label) = charset {
        let encoding = Encoding::for_label_no_replacement(label.as_bytes())
            .ok_or(SimpleClientError::UnsupportedEncoding)?;
        return Ok((encoding, 0));
    }
    if body.starts_with(b"<\0?\0x\0") {
        return Ok((UTF_16LE, 0));
    }
    if body.starts_with(b"\0<\0?\0x") {
        return Ok((UTF_16BE, 0));
    }
    let encoding = prescan::encoding(&body[..body.len().min(1024)]).unwrap_or(UTF_8);
    Ok((encoding, 0))
}

fn decode_utf8(
    body: &[u8],
    encoding: &'static Encoding,
    maximum: u64,
) -> SimpleClientResult<String> {
    let mut decoder = encoding.new_decoder_without_bom_handling();
    let mut remaining = body;
    let mut output = String::new();
    let mut buffer = [0u8; 8192];
    loop {
        let room = maximum.saturating_sub(output.len() as u64);
        // A scalar-sized fixed lookahead distinguishes exact EOF from more
        // output, without appending bytes beyond the caller's decoded cap.
        let request = room.saturating_add(4).min(buffer.len() as u64) as usize;
        let (result, read, written) =
            decoder.decode_to_utf8_without_replacement(remaining, &mut buffer[..request], true);
        if written as u64 > room {
            return Err(SimpleClientError::LimitExceeded(
                SimpleClientLimitKind::DecodedBytes,
            ));
        }
        let text = std::str::from_utf8(&buffer[..written])
            .map_err(|_| SimpleClientError::InvalidEncoding)?;
        output
            .try_reserve(text.len())
            .map_err(|_| SimpleClientError::LimitExceeded(SimpleClientLimitKind::DecodedBytes))?;
        output.push_str(text);
        remaining = &remaining[read..];
        match result {
            DecoderResult::InputEmpty => return Ok(output),
            DecoderResult::Malformed(_, _) => return Err(SimpleClientError::InvalidEncoding),
            DecoderResult::OutputFull if read == 0 && written == 0 => {
                return Err(SimpleClientError::LimitExceeded(
                    SimpleClientLimitKind::DecodedBytes,
                ));
            }
            DecoderResult::OutputFull => {}
        }
    }
}

fn meta_encoding(label: &[u8]) -> Option<&'static Encoding> {
    let encoding = Encoding::for_label_no_replacement(label)?;
    Some(if encoding == UTF_16LE || encoding == UTF_16BE {
        UTF_8
    } else if encoding == X_USER_DEFINED {
        WINDOWS_1252
    } else {
        encoding
    })
}
