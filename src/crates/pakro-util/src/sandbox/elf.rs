//! The kernel also checks Execute on the ELF interpreter during execve.
use eyre::{Context, Result, ensure};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::ffi::OsStringExt,
    path::PathBuf,
};

pub fn runtime_loaders(programs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut loaders = Vec::new();
    for path in programs {
        let mut input =
            File::open(path).with_context(|| format!("opening executable {}", path.display()))?;
        let mut header = [0u8; 64];
        let n = input.read(&mut header)?;
        if n < 4 || &header[..4] != b"\x7fELF" {
            continue;
        }
        ensure!(
            n == 64 && header[4] == 2 && header[5] == 1,
            "worker requires a native ELF64 little-endian executable"
        );
        let offset = u64::from_le_bytes(header[32..40].try_into()?);
        let size = u16::from_le_bytes(header[54..56].try_into()?) as u64;
        let count = u16::from_le_bytes(header[56..58].try_into()?) as u64;
        ensure!(
            size >= 56
                && size <= 256
                && count <= 1024
                && offset
                    .checked_add(size * count)
                    .is_some_and(|end| end <= input.metadata().map(|m| m.len()).unwrap_or(0)),
            "invalid executable program headers"
        );
        for index in 0..count {
            input.seek(SeekFrom::Start(offset + index * size))?;
            let mut program = [0u8; 56];
            input.read_exact(&mut program)?;
            if u32::from_le_bytes(program[..4].try_into()?) != 3 {
                continue;
            }
            let at = u64::from_le_bytes(program[8..16].try_into()?);
            let length = u64::from_le_bytes(program[32..40].try_into()?);
            ensure!(
                (2..=4096).contains(&length),
                "invalid executable interpreter length"
            );
            input.seek(SeekFrom::Start(at))?;
            let mut bytes = vec![0; length as usize];
            input.read_exact(&mut bytes)?;
            ensure!(
                bytes.pop() == Some(0) && !bytes.contains(&0),
                "invalid executable interpreter"
            );
            let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
            ensure!(path.is_absolute(), "ELF interpreter must be absolute");
            let path = path
                .canonicalize()
                .context("ELF runtime loader is unavailable")?;
            if !loaders.contains(&path) {
                loaders.push(path);
            }
        }
    }
    Ok(loaders)
}
