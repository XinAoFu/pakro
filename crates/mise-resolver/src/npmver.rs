//! npm range grammar to pubgrub ranges: caret, tilde, x-ranges, comparators,
//! hyphen ranges, and `||` unions, using node-semver's Version for parsing
//! and ordering. Prerelease exclusion is enforced one layer up (the metadata
//! provider filters prereleases unless exactly pinned) because "no
//! prerelease" is not expressible as an interval set.

use std::str::FromStr;

use node_semver::Version;
use pubgrub::Ranges;

pub type NpmRanges = Ranges<Version>;

pub fn range_to_ranges(range: &str) -> Result<NpmRanges, String> {
    let trimmed = range.trim();
    if trimmed.is_empty() {
        return Ok(Ranges::full());
    }
    let mut acc = Ranges::empty();
    for branch in trimmed.split("||") {
        acc = acc.union(&branch_to_ranges(branch)?);
    }
    Ok(acc)
}

fn branch_to_ranges(branch: &str) -> Result<NpmRanges, String> {
    let tokens: Vec<&str> = branch.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(format!("empty branch in {branch:?}"));
    }
    // hyphen range: a bare "-" token between two partials, "1.2.3 - 2.3.4"
    if let Some(pos) = tokens.iter().position(|t| *t == "-") {
        if tokens.len() < 3 || pos == 0 || pos + 1 >= tokens.len() {
            return Err(format!("malformed hyphen range {branch:?}"));
        }
        if tokens.iter().filter(|t| **t == "-").count() != 1 {
            return Err(format!("malformed hyphen range {branch:?}"));
        }
        let lo = parse_partial(tokens[pos - 1])?;
        let hi = parse_partial(tokens[pos + 1])?;
        let range = Ranges::higher_than(fill_lower(&lo));
        // a full upper bound is inclusive; a partial one is an x-range
        let upper = match (hi.minor, hi.patch) {
            (Some(_), Some(_)) => Ranges::lower_than(fill_lower(&hi)),
            (None, _) => Ranges::strictly_lower_than(bump_major(hi.major)),
            (Some(minor), None) => Ranges::strictly_lower_than(bump_minor(hi.major, minor)),
        };
        return Ok(range.intersection(&upper));
    }
    let mut merged: Vec<String> = vec![];
    let mut tokens = tokens.iter().peekable();
    while let Some(token) = tokens.next() {
        if matches!(*token, ">=" | "<=" | ">" | "<" | "=") && tokens.peek().is_some() {
            let next = tokens.next().expect("peeked");
            merged.push(format!("{token}{next}"));
        } else {
            merged.push((*token).to_string());
        }
    }
    let mut acc = Ranges::full();
    for token in &merged {
        acc = acc.intersection(&comparator_to_ranges(token)?);
    }
    Ok(acc)
}

fn comparator_to_ranges(token: &str) -> Result<NpmRanges, String> {
    if matches!(token, "*" | "x" | "X") {
        return Ok(Ranges::full());
    }
    for (op, strip) in [
        (">=", ">="),
        ("<=", "<="),
        (">", ">"),
        ("<", "<"),
        ("=", "="),
    ] {
        if let Some(rest) = token.strip_prefix(strip) {
            // a full semver (possibly prerelease) is used as-is; partials
            // complete with zeros
            let v = Version::from_str(rest)
                .or_else(|_| parse_partial(rest).map(|partial| fill_lower(&partial)))?;
            return Ok(match op {
                ">=" => Ranges::higher_than(v),
                "<=" => Ranges::lower_than(v),
                ">" => Ranges::strictly_higher_than(v),
                "<" => Ranges::strictly_lower_than(v),
                _ => Ranges::singleton(v),
            });
        }
    }
    if let Some(rest) = token.strip_prefix('^') {
        return caret_ranges(&parse_partial(rest)?);
    }
    if let Some(rest) = token.strip_prefix('~') {
        return tilde_ranges(&parse_partial(rest)?);
    }
    // bare token: a full version (possibly prerelease) is exact, a partial
    // is an x-range
    if let Ok(full) = Version::from_str(token) {
        return Ok(Ranges::singleton(full));
    }
    let partial = parse_partial(token)?;
    if partial.minor.is_some() && partial.patch.is_some() {
        Ok(Ranges::singleton(fill_lower(&partial)))
    } else {
        x_ranges(&partial)
    }
}

