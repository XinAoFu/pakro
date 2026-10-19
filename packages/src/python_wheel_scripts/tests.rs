use super::*;

use sha2::{Digest as _, Sha256};
use std::io::{self, Cursor};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::Command;

fn target() -> PythonScriptTarget<'static> {
    PythonScriptTarget {
        final_prefix: Path::new("/opt/pakro/env"),
        interpreter: Path::new("/opt/pakro/env/bin/python"),
    }
}
fn entry(name: &str, module: &str, attributes: &[&str]) -> WheelEntryPoint {
    // A direct, valid value fixture keeps generator tests independent of the
    // deliberately wrong parse_entry_points baseline.
    WheelEntryPoint {
        name: name.into(),
        module: module.into(),
        attributes: attributes.iter().map(|value| (*value).to_owned()).collect(),
        extras: Vec::new(),
        kind: WheelEntryPointKind::Console,
    }
}
fn parsed(text: &str) -> Vec<WheelEntryPoint> {
    parse_entry_points(text.as_bytes(), WheelScriptLimits::default()).unwrap()
}
fn parse_rejected(bytes: &[u8], diagnostic: &str) {
    let error = parse_entry_points(bytes, WheelScriptLimits::default()).unwrap_err();
    assert!(
        error.to_string().contains(diagnostic),
        "expected {diagnostic:?}, got {error}"
    );
}
fn rewritten(input: &[u8], mode: u32) -> (Vec<u8>, WheelScriptTransform) {
    let mut output = Vec::new();
    let stats = rewrite_python_script(
        Cursor::new(input),
        &mut output,
        &target(),
        mode,
        WheelScriptLimits::default(),
    )
    .unwrap();
    (output, stats)
}
fn assert_stats(
    input: &[u8],
    output: &[u8],
    stats: &WheelScriptTransform,
    operation: WheelScriptOperation,
    mode: u32,
) {
    assert_eq!(stats.operation(), operation);
    assert_eq!(stats.raw_size(), input.len() as u64);
    assert_eq!(
        stats.raw_sha256().as_slice(),
        Sha256::digest(input).as_slice()
    );
    assert_eq!(stats.size(), output.len() as u64);
    assert_eq!(stats.sha256().as_slice(), Sha256::digest(output).as_slice());
    assert_eq!(stats.mode(), mode);
}

#[test]
fn console_gui_names_are_case_sensitive_and_plugin_groups_do_not_create_commands() {
    let values = parsed(
        "[console_scripts]\nTool = demo:main\ntool = demo:other\n[gui_scripts]\nWindow = demo:ui.run\n[pytest11]\nplugin = demo.plugin\n[CONSOLE_SCRIPTS]\nnot_a_command = demo:extra\n",
    );
    assert_eq!(
        values.iter().map(WheelEntryPoint::name).collect::<Vec<_>>(),
        ["Tool", "tool", "Window"]
    );
    assert_eq!(values[0].kind(), WheelEntryPointKind::Console);
    assert_eq!(values[0].module(), "demo");
    assert_eq!(values[2].kind(), WheelEntryPointKind::Gui);
    assert_eq!(values[2].attributes(), ["ui", "run"]);
}

#[test]
fn unicode_names_targets_and_legal_basename_punctuation_are_retained() {
    let values = parsed("[console_scripts]\n工具,\" helper = 包.模块 : 运行.入口\n");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].name(), "工具,\" helper");
    assert_eq!(values[0].module(), "包.模块");
    assert_eq!(values[0].attributes(), ["运行", "入口"]);
}

#[test]
fn old_extras_and_reference_whitespace_are_parsed_without_installing_dependencies() {
    let values = parsed(
        "[console_scripts]\nrun = demo.module   :   outer.call   [ feature_a , feature-b ]   \n",
    );
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].module(), "demo.module");
    assert_eq!(values[0].attributes(), ["outer", "call"]);
    assert_eq!(values[0].extras(), ["feature_a", "feature-b"]);
    let empty = parsed("[console_scripts]\nrun = demo:main []\n");
    assert_eq!(empty.len(), 1);
    assert!(empty[0].extras().is_empty());
}

