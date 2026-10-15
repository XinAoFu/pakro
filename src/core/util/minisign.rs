use crate::*;
use minisign_verify::*;
#[cfg(test)]
use std::sync::LazyLock;

// This key belongs to the upstream release fixtures. It is not a pakro trust root.
#[cfg(test)]
pub static UPSTREAM_RELEASE_PUB_KEY: LazyLock<String> = LazyLock::new(|| {
    include_str!("../../../packaging/resources/keys/minisign.pub")
        .to_string()
        .lines()
        .last()
        .unwrap()
        .to_string()
});

/// A pakro release key must be established before downloads can be trusted.
pub fn release_public_key() -> Result<&'static str> {
    eyre::bail!(
        "pakro release verification is not configured; install pakro from a local build or your package manager"
    )
}

pub fn verify(pub_key: &str, data: &[u8], sig: &str) -> Result<()> {
    let public_key = PublicKey::from_base64(pub_key)?;
    let signature = Signature::decode(sig)?;
    public_key.verify(data, &signature, false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn upstream_key_does_not_enable_pakro_release_downloads() {
        assert!(super::release_public_key().is_err());
    }
}
