//! One bounded outer declaration boundary and pre-collection budget pass.

use super::*;
use crate::python_markers::{
    PythonMarkerLimitKind, preflight_python_marker, validate_python_marker_limits,
};

fn exceeded(kind: PythonRequirementLimitKind) -> PythonRequirementError {
    PythonRequirementError::LimitExceeded(kind)
}
fn syntax(offset: usize) -> PythonRequirementError {
    PythonRequirementError::Syntax { offset }
}
fn space(bytes: &[u8], position: &mut usize) {
    while bytes
        .get(*position)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        *position += 1;
    }
}
fn name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn locate(
    raw: &str,
    limits: PythonRequirementLimits,
) -> PythonRequirementResult<PythonRequirementSpans> {
    let bytes = raw.as_bytes();
    let mut position = 0usize;
    space(bytes, &mut position);
    let name_start = position;
    while bytes.get(position).is_some_and(|byte| name_byte(*byte)) {
        position += 1;
    }
    if position == name_start {
        return Err(syntax(position));
    }
    space(bytes, &mut position);
    if bytes.get(position) == Some(&b'[') {
        position += 1;
        space(bytes, &mut position);
        let mut extras = usize::from(bytes.get(position) != Some(&b']'));
        if extras > limits.max_requested_extras {
            return Err(exceeded(PythonRequirementLimitKind::RequestedExtras));
        }
        while let Some(byte) = bytes.get(position) {
            if *byte == b']' {
                break;
            }
            if *byte == b',' {
                extras = extras
                    .checked_add(1)
                    .ok_or_else(|| exceeded(PythonRequirementLimitKind::RequestedExtras))?;
                if extras > limits.max_requested_extras {
                    return Err(exceeded(PythonRequirementLimitKind::RequestedExtras));
                }
            }
            position += 1;
        }
        if bytes.get(position) != Some(&b']') {
            return Err(syntax(position));
        }
        position += 1;
        space(bytes, &mut position);
    }
    let version_start = position;
    let (uri, delimiter) = if bytes.get(position) == Some(&b'@') {
        position += 1;
        space(bytes, &mut position);
        let start = position;
        while bytes
            .get(position)
            .is_some_and(|byte| !matches!(byte, b' ' | b'\t'))
        {
            position += 1;
        }
        if start == position {
            return Err(syntax(start));
        }
        let uri = Some(start..position);
        space(bytes, &mut position);
        let delimiter = if position == bytes.len() {
            None
        } else if bytes[position] == b';' {
            Some(position)
        } else {
            return Err(syntax(position));
        };
        (uri, delimiter)
    } else {
        (None, bytes.iter().position(|byte| *byte == b';'))
    };
    let marker = delimiter.map(|position| position + 1..raw.len());
    if let Some(marker) = &marker
        && raw[marker.clone()].trim_matches([' ', '\t']).is_empty()
    {
        return Err(syntax(raw.len()));
    }
    let version_end = raw[..delimiter.unwrap_or(raw.len())]
        .trim_end_matches([' ', '\t'])
        .len();
    let versions =
        (uri.is_none() && version_start < version_end).then_some(version_start..version_end);
    Ok(PythonRequirementSpans {
        prefix: 0..delimiter.unwrap_or(raw.len()),
        uri,
        marker,
        versions,
    })
}

fn prefix_tokens(
    raw: &str,
    spans: &PythonRequirementSpans,
    limit: usize,
) -> PythonRequirementResult<usize> {
    let bytes = raw.as_bytes();
    let mut position = spans.prefix.start;
    let mut count = 0usize;
    while position < spans.prefix.end {
        if matches!(bytes[position], b' ' | b'\t') {
            position += 1;
            continue;
        }
        count = count
            .checked_add(1)
            .filter(|count| *count <= limit)
            .ok_or_else(|| exceeded(PythonRequirementLimitKind::SyntaxTokens))?;
        if let Some(uri) = &spans.uri
            && position == uri.start
        {
            position = uri.end;
            continue;
        }
        let character = raw[position..]
            .chars()
            .next()
            .ok_or_else(|| syntax(position))?;
        if character.is_whitespace() {
            return Err(syntax(position));
        }
        if name_byte(bytes[position]) {
            while position < spans.prefix.end && name_byte(bytes[position]) {
                position += 1;
            }
        } else if matches!(bytes[position], b'<' | b'>' | b'=' | b'!' | b'~') {
            while position < spans.prefix.end
                && matches!(bytes[position], b'<' | b'>' | b'=' | b'!' | b'~')
            {
                position += 1;
            }
        } else {
            position += character.len_utf8();
        }
    }
    Ok(count)
}

pub(super) fn preflight(
    raw: &str,
    limits: PythonRequirementLimits,
) -> PythonRequirementResult<PythonRequirementSpans> {
    if [
        limits.max_input_bytes,
        limits.max_requested_extras,
        limits.max_syntax_tokens,
        limits.max_marker_comparisons,
    ]
    .contains(&0)
    {
        return Err(PythonRequirementError::InvalidLimits);
    }
    validate_python_marker_limits(limits.marker)
        .map_err(|_| PythonRequirementError::InvalidLimits)?;
    if raw.len() > limits.max_input_bytes {
        return Err(exceeded(PythonRequirementLimitKind::InputBytes));
    }
    if let Some((offset, _)) = raw
        .char_indices()
        .find(|(_, value)| value.is_control() && *value != '\t')
    {
        return Err(syntax(offset));
    }
    let spans = locate(raw, limits)?;
    let prefix = prefix_tokens(raw, &spans, limits.max_syntax_tokens)?;
    if let Some(marker) = &spans.marker {
        let used = prefix
            .checked_add(1)
            .filter(|used| *used < limits.max_syntax_tokens)
            .ok_or_else(|| exceeded(PythonRequirementLimitKind::SyntaxTokens))?;
        let remaining = limits.max_syntax_tokens - used;
        let mut marker_limits = limits.marker;
        marker_limits.max_tokens = marker_limits.max_tokens.min(remaining);
        preflight_python_marker(
            &raw[marker.clone()],
            marker_limits,
            limits.max_marker_comparisons,
        )
        .map_err(|error| match error {
            PythonMarkerError::LimitExceeded(PythonMarkerLimitKind::Comparisons) => {
                exceeded(PythonRequirementLimitKind::MarkerComparisons)
            }
            PythonMarkerError::LimitExceeded(PythonMarkerLimitKind::Tokens)
                if remaining <= limits.marker.max_tokens =>
            {
                exceeded(PythonRequirementLimitKind::SyntaxTokens)
            }
            other => PythonRequirementError::Marker(other),
        })?;
    }
    Ok(spans)
}
