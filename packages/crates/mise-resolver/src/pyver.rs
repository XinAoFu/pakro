//! PEP 440 constraint strings to pubgrub ranges. This is the only place in
//! the whole system that interprets python version semantics.

use std::str::FromStr;

use pep440_rs::{Operator, Version, VersionSpecifier, VersionSpecifiers};
use pubgrub::Ranges;

pub type PyRanges = Ranges<Version>;

/// An empty constraint means "any version". Multi-specifier sets intersect.
pub fn constraint_to_ranges(constraint: &str) -> Result<PyRanges, String> {
    let trimmed = constraint.trim();
    if trimmed.is_empty() {
        return Ok(Ranges::full());
    }
    let specs = VersionSpecifiers::from_str(trimmed).map_err(|e| e.to_string())?;
    let mut acc = Ranges::full();
    for spec in specs.iter() {
        acc = acc.intersection(&specifier_to_ranges(spec));
    }
    Ok(acc)
}

fn specifier_to_ranges(spec: &VersionSpecifier) -> PyRanges {
    let v = spec.version();
    match spec.operator() {
        Operator::Equal | Operator::ExactEqual => Ranges::singleton(v.clone()),
        // ==1.2.* : versions whose release starts with 1.2
        Operator::EqualStar => star_range(v),
        Operator::NotEqual => Ranges::singleton(v.clone()).complement(),
        Operator::NotEqualStar => star_range(v).complement(),
        // ~=1.4.2 : >=1.4.2, ==1.4.*
        Operator::TildeEqual => {
            let release = v.release();
            let upper = bump_last(release[..release.len() - 1].to_vec());
            Ranges::between(v.clone(), upper)
        }
        Operator::LessThan => Ranges::strictly_lower_than(v.clone()),
        Operator::LessThanEqual => Ranges::lower_than(v.clone()),
        Operator::GreaterThan => Ranges::strictly_higher_than(v.clone()),
        Operator::GreaterThanEqual => Ranges::higher_than(v.clone()),
    }
}

fn star_range(v: &Version) -> PyRanges {
    Ranges::between(
        Version::new(v.release().to_vec()),
        bump_last(v.release().to_vec()),
    )
}

fn bump_last(mut release: Vec<u64>) -> Version {
    let last = release.len() - 1;
    release[last] += 1;
    Version::new(release)
}
