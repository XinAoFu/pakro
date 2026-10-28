//! Bounded streaming script rewriting with independent raw and final statistics.

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};

use eyre::{Result, ensure, eyre};
use sha2::{Digest as _, Sha256};

use super::{
    PythonScriptTarget, WheelScriptLimits, WheelScriptOperation, WheelScriptTransform, launcher,
};

#[derive(Debug)]
struct InputLimit;
impl fmt::Display for InputLimit {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("script input byte limit exceeded")
    }
}
impl std::error::Error for InputLimit {}
fn read_error(error: io::Error) -> eyre::Report {
    if error
        .get_ref()
        .is_some_and(|source| source.downcast_ref::<InputLimit>().is_some())
    {
        eyre!("script input byte limit exceeded")
    } else {
        eyre!("script read failed")
    }
}

struct Input<R> {
    reader: R,
    size: u64,
    digest: Sha256,
    limit: u64,
}
impl<R: Read> Read for Input<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let remaining = self.limit - self.size;
        // One byte beyond the remaining allowance is enough to distinguish an
        // exact-limit EOF from oversized input. Never pull an unbounded chunk.
        let allowance = remaining.saturating_add(1);
        let maximum = bytes
            .len()
            .min(usize::try_from(allowance).unwrap_or(usize::MAX));
        let count = self.reader.read(&mut bytes[..maximum])?;
        if count as u64 > remaining {
            return Err(io::Error::other(InputLimit));
        }
        self.size += count as u64;
        self.digest.update(&bytes[..count]);
        Ok(count)
    }
}

struct Output<'a, W> {
    writer: &'a mut W,
    size: u64,
    digest: Sha256,
    limit: u64,
}
impl<W: Write> Output<'_, W> {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            bytes.len() as u64 <= self.limit.saturating_sub(self.size),
            "script output byte limit exceeded"
        );
        self.writer
            .write_all(bytes)
            .map_err(|_| eyre!("script write failed"))?;
        self.size += bytes.len() as u64;
        self.digest.update(bytes);
        Ok(())
    }
}

struct PhysicalLine {
    content: Vec<u8>,
    ending: &'static [u8],
}
fn physical_line<R: BufRead>(
    input: &mut R,
    mut content: Vec<u8>,
    limit: usize,
) -> Result<PhysicalLine> {
    ensure!(
        content.len() <= limit,
        "script first line byte limit exceeded"
    );
    loop {
        let available = input.fill_buf().map_err(read_error)?;
        if available.is_empty() {
            return Ok(PhysicalLine {
                content,
                ending: b"",
            });
        }
        let end = available
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'));
        let count = end.unwrap_or(available.len());
        ensure!(
            count <= limit.saturating_sub(content.len()),
            "script first line capture byte limit exceeded"
        );
        content.extend_from_slice(&available[..count]);
        let terminator = end.map(|at| available[at]);
        input.consume(count + usize::from(terminator.is_some()));
        match terminator {
            None => {}
            Some(b'\n') => {
                return Ok(PhysicalLine {
                    content,
                    ending: b"\n",
                });
            }
            Some(b'\r') => {
                let follows_lf = input.fill_buf().map_err(read_error)?.first() == Some(&b'\n');
                if follows_lf {
                    input.consume(1);
                }
                return Ok(PhysicalLine {
                    content,
                    ending: if follows_lf { b"\r\n" } else { b"\r" },
                });
            }
            _ => unreachable!("line terminator is CR or LF"),
        }
    }
}

pub(super) fn rewrite<R: Read, W: Write>(
    input: R,
    writer: &mut W,
    target: &PythonScriptTarget<'_>,
    source_mode: u32,
    limits: WheelScriptLimits,
) -> Result<WheelScriptTransform> {
    super::validate_limits(limits)?;
    let executable = launcher::executable(target)?;
    let mut input = BufReader::with_capacity(
        64 * 1024,
        Input {
            reader: input,
            size: 0,
            digest: Sha256::new(),
            limit: limits.max_input_bytes,
        },
    );
    let mut output = Output {
        writer,
        size: 0,
        digest: Sha256::new(),
        limit: limits.max_output_bytes,
    };
    let mut prefix = [0; 8];
    let mut used = 0;
    while used < prefix.len() {
        let count = input.read(&mut prefix[used..]).map_err(read_error)?;
        if count == 0 {
            break;
        }
        used += count;
    }
    let rewritten = used == prefix.len() && &prefix == b"#!python";
    if rewritten {
        let first = physical_line(&mut input, prefix.to_vec(), limits.max_first_line_bytes)?;
        let second = if first.ending.is_empty() {
            PhysicalLine {
                content: Vec::new(),
                ending: b"",
            }
        } else {
            physical_line(&mut input, Vec::new(), limits.max_first_line_bytes)?
        };
        let first_cookie = launcher::coding_cookie(&first.content);
        let cookie = first_cookie.or_else(|| launcher::coding_cookie(&second.content));
        // A direct shebang preserves an original second-line cookie naturally.
        // A first-line cookie or any trampoline must redeclare it in line two.
        let copied_cookie = if launcher::complex(executable) || first_cookie.is_some() {
            cookie
        } else {
            None
        };
        output.append(&launcher::prefix(executable, copied_cookie, limits)?)?;
        output.append(&second.content)?;
        output.append(second.ending)?;
    } else {
        output.append(&prefix[..used])?;
    }
    let mut bytes = [0; 64 * 1024];
    loop {
        let count = input.read(&mut bytes).map_err(read_error)?;
        if count == 0 {
            break;
        }
        output.append(&bytes[..count])?;
    }
    let raw = input.into_inner();
    let mode = (source_mode & 0o777) | if rewritten { 0o111 } else { 0 };
    Ok(WheelScriptTransform {
        operation: if rewritten {
            WheelScriptOperation::RewritePythonShebang
        } else {
            WheelScriptOperation::Copy
        },
        raw_sha256: raw.digest.finalize().into(),
        raw_size: raw.size,
        sha256: output.digest.finalize().into(),
        size: output.size,
        mode,
    })
}