#[test]
fn defaults_are_inherited_and_explicit_section_overrides_are_case_sensitive() {
    let values = parsed(
        "[DEFAULT]\nRun = demo:default\nother = demo:other\n[console_scripts]\nRun = demo:explicit\n[gui_scripts]\nwindow = demo:window\n",
    );
    let console: Vec<_> = values
        .iter()
        .filter(|value| value.kind() == WheelEntryPointKind::Console)
        .collect();
    assert_eq!(console.len(), 2);
    assert_eq!(console[0].name(), "Run");
    assert_eq!(console[0].attributes(), ["explicit"]);
    assert_eq!(
        values
            .iter()
            .filter(|value| value.kind() == WheelEntryPointKind::Gui)
            .count(),
        3
    );
    assert!(parsed("[DEFAULT]\nrun = demo:main\n").is_empty());
    // ConfigParser treats repeated DEFAULT blocks as the same defaults table;
    // section-header suffix text is also accepted by its INI reader.
    let repeated = parsed(
        "[DEFAULT]\nfirst=demo:one\n[DEFAULT]\nsecond=demo:two\n[console_scripts] suffix text\nrun=demo:run\n",
    );
    assert_eq!(
        repeated
            .iter()
            .map(WheelEntryPoint::name)
            .collect::<Vec<_>>(),
        ["first", "run", "second"]
    );
}

#[test]
fn interpolation_does_not_hide_invalid_inherited_command_values() {
    // DEFAULT/section values in an existing script group are themselves
    // commands, so these bare helper modules cannot become hidden aliases.
    parse_rejected(b"[DEFAULT]\nmodule = demo\n[console_scripts]\nrun = %(target)s\ntarget = %(module)s:main\nmodule = package\n", "entry points");
}

#[test]
fn interpolation_of_valid_default_entrypoint_values_and_overrides_works() {
    let values = parsed(
        "[DEFAULT]\nbase = demo:main\n[console_scripts]\ncopy = %(base)s\nbase = other:run\n",
    );
    assert_eq!(values.len(), 2);
    for value in values {
        assert_eq!(
            (value.module(), value.attributes()),
            ("other", ["run".to_string()].as_slice())
        );
    }
}

