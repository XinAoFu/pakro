//! Case-sensitive ConfigParser-compatible entry-point metadata, without execution.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use eyre::{ContextCompat, Result, ensure, eyre};
use regex::Regex;

use super::{WheelEntryPoint, WheelEntryPointKind, WheelScriptLimits};

static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:_|\p{XID_Start})\p{XID_Continue}*$")
        .expect("constant Python identifier pattern")
});
static GROUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\p{L}\p{N}_]+(?:\.[\p{L}\p{N}_]+)*$")
        .expect("constant entry-point group pattern")
});

type Options = BTreeMap<String, String>;

pub(super) fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && !value.contains(['=', '\0', '/', '\\'])
        && !value.starts_with('[')
        && !matches!(value, "." | "..")
        && !(value.as_bytes().get(1) == Some(&b':') && value.as_bytes()[0].is_ascii_alphabetic())
}
fn valid_extra(value: &str) -> bool {
    !value.is_empty()
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn validate_entry(entry: &WheelEntryPoint) -> Result<()> {
    ensure!(
        valid_name(entry.name()),
        "entry points: unsafe command name"
    );
    ensure!(
        !entry.module().is_empty()
            && entry
                .module()
                .split('.')
                .all(|part| IDENTIFIER.is_match(part)),
        "entry points: invalid module reference"
    );
    ensure!(
        !entry.attributes().is_empty()
            && entry
                .attributes()
                .iter()
                .all(|part| IDENTIFIER.is_match(part)),
        "entry points: invalid callable attribute reference"
    );
    ensure!(
        entry.extras().iter().all(|extra| valid_extra(extra)),
        "entry points: invalid extras"
    );
    Ok(())
}

fn append_bounded(output: &mut String, value: &str, limits: WheelScriptLimits) -> Result<()> {
    ensure!(
        value.len()
            <= limits
                .max_entrypoint_value_bytes
                .saturating_sub(output.len()),
        "entry points value byte limit exceeded"
    );
    output.push_str(value);
    Ok(())
}

fn interpolate(
    value: &str,
    values: &Options,
    depth: usize,
    limits: WheelScriptLimits,
) -> Result<String> {
    ensure!(depth <= 10, "entry points interpolation depth exceeded");
    let mut output = String::new();
    let mut at = 0;
    while let Some(percent) = value[at..].find('%') {
        let percent = at + percent;
        append_bounded(&mut output, &value[at..percent], limits)?;
        match value.as_bytes().get(percent + 1) {
            Some(b'%') => {
                append_bounded(&mut output, "%", limits)?;
                at = percent + 2;
            }
            Some(b'(') => {
                let end = value[percent + 2..]
                    .find(')')
                    .map(|end| percent + 2 + end)
                    .context("entry points interpolation has no closing key")?;
                ensure!(
                    end > percent + 2 && value.as_bytes().get(end + 1) == Some(&b's'),
                    "entry points interpolation syntax is invalid"
                );
                let key = &value[percent + 2..end];
                let replacement = values
                    .get(key)
                    .context("entry points interpolation key is missing")?;
                if replacement.contains('%') {
                    append_bounded(
                        &mut output,
                        &interpolate(replacement, values, depth + 1, limits)?,
                        limits,
                    )?;
                } else {
                    append_bounded(&mut output, replacement, limits)?;
                }
                at = end + 2;
            }
            _ => return Err(eyre!("entry points interpolation syntax is invalid")),
        }
    }
    append_bounded(&mut output, &value[at..], limits)?;
    Ok(output)
}

fn reference(name: String, value: &str, kind: WheelEntryPointKind) -> Result<WheelEntryPoint> {
    let value = value.trim();
    let (reference, extras) = if let Some((reference, bracket)) = value.split_once('[') {
        let inner = bracket
            .strip_suffix(']')
            .context("entry points: malformed extras")?;
        ensure!(
            !inner.contains(['[', ']']),
            "entry points: malformed extras"
        );
        let mut extras = Vec::new();
        let mut seen = BTreeSet::new();
        if !inner.trim().is_empty() {
            for extra in inner.split(',').map(str::trim) {
                ensure!(valid_extra(extra), "entry points: invalid extras");
                if seen.insert(extra) {
                    extras.push(extra.to_owned());
                }
            }
        }
        (reference.trim(), extras)
    } else {
        (value, Vec::new())
    };
    let (module, attributes) = reference
        .split_once(':')
        .context("entry points: callable reference requires attributes")?;
    let entry = WheelEntryPoint {
        name,
        module: module.trim().to_owned(),
        attributes: attributes.trim().split('.').map(str::to_owned).collect(),
        extras,
        kind,
    };
    validate_entry(&entry)?;
    Ok(entry)
}

fn options_mut<'a>(
    section: &str,
    defaults: &'a mut Options,
    sections: &'a mut BTreeMap<String, Options>,
) -> &'a mut Options {
    if section == "DEFAULT" {
        defaults
    } else {
        sections.get_mut(section).unwrap()
    }
}