/// `^`: the leftmost non-zero segment may move.
fn caret_ranges(partial: &Partial) -> Result<NpmRanges, String> {
    let lo = fill_lower(partial);
    let upper = match (partial.minor, partial.patch) {
        (None, _) => bump_major(partial.major),
        (Some(minor), None) => {
            if partial.major == 0 {
                bump_minor(0, minor)
            } else {
                bump_major(partial.major)
            }
        }
        (Some(minor), Some(_)) => {
            if partial.major > 0 {
                bump_major(partial.major)
            } else if minor > 0 {
                bump_minor(0, minor)
            } else {
                // ^0.0.x pins the patch exactly
                return Ok(Ranges::singleton(lo));
            }
        }
    };
    Ok(Ranges::between(lo, upper))
}

/// `~`: patch may move when the minor is named, minor may move when it is
/// not.
fn tilde_ranges(partial: &Partial) -> Result<NpmRanges, String> {
    let lo = fill_lower(&fill_to_minor(partial));
    let upper = match partial.minor {
        None => bump_major(partial.major),
        Some(minor) => bump_minor(partial.major, minor),
    };
    Ok(Ranges::between(lo, upper))
}

fn x_ranges(partial: &Partial) -> Result<NpmRanges, String> {
    let upper = match partial.minor {
        None => bump_major(partial.major),
        Some(minor) => bump_minor(partial.major, minor),
    };
    Ok(Ranges::between(fill_lower(&fill_to_minor(partial)), upper))
}

#[derive(Debug, Clone, Copy)]
struct Partial {
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
}

/// Parses `1`, `1.2`, `1.2.3`, optionally with prerelease/build suffixes on
/// full versions. Dist-tags (`latest`, `next`, …) fail here on purpose: the
/// strict-version policy resolves nothing implicitly.
fn parse_partial(text: &str) -> Result<Partial, String> {
    let text = text.trim();
    let bad = || format!("invalid version {text:?}");
    let core = match text.split_once(['-', '+']) {
        Some((core, _)) => core,
        None => text,
    };
    let num = |part: &str| -> Result<Option<u64>, String> {
        match part {
            "*" | "x" | "X" => Ok(None),
            other => other.parse().map(Some).map_err(|_| bad()),
        }
    };
    let mut parts = core.split('.');
    let Some(first) = parts.next() else {
        return Err(bad());
    };
    // "*" in the major position alone never carries information; reject
    let Some(major) = num(first)? else {
        return Err(bad());
    };
    let minor = parts.next().map(num).transpose()?.flatten();
    let patch = parts.next().map(num).transpose()?.flatten();
    if parts.next().is_some() {
        return Err(bad());
    }
    Ok(Partial {
        major,
        minor,
        patch,
    })
}

fn fill_lower(partial: &Partial) -> Version {
    version(
        partial.major,
        partial.minor.unwrap_or(0),
        partial.patch.unwrap_or(0),
    )
}

fn fill_to_minor(partial: &Partial) -> Partial {
    Partial {
        major: partial.major,
        minor: Some(partial.minor.unwrap_or(0)),
        patch: partial.patch,
    }
}

fn version(major: u64, minor: u64, patch: u64) -> Version {
    Version::from_str(&format!("{major}.{minor}.{patch}")).expect("synthesized semver parses")
}

fn bump_major(major: u64) -> Version {
    version(major + 1, 0, 0)
}

fn bump_minor(major: u64, minor: u64) -> Version {
    version(major, minor + 1, 0)
}
