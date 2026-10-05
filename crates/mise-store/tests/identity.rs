use mise_store::{Digest, Eco, PackageIdentity, Platform, RuntimeId};

fn hex64(c: char) -> String {
    std::iter::repeat_n(c, 64).collect()
}

#[test]
fn identity_roundtrip_python() {
    let s = format!(
        "python:numpy@4.2.2+python@3.10.14/macos-aarch64/sha256-{}",
        hex64('a')
    );
    let id: PackageIdentity = s.parse().unwrap();
    assert_eq!(id.eco, Eco::Python);
    assert_eq!(id.name, "numpy");
    assert_eq!(id.version, "4.2.2");
    assert_eq!(
        id.runtime.clone().unwrap(),
        RuntimeId {
            eco: Eco::Python,
            version: "3.10.14".into()
        }
    );
    assert_eq!(id.platform, "macos-aarch64".parse().unwrap());
    assert_eq!(id.to_string(), s);
}

#[test]
fn identity_roundtrip_node_scoped() {
    // npm scoped package names contain '/', so platform and digest must be
    // split from the right
    let s = format!(
        "node:@types/node@5.8.2/linux-x86_64-gnu/sha256-{}",
        hex64('b')
    );
    let id: PackageIdentity = s.parse().unwrap();
    assert_eq!(id.eco, Eco::Node);
    assert_eq!(id.name, "@types/node");
    assert_eq!(id.version, "5.8.2");
    assert!(id.runtime.is_none());
    assert_eq!(id.to_string(), s);
}

#[test]
fn identity_rejects_garbage() {
    assert!(
        "python:numpy/macos-aarch64/sha256-aa"
            .parse::<PackageIdentity>()
            .is_err()
    );
    assert!(
        format!("ruby:rails@1.0.0/macos-aarch64/sha256-{}", hex64('d'))
            .parse::<PackageIdentity>()
            .is_err()
    );
    assert!(
        format!("python:numpy@1.0.0/macos-aarch64/sha256-{}", hex64('a'))
            .replace('a', "z")
            .parse::<PackageIdentity>()
            .is_err()
    );
    assert!(
        "python:numpy@1.0.0+python@/macos-aarch64/sha256-aaaa"
            .parse::<PackageIdentity>()
            .is_err()
    );
}

#[test]
fn digest_hashing_and_parsing() {
    let d = Digest::from_sha256_bytes(b"hello");
    assert_ne!(
        Digest::from_sha256_bytes(b"world").to_string(),
        d.to_string()
    );
    assert_eq!(d, d.to_string().parse().unwrap());
    assert!("sha256-short".parse::<Digest>().is_err());
    assert!(
        "md5-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            .parse::<Digest>()
            .is_err()
    );
}

#[test]
fn platform_parsing_and_detect() {
    let mac: Platform = "macos-aarch64".parse().unwrap();
    assert_eq!(mac.to_string(), "macos-aarch64");
    let linux: Platform = "linux-x86_64-musl".parse().unwrap();
    assert_eq!(linux.to_string(), "linux-x86_64-musl");
    // a linux platform string without a libc segment defaults to gnu
    let default_linux: Platform = "linux-x86_64".parse().unwrap();
    assert_eq!(default_linux.to_string(), "linux-x86_64-gnu");
    assert!("windows-x86_64".parse::<Platform>().is_err());
    assert!("macos-x86_64-gnu".parse::<Platform>().is_err());

    let detected = Platform::detect().to_string();
    assert!(detected.starts_with("macos-") || detected.starts_with("linux-"));
    if detected.starts_with("linux-") {
        assert!(detected.ends_with("-gnu") || detected.ends_with("-musl"));
    }
}

#[test]
fn identity_dir_name_rewrites_scoped_separator() {
    let id: PackageIdentity = format!(
        "node:@types/node@5.8.2/linux-x86_64-gnu/sha256-{}",
        hex64('e')
    )
    .parse()
    .unwrap();
    assert_eq!(id.dir_name(), "@types+node");
}
