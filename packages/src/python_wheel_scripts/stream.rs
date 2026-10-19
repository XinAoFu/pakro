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
