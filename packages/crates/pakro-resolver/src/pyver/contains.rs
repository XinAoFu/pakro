//! A narrow compatibility bridge around the existing PEP 440 matcher.

use pep440_rs::{Operator, Version, VersionSpecifiers};

/// Match a version without adding any prerelease selection policy.
pub fn specifiers_contain(specifiers: &VersionSpecifiers, candidate: &Version) -> bool {
    specifiers.iter().all(|specifier| {
        if *specifier.operator() == Operator::LessThan && specifier.version().any_prerelease() {
            // pep440_rs 0.7 also excludes every same-release prerelease here.
            // PEP 440 explicitly permits them when the bound itself is a pre.
            candidate.clone().without_local() < *specifier.version()
        } else {
            specifier.contains(candidate)
        }
    })
}

#[cfg(test)]
#[path = "contains/tests.rs"]
mod tests;