pub(super) fn rebase<R: Read, W: Write>(
    input: R,
    writer: &mut W,
    old_target: &PythonScriptTarget<'_>,
    new_target: &PythonScriptTarget<'_>,
    source_mode: u32,
    limits: WheelScriptLimits,
) -> Result<WheelScriptTransform> {
    super::validate_limits(limits)?;
    let old = launcher::executable(old_target)?;
    let new = launcher::executable(new_target)?;
    let without_cookie = launcher::prefix(old, None, limits)?;
    let maximum = limits.max_wrapper_bytes.min(
        without_cookie
            .len()
            .checked_add(limits.max_first_line_bytes)
            .ok_or_else(|| eyre!("installed script probe size overflow"))?,
    );
    let mut input = BufReader::with_capacity(
        64 * 1024,
        Input {
            reader: input,
            size: 0,
            digest: Sha256::new(),
            limit: limits.max_input_bytes,
        },
    );
    let mut output = Output {
        writer,
        size: 0,
        digest: Sha256::new(),
        limit: limits.max_output_bytes,
    };
    let mut probe = Vec::new();
    let mut buffer = [0; 64 * 1024];
    while probe.len() < maximum {
        let count = input
            .read(&mut buffer[..(maximum - probe.len()).min(64 * 1024)])
            .map_err(read_error)?;
        if count == 0 {
            break;
        }
        probe.extend_from_slice(&buffer[..count]);
    }
    let first_end = probe
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1);
    let second_line = first_end.map(|start| {
        let rest = &probe[start..];
        &rest[..rest
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(rest.len())]
    });
    let cookie = second_line.and_then(launcher::coding_cookie);
    let canonical_cookie = cookie.filter(|cookie| {
        second_line == Some(format!("# coding: {cookie}").as_bytes())
            && first_end.is_some_and(|start| probe[start..].contains(&b'\n'))
    });
    let with_cookie = canonical_cookie
        .map(|cookie| launcher::prefix(old, Some(cookie), limits))
        .transpose()?;
    let consumed = if let Some(prefix) = &with_cookie {
        if probe.starts_with(prefix) {
            Some((prefix.len(), canonical_cookie))
        } else if probe.starts_with(&without_cookie) {
            Some((without_cookie.len(), None))
        } else {
            None
        }
    } else if probe.starts_with(&without_cookie) {
        Some((without_cookie.len(), None))
    } else {
        None
    };
    if let Some((consumed, canonical)) = consumed {
        // A direct launch can retain an original noncanonical second-line
        // cookie. A new trampoline must also put that encoding on line two.
        let encoding = canonical.or_else(|| if launcher::complex(new) { cookie } else { None });
        output.append(&launcher::prefix(new, encoding, limits)?)?;
        output.append(&probe[consumed..])?;
    } else {
        output.append(&probe)?;
    }
    loop {
        let count = input.read(&mut buffer).map_err(read_error)?;
        if count == 0 {
            break;
        }
        output.append(&buffer[..count])?;
    }
    let raw = input.into_inner();
    Ok(WheelScriptTransform {
        operation: if consumed.is_some() {
            WheelScriptOperation::RewritePythonShebang
        } else {
            WheelScriptOperation::Copy
        },
        raw_sha256: raw.digest.finalize().into(),
        raw_size: raw.size,
        sha256: output.digest.finalize().into(),
        size: output.size,
        mode: source_mode & 0o777,
    })
}

#[cfg(test)]
mod rebase_tests {
    use super::*;
    use std::io::Cursor;
    use std::path::Path;

