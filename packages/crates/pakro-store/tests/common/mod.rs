use pakro_store::{Digest, PackageIdentity};

pub fn make_tgz() -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut builder = tar::Builder::new(&mut enc);
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "pkg/hello.txt", &b"hello"[..])
            .unwrap();
        builder.finish().unwrap();
    }
    enc.finish().unwrap()
}

pub fn identity_for(pkg: &str, version: &str, digest: Digest) -> PackageIdentity {
    format!("python:{pkg}@{version}+python@3.10.14/macos-aarch64/{digest}")
        .parse()
        .unwrap()
}
