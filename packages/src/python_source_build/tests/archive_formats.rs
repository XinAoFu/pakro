use super::*;
use std::io::Cursor;

fn zipped(entries: &[(&str, &[u8], u32)], method: zip::CompressionMethod) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes, mode) in entries {
        zip.start_file(
            *name,
            zip::write::SimpleFileOptions::default()
                .compression_method(method)
                .unix_permissions(*mode),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    let mut bytes = zip.finish().unwrap().into_inner();
    // Zip8's writer deliberately masks privileged bits. Restore only the
    // requested central-mode fixture facts so the extractor must reject them.
    let footer = bytes.len() - 22;
    let mut central =
        u32::from_le_bytes(bytes[footer + 16..footer + 20].try_into().unwrap()) as usize;
    for (_, _, mode) in entries {
        assert_eq!(&bytes[central..central + 4], b"PK\x01\x02");
        if *mode & 0o7000 != 0 {
            let attributes =
                u32::from_le_bytes(bytes[central + 38..central + 42].try_into().unwrap());
            let attributes = (attributes & 0xffff) | ((0o100000 | *mode) << 16);
            bytes[central + 38..central + 42].copy_from_slice(&attributes.to_le_bytes());
        }
        let length =
            46 + usize::from(u16::from_le_bytes(
                bytes[central + 28..central + 30].try_into().unwrap(),
            )) + usize::from(u16::from_le_bytes(
                bytes[central + 30..central + 32].try_into().unwrap(),
            )) + usize::from(u16::from_le_bytes(
                bytes[central + 32..central + 34].try_into().unwrap(),
            ));
        central += length;
    }
    bytes
}
fn xz_bundle(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(bundle(entries).as_slice())
        .read_to_end(&mut tar)
        .unwrap();
    let mut xz = xz2::write::XzEncoder::new(Vec::new(), 1);
    xz.write_all(&tar).unwrap();
    xz.finish().unwrap()
}
fn reject(bytes: &[u8], filename: &str, limits: SourceBuildLimits) -> String {
    let owner = tempfile::tempdir().unwrap();
    let scratch = owner.path().join("builds");
    std::fs::create_dir(&scratch).unwrap();
    let result = extract_raw_source(
        &mut source_named(bytes, filename, owner.path()),
        &scratch,
        Instant::now() + Duration::from_secs(10),
        limits,
    );
    let error = match result {
        Ok(_) => panic!("unsafe archive was accepted: {filename}"),
        Err(error) => format!("{error:#}"),
    };
    assert_eq!(
        std::fs::read_dir(&scratch).unwrap().count(),
        0,
        "failed extraction must clean only its private workspace"
    );
    assert!(!owner.path().join("outside.py").exists());
    error
}

#[test]
fn p6_zip_sdist_parses_original_declarations_and_owns_private_tree() {
    for method in [
        zip::CompressionMethod::Stored,
        zip::CompressionMethod::Deflated,
    ] {
        let owner = tempfile::tempdir().unwrap();
        let scratch = owner.path().join("builds");
        std::fs::create_dir(&scratch).unwrap();
        let bytes = zipped(
            &[
                (
                    "demo-1.0/pyproject.toml",
                    b"[build-system]\nrequires=[]\nbuild-backend='fixture'\n",
                    0o644,
                ),
                ("demo-1.0/tool", b"#!/bin/sh\nexit 0\n", 0o755),
            ],
            method,
        );
        let tree = extract_source(
            &mut source_named(&bytes, "demo-1.0.zip", owner.path()),
            &scratch,
            Instant::now() + Duration::from_secs(10),
            Default::default(),
        )
        .unwrap();
        assert_eq!(tree.backend, "fixture");
        assert!(tree.initial_requirements.is_empty());
        assert_eq!(tree.format(), SourceArchiveFormat::Zip);
        assert_eq!(tree.extracted_entries, 3);
        assert_eq!(
            tree.read_relative(Path::new("tool"), 128).unwrap(),
            b"#!/bin/sh\nexit 0\n"
        );
        assert!(tree.read_relative(Path::new("../outside.py"), 128).is_err());
        let workspace = tree.workspace.path().to_owned();
        assert!(workspace.starts_with(&scratch));
        drop(tree);
        assert!(!workspace.exists());
    }
}

#[test]
fn p6_raw_archives_do_not_require_or_invent_python_package_declarations() {
    let owner = tempfile::tempdir().unwrap();
    let scratch = owner.path().join("builds");
    std::fs::create_dir(&scratch).unwrap();
    for (filename, bytes, format) in [
        (
            "source-fixture.tar.gz",
            bundle(&[("source/configure", b"fixture input\n")]),
            SourceArchiveFormat::TarGz,
        ),
        (
            "source-fixture.zip",
            zipped(
                &[("source/configure", b"fixture input\n", 0o755)],
                zip::CompressionMethod::Stored,
            ),
            SourceArchiveFormat::Zip,
        ),
        (
            "source-fixture.tar.xz",
            xz_bundle(&[("source/configure", b"fixture input\n")]),
            SourceArchiveFormat::TarXz,
        ),
    ] {
        let tree = extract_raw_source(
            &mut source_named(&bytes, filename, owner.path()),
            &scratch,
            Instant::now() + Duration::from_secs(10),
            Default::default(),
        )
        .unwrap();
        assert_eq!(tree.format(), format);
        assert_eq!(
            tree.read_relative(Path::new("configure"), 128).unwrap(),
            b"fixture input\n"
        );
        assert_eq!(tree.extracted_entries(), 2);
        assert!(tree.extracted_path_bytes() > 0 && tree.decoded_bytes() > 0);
        assert!(tree.root().is_dir());
        drop(tree);
        assert!(
            extract_source(
                &mut source_named(&bytes, filename, owner.path()),
                &scratch,
                Instant::now() + Duration::from_secs(10),
                Default::default()
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    }
}

#[test]
fn p6_zip_sdist_rejects_bad_crc_truncated_and_unrecorded_trailer() {
    let valid = zipped(
        &[("demo-1.0/setup.py", b"CRC_SENTINEL_PAYLOAD", 0o644)],
        zip::CompressionMethod::Stored,
    );
    let mut bad_crc = valid.clone();
    let at = bad_crc
        .windows(b"CRC_SENTINEL_PAYLOAD".len())
        .position(|bytes| bytes == b"CRC_SENTINEL_PAYLOAD")
        .unwrap();
    bad_crc[at] ^= 1;
    let error = reject(&bad_crc, "demo-1.0.zip", Default::default());
    assert!(
        error.contains("CRC") || error.contains("checksum"),
        "{error}"
    );
    let mut truncated = valid.clone();
    truncated.truncate(truncated.len() - 4);
    reject(&truncated, "demo-1.0.zip", Default::default());
    let mut trailer = valid;
    trailer.extend_from_slice(b"unrecorded archive tail");
    reject(&trailer, "demo-1.0.zip", Default::default());
}

#[test]
fn p6_zip_sdist_rejects_raw_dotdot_dot_bundle_conflicts_and_privileged_modes() {
    for path in [
        "demo-1.0/../outside.py",
        "demo-1.0/./setup.py",
        "/outside.py",
        "demo-1.0\\outside.py",
        "demo-1.0",
    ] {
        let bytes = zipped(&[(path, b"pass\n", 0o644)], zip::CompressionMethod::Stored);
        reject(&bytes, "demo-1.0.zip", Default::default());
    }
    let bytes = zipped(
        &[
            ("demo-1.0/setup.py", b"pass\n", 0o644),
            ("other/payload.py", b"pass\n", 0o644),
        ],
        zip::CompressionMethod::Stored,
    );
    reject(&bytes, "demo-1.0.zip", Default::default());
    let bytes = zipped(
        &[("demo-1.0/setup.py", b"pass\n", 0o4755)],
        zip::CompressionMethod::Stored,
    );
    reject(&bytes, "demo-1.0.zip", Default::default());
}

#[test]
fn p6_zip_and_xz_source_links_are_rejected_before_publication() {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.add_symlink(
        "demo-1.0/link",
        "../../outside",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    reject(
        &zip.finish().unwrap().into_inner(),
        "demo-1.0.zip",
        Default::default(),
    );
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_mode(0o777);
    header.set_size(0);
    header.set_link_name("../../outside").unwrap();
    header.set_cksum();
    tar.append_data(&mut header, "source/link", &[][..])
        .unwrap();
    let mut xz = xz2::write::XzEncoder::new(Vec::new(), 1);
    xz.write_all(&tar.into_inner().unwrap()).unwrap();
    reject(
        &xz.finish().unwrap(),
        "source-fixture.tar.xz",
        Default::default(),
    );
}

#[test]
fn p6_source_archives_enforce_central_entries_expansion_and_real_directory_budgets() {
    let bytes = zipped(
        &[
            ("demo-1.0/setup.py", b"pass\n", 0o644),
            ("demo-1.0/a/b/payload.py", b"data", 0o644),
        ],
        zip::CompressionMethod::Deflated,
    );
    for limits in [
        SourceBuildLimits {
            max_entries: 1,
            ..Default::default()
        },
        SourceBuildLimits {
            max_entries: 3,
            ..Default::default()
        },
        SourceBuildLimits {
            max_total_path_bytes: 16,
            ..Default::default()
        },
        SourceBuildLimits {
            max_file_bytes: 1,
            ..Default::default()
        },
        SourceBuildLimits {
            max_decoded_bytes: 2,
            ..Default::default()
        },
        SourceBuildLimits {
            max_archive_bytes: 16,
            ..Default::default()
        },
    ] {
        reject(&bytes, "demo-1.0.zip", limits);
    }
    reject(
        &xz_bundle(&[("source/configure", b"payload")]),
        "source-fixture.tar.xz",
        SourceBuildLimits {
            max_decoded_bytes: 128,
            ..Default::default()
        },
    );
}

#[test]
fn p6_xz_source_requires_bounded_decoder_and_complete_single_stream() {
    let bytes = xz_bundle(&[("source/configure", b"input bytes")]);
    let error = reject(
        &bytes,
        "source-fixture.tar.xz",
        SourceBuildLimits {
            max_decoder_memory_bytes: 1,
            ..Default::default()
        },
    );
    assert!(
        error.contains("memory") || error.contains("Memory") || error.contains("Memlimit"),
        "{error}"
    );
    reject(
        &bytes,
        "source-fixture.tar.xz",
        SourceBuildLimits {
            max_decoder_memory_bytes: 64 * 1024 * 1024 + 1,
            ..Default::default()
        },
    );
    let mut truncated = bytes.clone();
    truncated.truncate(truncated.len() - 4);
    reject(&truncated, "source-fixture.tar.xz", Default::default());
    let mut crc = bytes.clone();
    let end = crc.len();
    crc[end - 8] ^= 1;
    reject(&crc, "source-fixture.tar.xz", Default::default());
    let mut trailer = bytes.clone();
    trailer.extend_from_slice(b"tail");
    reject(&trailer, "source-fixture.tar.xz", Default::default());
    let mut concatenated = bytes.clone();
    concatenated.extend_from_slice(&bytes);
    reject(&concatenated, "source-fixture.tar.xz", Default::default());
    let mut padding = bytes;
    padding.extend_from_slice(&[0; 4]);
    reject(&padding, "source-fixture.tar.xz", Default::default());
}

#[test]
fn p6_expired_raw_source_extraction_creates_no_workspace() {
    let owner = tempfile::tempdir().unwrap();
    let scratch = owner.path().join("absent-builds");
    let bytes = zipped(
        &[("source/input", b"payload", 0o644)],
        zip::CompressionMethod::Stored,
    );
    assert!(
        extract_raw_source(
            &mut source_named(&bytes, "source.zip", owner.path()),
            &scratch,
            Instant::now() - Duration::from_millis(1),
            Default::default()
        )
        .is_err()
    );
    assert!(!scratch.exists());
}

/// This checks an authenticated official source archive as raw input only.
/// It does not execute configure, build Python, or publish a language object.
#[test]
#[ignore = "requires the original verified CPython 3.13.16 official tar.xz via PAKRO_TEST_CPYTHON_SOURCE_ARCHIVE"]
fn p6_real_cpython_source_xz_extracts_owned_raw_tree() {
    const FILENAME: &str = "Python-3.13.16.tar.xz";
    const SIZE: u64 = 23_225_704;
    const DIGEST: &str = "sha256-f4b1bfb3c79b5bb11b8d228a12504163b4c0dab4d679828d8f5f26b6cb6ab35d";
    let input = std::path::PathBuf::from(
        std::env::var_os("PAKRO_TEST_CPYTHON_SOURCE_ARCHIVE").expect(
            "PAKRO_TEST_CPYTHON_SOURCE_ARCHIVE must identify the original official CPython archive",
        ),
    );
    assert_eq!(input.file_name(), Some(std::ffi::OsStr::new(FILENAME)));
    let original = std::fs::symlink_metadata(&input).unwrap();
    assert!(original.is_file() && !original.file_type().is_symlink());
    assert_eq!(original.len(), SIZE);
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let scratch = root.join("builds");
    let digest: pakro_store::Digest = DIGEST.parse().unwrap();
    let mut artifact = spool_verified_reader(
        std::fs::File::open(&input).unwrap(),
        FILENAME,
        digest,
        SIZE,
        &root.join("snapshots"),
    )
    .unwrap();
    assert_eq!(artifact.filename(), FILENAME);
    assert_eq!(artifact.digest(), digest);
    assert_eq!(artifact.size(), SIZE);
    let tree = extract_raw_source(
        &mut artifact,
        &scratch,
        Instant::now() + Duration::from_secs(120),
        SourceBuildLimits::default(),
    )
    .unwrap();
    assert_eq!(tree.format(), SourceArchiveFormat::TarXz);
    assert_eq!(tree.extracted_entries(), 5_272);
    assert_eq!(
        tree.root().file_name(),
        Some(std::ffi::OsStr::new("Python-3.13.16"))
    );
    for (relative, signature, maximum) in [
        ("configure", b"GNU Autoconf".as_slice(), 8 * 1024 * 1024),
        ("Include/Python.h", b"Py_PYTHON_H".as_slice(), 64 * 1024),
        ("Modules/_ssl.c", b"PyInit__ssl".as_slice(), 2 * 1024 * 1024),
        ("Lib/ssl.py", b"class SSLContext".as_slice(), 1024 * 1024),
        (
            "Lib/encodings/__init__.py",
            b"search_function".as_slice(),
            1024 * 1024,
        ),
        ("Lib/os.py", b"def _execvpe".as_slice(), 1024 * 1024),
    ] {
        let bytes = tree.read_relative(Path::new(relative), maximum).unwrap();
        assert!(
            bytes
                .windows(signature.len())
                .any(|window| window == signature),
            "official raw source member is missing its expected content: {relative}"
        );
    }
    let decoded = tree.decoded_bytes();
    let workspace = tree.workspace.path().to_owned();
    let bundle = tree.root().to_owned();
    assert!(workspace.starts_with(&scratch) && bundle.starts_with(&workspace));
    drop(tree);
    assert!(!workspace.exists() && !bundle.exists());
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    drop(artifact);
    assert_eq!(std::fs::symlink_metadata(&input).unwrap().len(), SIZE);
    assert!(
        !root.join("data").exists(),
        "raw extraction must not initialize or publish a language library"
    );
    println!(
        "P6_CPYTHON_RAW_COMPLETE version=3.13.16 entries=5272 decoded_bytes={decoded} digest={DIGEST}"
    );
}