    fn target(prefix: &'static str, executable: &'static str) -> PythonScriptTarget<'static> {
        PythonScriptTarget {
            final_prefix: Path::new(prefix),
            interpreter: Path::new(executable),
        }
    }

    fn stats(input: &[u8], output: &[u8], receipt: &WheelScriptTransform) {
        assert_eq!(receipt.raw_size(), input.len() as u64);
        assert_eq!(
            receipt.raw_sha256().as_slice(),
            Sha256::digest(input).as_slice()
        );
        assert_eq!(receipt.size(), output.len() as u64);
        assert_eq!(
            receipt.sha256().as_slice(),
            Sha256::digest(output).as_slice()
        );
    }

    #[test]
    fn p3_rebase_direct_prefix_preserves_canonical_cookie_body_mode_and_independent_hashes() {
        let old = target("/opt/old", "/opt/old/bin/python");
        let new = target("/opt/new", "/opt/new/bin/python");
        let input = b"#!/opt/old/bin/python\n# coding: latin-1\nprint('\xe9')\n";
        let expected = b"#!/opt/new/bin/python\n# coding: latin-1\nprint('\xe9')\n";
        let mut output = Vec::new();
        let receipt = rebase(
            Cursor::new(input),
            &mut output,
            &old,
            &new,
            0o750,
            WheelScriptLimits::default(),
        )
        .unwrap();
        assert_eq!(output, expected);
        assert_eq!(
            receipt.operation(),
            WheelScriptOperation::RewritePythonShebang
        );
        assert_eq!(receipt.mode(), 0o750);
        stats(input, &output, &receipt);
    }

    #[test]
    fn p3_rebase_exact_trampoline_rewrites_only_its_generated_prefix() {
        let old = target("/opt/old env", "/opt/old env/bin/.pakro python");
        let new = target("/opt/new env", "/opt/new env/bin/.pakro python");
        let limits = WheelScriptLimits::default();
        let body = b"print('/opt/old env stays literal in the original body')\n";
        let mut input =
            launcher::prefix(launcher::executable(&old).unwrap(), Some("utf-8"), limits).unwrap();
        input.extend_from_slice(body);
        let mut expected =
            launcher::prefix(launcher::executable(&new).unwrap(), Some("utf-8"), limits).unwrap();
        expected.extend_from_slice(body);
        let mut output = Vec::new();
        let receipt = rebase(Cursor::new(&input), &mut output, &old, &new, 0o755, limits).unwrap();
        assert_eq!(output, expected);
        assert_eq!(
            receipt.operation(),
            WheelScriptOperation::RewritePythonShebang
        );
        stats(&input, &output, &receipt);
    }

    #[test]
    fn p3_rebase_other_or_partial_prefixes_remain_exact_streaming_copies() {
        let old = target("/opt/old", "/opt/old/bin/python");
        let new = target("/opt/new", "/opt/new/bin/python");
        for input in [
            b"#!/bin/sh\nprintf '/opt/old is an unknown body reference'\n".to_vec(),
            b"#!/opt/other/bin/python\nprint(1)\n".to_vec(),
            b"#!/opt/old/bin/python".to_vec(),
            vec![b'x'; 192 * 1024],
        ] {
            let mut output = Vec::new();
            let receipt = rebase(
                Cursor::new(&input),
                &mut output,
                &old,
                &new,
                0o640,
                WheelScriptLimits::default(),
            )
            .unwrap();
            assert_eq!(output, input);
            assert_eq!(receipt.operation(), WheelScriptOperation::Copy);
            assert_eq!(receipt.mode(), 0o640);
            stats(&input, &output, &receipt);
        }
    }

    #[test]
    fn p3_rebase_validates_new_target_and_both_stream_budgets() {
        let old = target("/opt/old", "/opt/old/bin/python");
        let outside = target("/opt/new", "/opt/elsewhere/python");
        assert!(
            rebase(
                Cursor::new(b"ordinary"),
                &mut Vec::new(),
                &old,
                &outside,
                0o644,
                WheelScriptLimits::default()
            )
            .is_err()
        );
        let new = target("/opt/new longer", "/opt/new longer/bin/.pakro python");
        let input = b"#!/opt/old/bin/python\nprint(1)\n";
        let mut limits = WheelScriptLimits::default();
        limits.max_input_bytes = input.len() as u64 - 1;
        assert!(
            rebase(
                Cursor::new(input),
                &mut Vec::new(),
                &old,
                &new,
                0o755,
                limits
            )
            .is_err()
        );
        limits = WheelScriptLimits::default();
        limits.max_output_bytes = input.len() as u64;
        assert!(
            rebase(
                Cursor::new(input),
                &mut Vec::new(),
                &old,
                &new,
                0o755,
                limits
            )
            .is_err()
        );
    }
}
