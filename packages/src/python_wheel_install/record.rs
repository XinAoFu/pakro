//! Installed RECORD from final file facts and the owner's reserved metadata.

use std::path::Path;

use base64::Engine;
use eyre::{ContextCompat, Result, ensure};
use sha2::{Digest as _, Sha256};

use super::super::python_wheel_plan::{WheelGeneratedTarget, WheelGraphPlan, WheelOwner};
use super::{INSTALLER, WheelInstallLimits};

fn relative(path: &Path, base: &Path) -> Result<String> {
    let left: Vec<_> = base.components().collect();
    let right: Vec<_> = path.components().collect();
    let common = left.iter().zip(&right).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_owned(); left.len() - common];
    for component in &right[common..] {
        parts.push(
            component
                .as_os_str()
                .to_str()
                .context("wheel record path is not UTF-8")?
                .to_owned(),
        );
    }
    Ok(parts.join("/"))
}

fn append(output: &mut Vec<u8>, value: &[u8], maximum: usize) -> Result<()> {
    ensure!(
        value.len() <= maximum.saturating_sub(output.len()),
        "installed record byte limit exceeded"
    );
    output.extend_from_slice(value);
    Ok(())
}
fn field(output: &mut Vec<u8>, value: &str, maximum: usize) -> Result<()> {
    let quoted = value.contains([',', '"', '\r', '\n']);
    if quoted {
        append(output, b"\"", maximum)?;
    }
    for byte in value.as_bytes() {
        if quoted && *byte == b'"' {
            append(output, b"\"", maximum)?;
        }
        append(output, std::slice::from_ref(byte), maximum)?;
    }
    if quoted {
        append(output, b"\"", maximum)?;
    }
    Ok(())
}

fn push_row(
    rows: &mut Vec<(String, String, String)>,
    encoded_bytes: &mut usize,
    row: (String, String, String),
    maximum: usize,
) -> Result<()> {
    // Charge the exact eventual CSV size before retaining each row. Only this
    // one path and two small ASCII fields can exist beyond the saved budget;
    // long source paths cannot accumulate into an oversized intermediate Vec.
    let mut size = 3_usize; // two commas and the final LF
    for field in [&row.0, &row.1, &row.2] {
        size = size
            .checked_add(field.len())
            .context("installed record byte limit overflow")?;
        if field.contains([',', '"', '\r', '\n']) {
            size = size
                .checked_add(2)
                .and_then(|value| {
                    value.checked_add(field.bytes().filter(|byte| *byte == b'"').count())
                })
                .context("installed record byte limit overflow")?;
        }
    }
    let used = encoded_bytes
        .checked_add(size)
        .context("installed record byte limit overflow")?;
    ensure!(used <= maximum, "installed record byte limit exceeded");
    *encoded_bytes = used;
    rows.push(row);
    Ok(())
}

pub(crate) fn from_verified_files(
    record: &Path,
    files: &[crate::packages::python_environment::prefix::verify::PrefixExpectedFile],
    limits: WheelInstallLimits,
) -> Result<Vec<u8>> {
    let base = record
        .parent()
        .and_then(Path::parent)
        .context("derived RECORD metadata base is missing")?;
    ensure!(
        files.len() < limits.max_record_rows,
        "derived RECORD row budget exceeded"
    );
    let mut rows = Vec::new();
    let mut encoded_bytes = 0;
    for file in files {
        ensure!(
            file.relative != record,
            "derived RECORD must not hash itself"
        );
        push_row(
            &mut rows,
            &mut encoded_bytes,
            (
                relative(&file.relative, base)?,
                format!(
                    "sha256={}",
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(file.sha256)
                ),
                file.size.to_string(),
            ),
            limits.max_record_bytes,
        )?;
    }
    push_row(
        &mut rows,
        &mut encoded_bytes,
        (relative(record, base)?, String::new(), String::new()),
        limits.max_record_bytes,
    )?;
    rows.sort();
    ensure!(
        rows.windows(2).all(|pair| pair[0].0 != pair[1].0),
        "derived RECORD repeats a file path"
    );
    let mut output = Vec::new();
    for (path, hash, size) in rows {
        field(&mut output, &path, limits.max_record_bytes)?;
        append(&mut output, b",", limits.max_record_bytes)?;
        append(&mut output, hash.as_bytes(), limits.max_record_bytes)?;
        append(&mut output, b",", limits.max_record_bytes)?;
        append(&mut output, size.as_bytes(), limits.max_record_bytes)?;
        append(&mut output, b"\n", limits.max_record_bytes)?;
    }
    ensure!(
        output.len() == encoded_bytes,
        "derived RECORD CSV accounting differs"
    );
    Ok(output)
}