pub(super) fn parse(bytes: &[u8], limits: WheelScriptLimits) -> Result<Vec<WheelEntryPoint>> {
    super::validate_limits(limits)?;
    ensure!(
        bytes.len() <= limits.max_entrypoints_bytes,
        "entry points metadata byte limit exceeded"
    );
    let text = std::str::from_utf8(bytes).map_err(|_| eyre!("entry points: invalid UTF-8"))?;
    ensure!(!text.contains('\0'), "entry points: NUL in metadata");
    let mut defaults = Options::new();
    let mut sections: BTreeMap<String, Options> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut previous: Option<(String, String, usize, usize)> = None;
    let mut count = 0_usize;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let value = line.trim();
        if value.starts_with(['#', ';']) {
            continue;
        }
        if value.is_empty() {
            if let Some((_, _, _, pending)) = &mut previous {
                *pending += 1;
            }
            continue;
        }
        let indent = line.chars().take_while(|ch| ch.is_whitespace()).count();
        if let Some((section, key, initial_indent, pending)) = &previous
            && indent > *initial_indent
        {
            let field = options_mut(section, &mut defaults, &mut sections)
                .get_mut(key)
                .unwrap();
            let newlines = pending + 1;
            ensure!(
                newlines
                    <= limits
                        .max_entrypoint_value_bytes
                        .saturating_sub(field.len()),
                "entry points value byte limit exceeded"
            );
            for _ in 0..newlines {
                field.push('\n');
            }
            append_bounded(field, value, limits)?;
            previous.as_mut().unwrap().3 = 0;
            continue;
        }
        if let Some(header) = value.strip_prefix('[') {
            let end = header
                .find(']')
                .context("entry points INI section is unclosed")?;
            let name = &header[..end];
            ensure!(GROUP.is_match(name), "entry points INI group is invalid");
            if name != "DEFAULT" {
                ensure!(
                    !sections.contains_key(name),
                    "duplicate entry points INI section"
                );
                ensure!(
                    sections.len() < limits.max_entrypoints,
                    "entry points section count limit exceeded"
                );
                sections.insert(name.to_owned(), Options::new());
            }
            current = Some(name.to_owned());
            previous = None;
            continue;
        }
        let section = current
            .as_ref()
            .context("entry points INI option has no section")?;
        let (key, value) = value
            .split_once('=')
            .context("entry points INI requires an equals delimiter")?;
        let key = key.trim();
        let value = value.trim();
        ensure!(!key.is_empty(), "entry points INI key is empty");
        ensure!(
            value.len() <= limits.max_entrypoint_value_bytes,
            "entry points value byte limit exceeded"
        );
        count = count
            .checked_add(1)
            .context("entry points count limit overflow")?;
        ensure!(
            count <= limits.max_entrypoints,
            "entry points declaration count limit exceeded"
        );
        let table = options_mut(section, &mut defaults, &mut sections);
        ensure!(
            !table.contains_key(key),
            "duplicate entry points INI option"
        );
        table.insert(key.to_owned(), value.to_owned());
        previous = Some((section.clone(), key.to_owned(), indent, 0));
    }
    // ConfigParser joins continuation lines and strips outer whitespace before
    // interpolation. Case is retained in both option keys and lookup references.
    for table in std::iter::once(&mut defaults).chain(sections.values_mut()) {
        for value in table.values_mut() {
            *value = value.trim().to_owned();
        }
    }
    let mut output = Vec::new();
    for (group, kind) in [
        ("console_scripts", WheelEntryPointKind::Console),
        ("gui_scripts", WheelEntryPointKind::Gui),
    ] {
        let Some(section) = sections.get(group) else {
            continue;
        };
        let mut values = defaults.clone();
        values.extend(section.clone());
        for (name, raw) in &values {
            ensure!(
                output.len() < limits.max_entrypoints,
                "entry points wrapper count limit exceeded"
            );
            output.push(reference(
                name.clone(),
                &interpolate(raw, &values, 1, limits)?,
                kind,
            )?);
        }
    }
    Ok(output)
}