#[test]
fn interpolation_missing_keys_syntax_recursion_and_expansion_limits_are_errors() {
    for text in [
        "[console_scripts]\nrun = %(missing)s\n",
        "[console_scripts]\nrun = %bad\n",
        "[console_scripts]\nrun = %(run)s\n",
        "[console_scripts]\nrun = ${module}:main\n",
        "[console_scripts]\nrun = demo:main %%\n",
    ] {
        parse_rejected(text.as_bytes(), "entry points");
    }
    let error = parse_entry_points(
        b"[DEFAULT]\nbase = demo:main\n[console_scripts]\ncopy = %(base)s%(base)s\n",
        WheelScriptLimits {
            max_entrypoint_value_bytes: 12,
            ..WheelScriptLimits::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("limit"), "{error}");
    let error = parse_entry_points(
        b"[DEFAULT]\nbase = averylongmodule:main\n[console_scripts]\ncopy = %(base)s%(base)s\n",
        WheelScriptLimits {
            max_entrypoint_value_bytes: 30,
            ..WheelScriptLimits::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("limit"), "{error}");
}

#[test]
fn ini_continuations_crlf_comments_and_last_line_without_newline_are_supported() {
    let values = parsed(
        "; full comment\r\n[console_scripts]\r\n# comment\r\nrun = demo\r\n  : outer.call\r\nnext = demo:next",
    );
    assert_eq!(
        values.iter().map(WheelEntryPoint::name).collect::<Vec<_>>(),
        ["next", "run"]
    );
    assert_eq!(values[1].attributes(), ["outer", "call"]);
}

#[test]
fn duplicate_sections_and_duplicate_names_are_rejected_without_last_wins() {
    for text in [
        "[console_scripts]\nrun = demo:one\nrun = demo:two\n",
        "[console_scripts]\nrun = demo:one\n[console_scripts]\nother = demo:two\n",
        "[DEFAULT]\nrun = demo:one\nrun = demo:two\n[console_scripts]\nother = demo:other\n",
    ] {
        parse_rejected(text.as_bytes(), "duplicate");
    }
}

#[test]
fn malformed_ini_missing_sections_and_colon_only_delimiters_are_rejected() {
    for text in [
        "run = demo:main\n",
        "[console_scripts\nrun = demo:main\n",
        "[console_scripts]\nrun: demo:main\n",
        " orphan\n[console_scripts]\nrun=demo:main\n",
        "[console scripts]\nrun = demo:main\n",
        "[console_scripts]\nno delimiter\n",
    ] {
        parse_rejected(text.as_bytes(), "entry points");
    }
}

#[test]
fn non_utf8_and_nul_metadata_are_rejected_without_decoding_lossily() {
    for text in [
        b"[console_scripts]\nrun = demo:\xff\n".as_slice(),
        b"[console_scripts]\nrun\0 = demo:main\n",
    ] {
        parse_rejected(text, "entry points");
    }
}

#[test]
fn unsafe_command_names_cannot_escape_the_scripts_scheme() {
    for name in [
        "../outside",
        "/absolute",
        "a/b",
        "a\\b",
        "C:outside",
        ".",
        "..",
        "[hidden",
        "",
    ] {
        parse_rejected(
            format!("[console_scripts]\n{name} = demo:main\n").as_bytes(),
            "entry points",
        );
    }
}

#[test]
fn malformed_object_references_and_extras_are_not_interpolated_as_python_code() {
    for value in [
        "demo",
        "demo:",
        ":main",
        "demo..pkg:main",
        "1demo:main",
        "demo:main()",
        "demo:main;evil()",
        "demo:main [x",
        "demo:main [x,,y]",
        "demo:main [../x]",
        "demo:main ]",
        "demo:main extra",
    ] {
        parse_rejected(
            format!("[console_scripts]\nrun = {value}\n").as_bytes(),
            "entry points",
        );
    }
}

#[test]
fn empty_or_plugin_only_metadata_produces_no_wrappers() {
    for text in ["", "# comment\n", "[pytest11]\nplugin = module\n"] {
        assert!(parsed(text).is_empty());
    }
}

#[test]
fn parser_limits_count_all_declarations_including_ignored_groups_and_defaults() {
    for limits in [
        WheelScriptLimits {
            max_entrypoints_bytes: 1,
            ..WheelScriptLimits::default()
        },
        WheelScriptLimits {
            max_entrypoints: 1,
            ..WheelScriptLimits::default()
        },
        WheelScriptLimits {
            max_entrypoint_value_bytes: 1,
            ..WheelScriptLimits::default()
        },
    ] {
        let error = parse_entry_points(b"[DEFAULT]\nbase=demo:main\n[pytest11]\nplugin=module\n[console_scripts]\nrun=demo:main\n", limits).unwrap_err();
        assert!(error.to_string().contains("limit"), "{error}");
    }
}

#[test]
fn generated_wrapper_has_final_interpreter_digest_size_and_executable_mode() {
    let prepared = generate_entry_point(
        &entry("run", "demo", &["outer", "main"]),
        &target(),
        WheelScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(prepared.name(), "run");
    assert!(
        prepared
            .bytes()
            .starts_with(b"#!/opt/pakro/env/bin/python\n")
    );
    assert_eq!(prepared.size(), prepared.bytes().len() as u64);
    assert_eq!(
        prepared.sha256().as_slice(),
        Sha256::digest(prepared.bytes()).as_slice()
    );
    assert_eq!(prepared.mode(), 0o755);
}

#[test]
fn gui_and_console_wrappers_use_the_same_explicit_posix_interpreter() {
    let mut gui = entry("window", "demo", &["main"]);
    gui.kind = WheelEntryPointKind::Gui;
    for value in [entry("run", "demo", &["main"]), gui] {
        let prepared =
            generate_entry_point(&value, &target(), WheelScriptLimits::default()).unwrap();
        assert_eq!(prepared.name(), value.name());
        assert!(
            prepared
                .bytes()
                .starts_with(b"#!/opt/pakro/env/bin/python\n")
        );
        assert_eq!(prepared.mode(), 0o755);
    }
}

#[test]
fn short_and_long_shebang_boundaries_are_explicit_without_host_detection() {
    for (length, simple) in [(124, true), (125, false)] {
        let executable = PathBuf::from(format!("/p/{}", "x".repeat(length - 3)));
        let script_target = PythonScriptTarget {
            final_prefix: Path::new("/p"),
            interpreter: &executable,
        };
        let prepared = generate_entry_point(
            &entry("run", "demo", &["main"]),
            &script_target,
            WheelScriptLimits::default(),
        )
        .unwrap();
        assert_eq!(prepared.bytes().starts_with(b"#!/bin/sh\n"), !simple);
        assert!(!prepared.bytes().is_empty());
    }
}

#[test]
fn whitespace_and_quotes_in_final_interpreter_use_a_posix_trampoline() {
    for path in ["/p/space dir/python", "/p/a\tpython"] {
        let error = generate_entry_point(
            &entry("run", "demo", &["main"]),
            &PythonScriptTarget {
                final_prefix: Path::new("/p"),
                interpreter: Path::new(path),
            },
            WheelScriptLimits::default(),
        );
        if path.contains('\t') {
            assert!(error.unwrap_err().to_string().contains("target"));
        } else {
            assert!(error.unwrap().bytes().starts_with(b"#!/bin/sh\n"));
        }
    }
}

#[test]
fn invalid_final_target_paths_are_rejected_without_guessing_a_runtime() {
    for (prefix, executable) in [
        ("relative", "relative/bin/python"),
        ("/p", "python"),
        ("/p", "/other/python"),
        ("/p", "/p/../other/python"),
        ("/p", "/p/中文/python"),
        ("/p", "/p/a\npython"),
        ("/p", "/p/a\0python"),
    ] {
        let result = generate_entry_point(
            &entry("run", "demo", &["main"]),
            &PythonScriptTarget {
                final_prefix: Path::new(prefix),
                interpreter: Path::new(executable),
            },
            WheelScriptLimits::default(),
        );
        assert!(result.unwrap_err().to_string().contains("target"));
    }
}

#[test]
fn generator_revalidates_untrusted_entrypoint_values_without_emitting_code() {
    for value in [
        entry("../outside", "demo", &["main"]),
        entry("run", "demo;evil()", &["main"]),
        entry("run", "demo", &["main()"]),
        entry("run", "demo", &[]),
    ] {
        let error =
            generate_entry_point(&value, &target(), WheelScriptLimits::default()).unwrap_err();
        assert!(error.to_string().contains("entry point"), "{error}");
    }
}

#[test]
fn wrapper_limit_is_checked_before_returning_a_partial_script() {
    let error = generate_entry_point(
        &entry("run", "demo", &["main"]),
        &target(),
        WheelScriptLimits {
            max_wrapper_bytes: 8,
            ..WheelScriptLimits::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("limit"), "{error}");
}

#[test]
fn canonical_eight_byte_python_prefix_is_rewritten_including_old_suffixes() {
    for first in [
        b"#!python\n".as_slice(),
        b"#!pythonw\n",
        b"#!python3\n",
        b"#!python-custom -u\n",
        b"#!python -u\n",
    ] {
        let mut input = first.to_vec();
        input.extend_from_slice(b"print('unchanged body')\n");
        let (output, stats) = rewritten(&input, 0o644);
        assert_eq!(
            output,
            b"#!/opt/pakro/env/bin/python\nprint('unchanged body')\n"
        );
        assert_stats(
            &input,
            &output,
            &stats,
            WheelScriptOperation::RewritePythonShebang,
            0o755,
        );
    }
}

#[test]
fn crlf_lf_and_bare_cr_first_lines_preserve_body_bytes() {
    for ending in [b"\n".as_slice(), b"\r\n", b"\r"] {
        let mut input = b"#!python".to_vec();
        input.extend_from_slice(ending);
        input.extend_from_slice(b"# coding: latin-1\r\nprint('\xe9')\r\n");
        let (output, stats) = rewritten(&input, 0o640);
        assert_eq!(
            output,
            b"#!/opt/pakro/env/bin/python\n# coding: latin-1\r\nprint('\xe9')\r\n"
        );
        assert_stats(
            &input,
            &output,
            &stats,
            WheelScriptOperation::RewritePythonShebang,
            0o751,
        );
    }
}

#[test]
fn no_newline_python_marker_and_empty_scripts_have_exact_statistics() {
    let (output, stats) = rewritten(b"#!python", 0o444);
    assert_eq!(output, b"#!/opt/pakro/env/bin/python\n");
    assert_stats(
        b"#!python",
        &output,
        &stats,
        WheelScriptOperation::RewritePythonShebang,
        0o555,
    );
    let (output, stats) = rewritten(b"", 0o600);
    assert!(output.is_empty());
    assert_stats(b"", &output, &stats, WheelScriptOperation::Copy, 0o600);
}

#[test]
fn nonmatching_shebangs_and_binary_scripts_are_copied_byte_for_byte() {
    for input in [
        b"#!/usr/bin/python\nprint('x')\n".as_slice(),
        b"#!pYthon\nbody",
        b"#!pytho\nbody",
        b"\x7fELF\0\xff\nbody",
        b"plain resource\r\n",
    ] {
        let (output, stats) = rewritten(input, 0o100640);
        assert_eq!(output, input);
        assert_stats(input, &output, &stats, WheelScriptOperation::Copy, 0o640);
    }
}

#[test]
fn executable_bits_are_added_only_for_rewritten_python_and_special_bits_are_dropped() {
    for (input, expected) in [
        (b"#!python\npass\n".as_slice(), 0o751),
        (b"plain".as_slice(), 0o640),
    ] {
        let (output, stats) = rewritten(input, 0o107640);
        assert_eq!(stats.mode(), expected);
        assert_eq!(stats.sha256().as_slice(), Sha256::digest(output).as_slice());
    }
}

#[test]
fn huge_nonpython_first_line_streams_but_python_first_line_is_bounded() {
    let input = vec![b'x'; 200_000];
    let mut output = Vec::new();
    let limits = WheelScriptLimits {
        max_first_line_bytes: 16,
        ..WheelScriptLimits::default()
    };
    let stats =
        rewrite_python_script(Cursor::new(&input), &mut output, &target(), 0o644, limits).unwrap();
    assert_eq!(output, input);
    assert_stats(&input, &output, &stats, WheelScriptOperation::Copy, 0o644);
    let mut input = b"#!python ".to_vec();
    input.extend_from_slice(&[b'x'; 32]);
    let error = rewrite_python_script(
        Cursor::new(input),
        &mut Vec::new(),
        &target(),
        0o644,
        limits,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("first line") && error.to_string().contains("limit"),
        "{error}"
    );
}

#[test]
fn raw_and_final_stream_limits_are_both_enforced() {
    for (input, limits) in [
        (
            b"plain long".as_slice(),
            WheelScriptLimits {
                max_input_bytes: 2,
                ..WheelScriptLimits::default()
            },
        ),
        (
            b"#!python\npass\n".as_slice(),
            WheelScriptLimits {
                max_output_bytes: 8,
                ..WheelScriptLimits::default()
            },
        ),
    ] {
        let error = rewrite_python_script(
            Cursor::new(input),
            &mut Vec::new(),
            &target(),
            0o644,
            limits,
        )
        .unwrap_err();
        assert!(error.to_string().contains("limit"), "{error}");
    }
}

struct BrokenRead;
impl Read for BrokenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("fixture read"))
    }
}
struct BrokenWrite;
impl Write for BrokenWrite {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("fixture write"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn source_read_errors_and_output_write_errors_are_not_treated_as_eof() {
    let error = rewrite_python_script(
        BrokenRead,
        &mut Vec::new(),
        &target(),
        0o644,
        WheelScriptLimits::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("read"), "{error}");
    let error = rewrite_python_script(
        Cursor::new(b"plain"),
        &mut BrokenWrite,
        &target(),
        0o644,
        WheelScriptLimits::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("write"), "{error}");
}

struct ShortRead<'a> {
    bytes: &'a [u8],
}
impl Read for ShortRead<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(self.bytes.len()).min(1);
        output[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        Ok(count)
    }
}
#[test]
fn prefix_and_crlf_detection_work_across_single_byte_reads() {
    let input = b"#!pythonw\r\nprint('body')\r\n";
    let mut output = Vec::new();
    let stats = rewrite_python_script(
        ShortRead { bytes: input },
        &mut output,
        &target(),
        0o644,
        WheelScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(output, b"#!/opt/pakro/env/bin/python\nprint('body')\r\n");
    assert_stats(
        input,
        &output,
        &stats,
        WheelScriptOperation::RewritePythonShebang,
        0o755,
    );
}

#[test]
fn zero_limits_are_invalid_in_all_preparation_entrypoints() {
    let limits = WheelScriptLimits::default();
    for limits in [
        WheelScriptLimits {
            max_entrypoints_bytes: 0,
            ..limits
        },
        WheelScriptLimits {
            max_entrypoints: 0,
            ..limits
        },
        WheelScriptLimits {
            max_entrypoint_value_bytes: 0,
            ..limits
        },
        WheelScriptLimits {
            max_wrapper_bytes: 0,
            ..limits
        },
        WheelScriptLimits {
            max_first_line_bytes: 0,
            ..limits
        },
        WheelScriptLimits {
            max_input_bytes: 0,
            ..limits
        },
        WheelScriptLimits {
            max_output_bytes: 0,
            ..limits
        },
    ] {
        assert!(
            parse_entry_points(b"", limits)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
        assert!(
            generate_entry_point(&entry("run", "demo", &["main"]), &target(), limits)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
        assert!(
            rewrite_python_script(Cursor::new(b""), &mut Vec::new(), &target(), 0o644, limits)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
    }
}

#[test]
fn debug_and_errors_do_not_disclose_entrypoint_target_or_generated_source() {
    let value = entry("run", "private_module_token", &["private_attribute_token"]);
    let prepared = generate_entry_point(&value, &target(), WheelScriptLimits::default()).unwrap();
    assert!(!prepared.bytes().is_empty());
    let debug = format!("{value:?} {prepared:?}");
    for secret in ["private_module_token", "private_attribute_token"] {
        assert!(!debug.contains(secret), "{debug}");
    }
    let bad = entry(
        "run",
        "private_url_token @ https://user:password@private.invalid",
        &["main"],
    );
    let error = generate_entry_point(&bad, &target(), WheelScriptLimits::default()).unwrap_err();
    for secret in ["password", "private.invalid", "private_url_token"] {
        assert!(!error.to_string().contains(secret));
    }
}

fn executable_file(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn posix_trampoline_preserves_the_explicit_interpreter_path_and_every_argument() {
    let root = tempfile::Builder::new()
        .prefix("pakro-script-")
        .tempdir_in("/tmp")
        .unwrap();
    for label in [
        "short trampoline".to_owned(),
        "space and 'quote'".to_owned(),
        "long".repeat(35),
    ] {
        let prefix = root.path().join(&label);
        let interpreter = prefix.join("bin/python");
        executable_file(&interpreter, b"#!/bin/sh\nprintf '%s\\000' \"$0\" \"$@\"\n");
        let script_target = PythonScriptTarget {
            final_prefix: &prefix,
            interpreter: &interpreter,
        };
        let script = generate_entry_point(
            &entry("run", "demo", &["main"]),
            &script_target,
            WheelScriptLimits::default(),
        )
        .unwrap();
        // The fake is itself a shell script. XNU deliberately does not allow
        // nested shebang interpreters, so this fixture exercises sh exec only.
        // The explicit real-Python tests below cover native direct shebangs.
        assert!(script.bytes().starts_with(b"#!/bin/sh\n"));
        assert_eq!(script.mode(), 0o755);
        let path = prefix.join("bin/run");
        executable_file(&path, script.bytes());
        let output = Command::new(&path)
            .args(["two words", "quote'\"", "中文", "line\nbreak", ""])
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let actual: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
        let expected: Vec<Vec<u8>> = [
            interpreter.as_os_str().as_encoded_bytes(),
            path.as_os_str().as_encoded_bytes(),
            b"two words",
            b"quote'\"",
            "中文".as_bytes(),
            b"line\nbreak",
            b"",
            b"",
        ]
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect();
        assert_eq!(
            actual,
            expected.iter().map(Vec::as_slice).collect::<Vec<_>>()
        );
    }
}

#[test]
#[ignore = "requires an explicit existing PAKRO_TEST_PYTHON; proves wrappers, not prefix support"]
fn live_wrappers_call_selected_python_with_nested_unicode_targets_and_exit_semantics() {
    let selected = PathBuf::from(
        std::env::var_os("PAKRO_TEST_PYTHON").expect("explicit selected Python required"),
    )
    .canonicalize()
    .unwrap();
    assert!(selected.is_absolute());
    let root = tempfile::Builder::new()
        .prefix("pakro-script-")
        .tempdir_in("/tmp")
        .unwrap();
    let modules = root.path().join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::write(modules.join("demo_cli.py"), b"import json, sys\ndef main():\n print(json.dumps({'executable': sys.executable, 'args': sys.argv[1:]}, ensure_ascii=True))\n return 7\nclass Outer(object):\n call = staticmethod(main)\n").unwrap();
    std::fs::write(modules.join("模块.py"), "import json, sys\ndef 运行():\n print(json.dumps({'executable': sys.executable, 'args': sys.argv[1:]}, ensure_ascii=True))\n return None\n").unwrap();
    // The selected PyPy reports UTF-8 in C/UTF8-mode, yet its Unicode module
    // importer needs a real UTF-8 locale. Observe the fixture prerequisite
    // explicitly instead of weakening Unicode module/attribute assertions.
    let prerequisite = Command::new(&selected)
        .args([
            "-B",
            "-c",
            "import sys; __import__('模块'); assert callable(getattr(sys.modules['模块'], '运行'))",
        ])
        .env_clear()
        .env("LC_ALL", "C.UTF-8")
        .env("PYTHONPATH", &modules)
        .output()
        .unwrap();
    assert!(
        prerequisite.status.success(),
        "live Unicode fixture requires functional selected-runtime import under C.UTF-8: {:?}",
        prerequisite.stderr
    );
    for (index, module, attrs, kind, exit) in [
        (
            0,
            "demo_cli",
            vec!["Outer", "call"],
            WheelEntryPointKind::Console,
            7,
        ),
        (1, "模块", vec!["运行"], WheelEntryPointKind::Gui, 0),
    ] {
        for complex in [false, true] {
            let label = if complex {
                format!("live env '{index}'")
            } else {
                format!("direct{index}")
            };
            let prefix = root.path().join(label);
            let interpreter = prefix.join("bin/python");
            std::fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
            symlink(&selected, &interpreter).unwrap();
            let mut value = entry("command", module, &attrs);
            value.kind = kind;
            let prepared = generate_entry_point(
                &value,
                &PythonScriptTarget {
                    final_prefix: &prefix,
                    interpreter: &interpreter,
                },
                WheelScriptLimits::default(),
            )
            .unwrap();
            assert_eq!(prepared.bytes().starts_with(b"#!/bin/sh\n"), complex);
            assert_eq!(prepared.mode(), 0o755);
            let command = prefix.join("bin/command");
            executable_file(&command, prepared.bytes());
            let output = Command::new(command)
                .args(["two words", "中文", "quote'\""])
                .env_clear()
                .env("LC_ALL", "C.UTF-8")
                .env("PYTHONPATH", &modules)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(exit), "{:?}", output.stderr);
            let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                payload["args"],
                serde_json::json!(["two words", "中文", "quote'\""])
            );
            assert_eq!(
                PathBuf::from(payload["executable"].as_str().unwrap())
                    .canonicalize()
                    .unwrap(),
                selected
            );
        }
    }
}

fn interpolation_chain(aliases: usize) -> String {
    let mut text = String::from("[DEFAULT]\n");
    for index in 0..aliases {
        if index + 1 == aliases {
            text.push_str(&format!("key{index} = demo:main\n"));
        } else {
            text.push_str(&format!("key{index} = %(key{})s\n", index + 1));
        }
    }
    text.push_str("[console_scripts]\ncopy = %(key0)s\n");
    text
}

#[test]
fn interpolation_accepts_ten_levels_and_rejects_the_eleventh() {
    // Frozen independently against stdlib case-sensitive ConfigParser.
    let values = parsed(&interpolation_chain(10));
    assert_eq!(values.len(), 11);
    assert!(
        values
            .iter()
            .all(|value| value.module() == "demo" && value.attributes() == ["main"])
    );
    parse_rejected(interpolation_chain(11).as_bytes(), "interpolation");
}

struct ShortWrite(Vec<u8>);
impl Write for ShortWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len().min(1);
        self.0.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn partial_output_writes_are_completed_before_returning_statistics() {
    let input = b"#!python\nprint('body')\n";
    let mut output = ShortWrite(Vec::new());
    let stats = rewrite_python_script(
        Cursor::new(input),
        &mut output,
        &target(),
        0o644,
        WheelScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(output.0, b"#!/opt/pakro/env/bin/python\nprint('body')\n");
    assert_stats(
        input,
        &output.0,
        &stats,
        WheelScriptOperation::RewritePythonShebang,
        0o755,
    );
}

#[test]
fn a_cookie_on_the_original_shebang_is_preserved_for_direct_and_complex_launchers() {
    let input = b"#!python # coding: latin-1\nimport sys\nsys.stdout.write(str(ord('\xe9')))\n";
    for executable in ["/p/bin/python", "/p/space dir/bin/python"] {
        let target = PythonScriptTarget {
            final_prefix: Path::new("/p"),
            interpreter: Path::new(executable),
        };
        let mut output = Vec::new();
        let stats = rewrite_python_script(
            Cursor::new(input),
            &mut output,
            &target,
            0o644,
            WheelScriptLimits::default(),
        )
        .unwrap();
        let second = output.split(|byte| *byte == b'\n').nth(1).unwrap();
        assert!(
            second
                .windows(b"coding: latin-1".len())
                .any(|part| part == b"coding: latin-1")
        );
        assert!(output.ends_with(b"import sys\nsys.stdout.write(str(ord('\xe9')))\n"));
        assert_stats(
            input,
            &output,
            &stats,
            WheelScriptOperation::RewritePythonShebang,
            0o755,
        );
    }
}

#[test]
fn a_second_line_cookie_stays_in_the_first_two_lines_of_a_trampoline() {
    let input = b"#!python\n# coding: latin-1\nprint('\xe9')\n";
    let mut output = Vec::new();
    let target = PythonScriptTarget {
        final_prefix: Path::new("/p"),
        interpreter: Path::new("/p/space dir/bin/python"),
    };
    let stats = rewrite_python_script(
        Cursor::new(input),
        &mut output,
        &target,
        0o644,
        WheelScriptLimits::default(),
    )
    .unwrap();
    assert!(output.starts_with(b"#!/bin/sh\n"));
    assert!(
        output
            .split(|byte| *byte == b'\n')
            .nth(1)
            .unwrap()
            .windows(b"coding: latin-1".len())
            .any(|part| part == b"coding: latin-1")
    );
    assert!(output.ends_with(b"# coding: latin-1\nprint('\xe9')\n"));
    assert_stats(
        input,
        &output,
        &stats,
        WheelScriptOperation::RewritePythonShebang,
        0o755,
    );
}

#[test]
fn an_original_first_line_cookie_takes_precedence_over_the_second_line() {
    let input = b"#!python # coding: latin-1\n# coding: ascii\nprint('\xe9')\n";
    let mut output = Vec::new();
    let target = PythonScriptTarget {
        final_prefix: Path::new("/p"),
        interpreter: Path::new("/p/space dir/bin/python"),
    };
    rewrite_python_script(
        Cursor::new(input),
        &mut output,
        &target,
        0o644,
        WheelScriptLimits::default(),
    )
    .unwrap();
    let second = output.split(|byte| *byte == b'\n').nth(1).unwrap();
    assert!(
        second
            .windows(b"coding: latin-1".len())
            .any(|part| part == b"coding: latin-1")
    );
    assert!(output.ends_with(b"# coding: ascii\nprint('\xe9')\n"));
}

#[test]
#[ignore = "requires explicit PAKRO_TEST_PYTHON; validates script encoding, not full prefix support"]
fn live_rewritten_latin1_scripts_preserve_first_and_second_line_cookies() {
    let selected = PathBuf::from(
        std::env::var_os("PAKRO_TEST_PYTHON").expect("explicit selected Python required"),
    )
    .canonicalize()
    .unwrap();
    let root = tempfile::Builder::new()
        .prefix("pakro-script-live-")
        .tempdir_in("/tmp")
        .unwrap();
    for (index, input) in [
        b"#!python # coding: latin-1\nimport sys\nsys.stdout.write(str(ord('\xe9')) + '\\n')\n".as_slice(),
        b"#!python\n# coding: latin-1\nimport sys\nsys.stdout.write(str(ord('\xe9')) + '\\n')\n",
        b"#!python # coding: latin-1\n# coding: ascii\nimport sys\nsys.stdout.write(str(ord('\xe9')) + '\\n')\n",
    ].into_iter().enumerate() {
        for complex in [false, true] {
            let label = if complex { format!("with space '{index}'") } else { format!("direct{index}") };
            let prefix = root.path().join(label); let interpreter = prefix.join("bin/python");
            std::fs::create_dir_all(interpreter.parent().unwrap()).unwrap(); symlink(&selected, &interpreter).unwrap();
            let mut output = Vec::new();
            let stats = rewrite_python_script(Cursor::new(input), &mut output, &PythonScriptTarget { final_prefix: &prefix, interpreter: &interpreter }, 0o644, WheelScriptLimits::default()).unwrap();
            assert_eq!(stats.operation(), WheelScriptOperation::RewritePythonShebang);
            assert_eq!(output.starts_with(b"#!/bin/sh\n"), complex);
            let path = prefix.join("bin/script"); executable_file(&path, &output);
            let result = Command::new(path).env_clear().env("LC_ALL", "C").output().unwrap();
            assert!(result.status.success(), "{:?}", result.stderr);
            assert_eq!(result.stdout, b"233\n");
        }
    }
}

#[test]
fn encoding_detection_is_bounded_and_unknown_codec_names_are_preserved() {
    let script_target = PythonScriptTarget {
        final_prefix: Path::new("/p"),
        interpreter: Path::new("/p/space dir/python"),
    };
    let input = b"#!python\n# coding: name_that_exceeds_the_capture_budget\npass\n";
    let error = rewrite_python_script(
        Cursor::new(input),
        &mut Vec::new(),
        &script_target,
        0o644,
        WheelScriptLimits {
            max_first_line_bytes: 16,
            ..WheelScriptLimits::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("limit"), "{error}");
    let input = b"#!python # coding: pakro-unknown-codec\npass\n";
    let mut output = Vec::new();
    let stats = rewrite_python_script(
        Cursor::new(input),
        &mut output,
        &script_target,
        0o644,
        WheelScriptLimits::default(),
    )
    .unwrap();
    assert!(
        output
            .split(|byte| *byte == b'\n')
            .nth(1)
            .unwrap()
            .windows(b"pakro-unknown-codec".len())
            .any(|part| part == b"pakro-unknown-codec")
    );
    assert_stats(
        input,
        &output,
        &stats,
        WheelScriptOperation::RewritePythonShebang,
        0o755,
    );
}

struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("invalid target must fail before reading script")
    }
}
struct ForbiddenWrite;
impl Write for ForbiddenWrite {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        panic!("invalid target must fail before writing script")
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("invalid target must fail before flushing script")
    }
}
#[test]
fn rewriting_validates_the_final_target_before_reading_or_emitting_any_content() {
    let error = rewrite_python_script(
        ForbiddenRead,
        &mut ForbiddenWrite,
        &PythonScriptTarget {
            final_prefix: Path::new("/p"),
            interpreter: Path::new("relative"),
        },
        0o644,
        WheelScriptLimits::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("target"), "{error}");
}

#[test]
fn hash_in_a_short_interpreter_path_requires_a_posix_trampoline() {
    // XNU treats '#' as the end of a shebang line, even without whitespace.
    // Keep this ASCII path usable without platform detection or path rejection.
    let prepared = generate_entry_point(
        &entry("run", "demo", &["main"]),
        &PythonScriptTarget {
            final_prefix: Path::new("/p"),
            interpreter: Path::new("/p/hash#dir/python"),
        },
        WheelScriptLimits::default(),
    )
    .unwrap();
    assert!(prepared.bytes().starts_with(b"#!/bin/sh\n"));
}