pub(super) fn generate(
    plan: &WheelGraphPlan,
    owner: &WheelOwner,
    limits: WheelInstallLimits,
) -> Result<Vec<u8>> {
    let own_record = plan
        .pending_metadata()
        .iter()
        .find(|slot| {
            slot.owner() == owner && slot.target() == &WheelGeneratedTarget::InstalledRecord
        })
        .context("installed record reservation is missing")?;
    let base = own_record
        .destination()
        .parent()
        .and_then(Path::parent)
        .context("installed record metadata parent is missing")?;
    let count = plan
        .files()
        .iter()
        .filter(|file| file.owners().contains(owner))
        .count()
        .checked_add(
            plan.pending_metadata()
                .iter()
                .filter(|slot| slot.owner() == owner)
                .count(),
        )
        .context("installed record row limit overflow")?;
    ensure!(
        count <= limits.max_record_rows,
        "installed record row limit exceeded"
    );
    let mut rows = Vec::new();
    let mut encoded_bytes = 0;
    for file in plan
        .files()
        .iter()
        .filter(|file| file.owners().contains(owner))
    {
        push_row(
            &mut rows,
            &mut encoded_bytes,
            (
                relative(file.destination(), base)?,
                format!(
                    "sha256={}",
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(file.sha256())
                ),
                file.size().to_string(),
            ),
            limits.max_record_bytes,
        )?;
    }
    for slot in plan
        .pending_metadata()
        .iter()
        .filter(|slot| slot.owner() == owner)
    {
        ensure!(
            slot.destination().starts_with(plan.final_prefix()),
            "installed record destination escapes final prefix"
        );
        let (hash, size) = match slot.target() {
            WheelGeneratedTarget::InstalledRecord => (String::new(), String::new()),
            WheelGeneratedTarget::Installer => (
                format!(
                    "sha256={}",
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(Sha256::digest(INSTALLER))
                ),
                INSTALLER.len().to_string(),
            ),
            WheelGeneratedTarget::Scripts(_) => {
                eyre::bail!("installed record has an invalid metadata reservation")
            }
        };
        push_row(
            &mut rows,
            &mut encoded_bytes,
            (relative(slot.destination(), base)?, hash, size),
            limits.max_record_bytes,
        )?;
    }
    ensure!(
        rows.len() <= limits.max_record_rows,
        "installed record row limit exceeded"
    );
    rows.sort();
    ensure!(
        rows.windows(2).all(|pair| pair[0].0 != pair[1].0),
        "installed record has duplicate final file paths"
    );
    let mut output = Vec::new();
    for (path, hash, size) in rows {
        field(&mut output, &path, limits.max_record_bytes)?;
        append(&mut output, b",", limits.max_record_bytes)?;
        append(&mut output, hash.as_bytes(), limits.max_record_bytes)?;
        append(&mut output, b",", limits.max_record_bytes)?;
        append(&mut output, size.as_bytes(), limits.max_record_bytes)?;
        append(&mut output, b"\n", limits.max_record_bytes)?;
    }
    ensure!(
        output.len() == encoded_bytes,
        "installed record CSV accounting disagrees with final bytes"
    );
    Ok(output)
}
