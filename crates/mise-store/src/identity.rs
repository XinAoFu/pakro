use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::Digest as Sha2Digest;
use sha2::{Sha256, Sha512};

use crate::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eco {
    Python,
    Node,
}

impl Eco {
    pub fn dir_name(self) -> &'static str {
        match self {
            Eco::Python => "python",
            Eco::Node => "node",
        }
    }
}

impl fmt::Display for Eco {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir_name())
    }
}

impl FromStr for Eco {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "python" => Ok(Eco::Python),
            "node" => Ok(Eco::Node),
            other => Err(StoreError::UnknownEco(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Macos,
    Linux,
}

impl Os {
    pub fn as_str(self) -> &'static str {
        match self {
            Os::Macos => "macos",
            Os::Linux => "linux",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Libc {
    Gnu,
    Musl,
    Other,
}

impl Libc {
    pub fn as_str(self) -> &'static str {
        match self {
            Libc::Gnu => "gnu",
            Libc::Musl => "musl",
            Libc::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
    pub libc: Libc,
}

impl Platform {
    /// Compile-time host detection. The libc field only distinguishes builds
    /// mise itself was compiled for; runtime libc probing on Linux arrives
    /// with the artifact matching work in M1.
    pub fn detect() -> Self {
        let os = if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        };
        let arch = if cfg!(target_arch = "aarch64") {
            Arch::Aarch64
        } else {
            Arch::X86_64
        };
        let libc = if cfg!(target_os = "linux") {
            if cfg!(target_env = "musl") {
                Libc::Musl
            } else {
                Libc::Gnu
            }
        } else {
            Libc::Other
        };
        Self { os, arch, libc }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.os {
            Os::Linux => write!(
                f,
                "{}-{}-{}",
                self.os.as_str(),
                self.arch.as_str(),
                self.libc.as_str()
            ),
            Os::Macos => write!(f, "{}-{}", self.os.as_str(), self.arch.as_str()),
        }
    }
}

impl FromStr for Platform {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || StoreError::InvalidIdentity(format!("invalid platform: {s}"));
        let mut parts = s.split('-');
        let (Some(os), Some(arch), maybe_libc) = (parts.next(), parts.next(), parts.next()) else {
            return Err(invalid());
        };
        if parts.next().is_some() {
            return Err(invalid());
        }
        let os = match os {
            "macos" => Os::Macos,
            "linux" => Os::Linux,
            _ => return Err(invalid()),
        };
        let arch = match arch {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            _ => return Err(invalid()),
        };
        let libc = match (os, maybe_libc) {
            (Os::Linux, Some("gnu")) => Libc::Gnu,
            (Os::Linux, Some("musl")) => Libc::Musl,
            (Os::Linux, None) => Libc::Gnu,
            (Os::Macos, None) => Libc::Other,
            _ => return Err(invalid()),
        };
        Ok(Self { os, arch, libc })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuntimeId {
    pub eco: Eco,
    pub version: String,
}

impl fmt::Display for RuntimeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.eco, self.version)
    }
}

impl FromStr for RuntimeId {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || StoreError::InvalidIdentity(format!("invalid runtime id: {s}"));
        let (eco, version) = s.split_once('@').ok_or_else(invalid)?;
        if version.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            eco: eco.parse()?,
            version: version.to_string(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Digest {
    Sha256([u8; 32]),
    Sha512([u8; 64]),
}

impl Digest {
    pub fn from_sha256_bytes(bytes: &[u8]) -> Self {
        let mut out = [0u8; 32];
        out.copy_from_slice(&Sha256::digest(bytes));
        Digest::Sha256(out)
    }

    pub fn from_sha512_bytes(bytes: &[u8]) -> Self {
        let mut out = [0u8; 64];
        out.copy_from_slice(&Sha512::digest(bytes));
        Digest::Sha512(out)
    }

    pub fn hex(&self) -> String {
        match self {
            Digest::Sha256(bytes) => hex::encode(bytes),
            Digest::Sha512(bytes) => hex::encode(bytes),
        }
    }

    /// Recomputes the digest of `bytes` and compares. The algorithm comes
    /// from the prefix, so a sha512 identity can never verify against
    /// sha256 content.
    pub fn verify(&self, bytes: &[u8]) -> bool {
        *self
            == match self {
                Digest::Sha256(_) => Digest::from_sha256_bytes(bytes),
                Digest::Sha512(_) => Digest::from_sha512_bytes(bytes),
            }
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{}",
            match self {
                Digest::Sha256(_) => "sha256",
                Digest::Sha512(_) => "sha512",
            },
            self.hex()
        )
    }
}

impl FromStr for Digest {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = |reason: &str| StoreError::InvalidDigest(format!("{s}: {reason}"));
        let (algorithm, hex_part) = s
            .split_once('-')
            .ok_or_else(|| invalid("expected <algorithm>-<hex>"))?;
        if !matches!(algorithm, "sha256" | "sha512") {
            return Err(invalid("unsupported algorithm"));
        }
        let bytes = hex::decode(hex_part).map_err(|_| invalid("not hexadecimal"))?;
        match algorithm {
            "sha256" => Ok(Digest::Sha256(
                bytes
                    .try_into()
                    .map_err(|_| invalid("expected 64 hex characters"))?,
            )),
            _ => Ok(Digest::Sha512(
                bytes
                    .try_into()
                    .map_err(|_| invalid("expected 128 hex characters"))?,
            )),
        }
    }
}

impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageIdentity {
    pub eco: Eco,
    pub name: String,
    pub version: String,
    pub runtime: Option<RuntimeId>,
    pub platform: Platform,
    pub digest: Digest,
}

impl PackageIdentity {
    /// Directory component for the package name. npm scoped names contain '/'
    /// which cannot span a single path segment, and '+' is not legal in either
    /// ecosystem's package names, so the rewrite is collision-free.
    pub fn dir_name(&self) -> String {
        self.name.replace('/', "+")
    }
}

impl fmt::Display for PackageIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}@{}", self.eco, self.name, self.version)?;
        if let Some(runtime) = &self.runtime {
            write!(f, "+{runtime}")?;
        }
        write!(f, "/{}/{}", self.platform, self.digest)
    }
}

impl FromStr for PackageIdentity {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || StoreError::InvalidIdentity(s.to_string());
        // npm scoped names contain '/', so platform and digest come off the
        // right and everything left of them is the name segment
        let mut parts = s.rsplitn(3, '/');
        let digest = parts.next().ok_or_else(invalid)?.parse()?;
        let platform = parts.next().ok_or_else(invalid)?.parse()?;
        let head = parts.next().ok_or_else(invalid)?;
        let (eco_str, spec) = head.split_once(':').ok_or_else(invalid)?;
        // '+' never occurs in supported ecosystems' package names, so the
        // first '+' separates the optional runtime binding
        let (pkg, runtime) = match spec.split_once('+') {
            Some((pkg, runtime)) => (pkg, Some(runtime.parse()?)),
            None => (spec, None),
        };
        let (name, version) = pkg.rsplit_once('@').ok_or_else(invalid)?;
        if name.is_empty() || version.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            eco: eco_str.parse()?,
            name: name.to_string(),
            version: version.to_string(),
            runtime,
            platform,
            digest,
        })
    }
}
