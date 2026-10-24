//! Exercise the real package owners with the shell's small descriptor limit.
//! Only the isolated helper process changes its resource limits.

use super::{prepare_layout, wheel};
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};
use crate::libraries::package::lease::acquire_layout;
use nix::libc;
use std::fs::{self, File};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "libraries::package::layouts::tests::file_descriptors::s4_file_descriptor_layout_process_helper";
const CHILD_ROOT: &str = "PAKRO_S4_FILE_DESCRIPTOR_LAYOUT_ROOT";
const PACKAGE_COUNT: usize = 64;
const SUCCESS: &str = "FILE_DESCRIPTOR_LAYOUT_64_AND_RELEASE_PASSED";

fn limits() -> (libc::rlim_t, libc::rlim_t) {
    let mut value = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut value) },
        0,
        "reading descriptor limits failed: {}",
        std::io::Error::last_os_error()
    );
    (value.rlim_cur, value.rlim_max)
}

fn open_descriptors() -> usize {
    // The directory's own descriptor is included in both observations.
    fs::read_dir("/dev/fd").unwrap().count()
}

#[test]
fn s4_file_descriptor_capacity_allows_sixty_four_real_package_trees() {
    let before = limits();
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let output_path = root.join("child-output.txt");
    let output = File::create(&output_path).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            CHILD,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ROOT, &root)
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated descriptor-layout helper exceeded 120 seconds");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(limits(), before, "the parallel test process limits changed");
    let mut output = String::new();
    File::open(output_path)
        .unwrap()
        .take(1024 * 1024)
        .read_to_string(&mut output)
        .unwrap();
    assert!(
        status.success(),
        "isolated descriptor-layout helper failed: {status}\n{output}"
    );
    assert!(
        output.contains(SUCCESS),
        "the exact helper did not finish its assertions:\n{output}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "isolated subprocess helper; invoked by the descriptor-capacity parent"]
async fn s4_file_descriptor_layout_process_helper() {
    let root = std::path::PathBuf::from(
        std::env::var_os(CHILD_ROOT).expect("the parent must supply an owned temporary root"),
    );
    assert!(root.is_absolute() && root.is_dir());
    let (_, inherited_hard) = limits();
    assert!(
        inherited_hard >= 4096,
        "descriptor-layout fixture prerequisite failed: host hard limit is {inherited_hard}, requires at least 4096"
    );
    let constrained = libc::rlimit {
        rlim_cur: 256,
        rlim_max: 4096,
    };
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &constrained) },
        0,
        "setting the child-only descriptor limits failed: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(limits(), (256, 4096));
    pakro_util::process_limits::ensure_file_descriptor_capacity()
        .expect("initializing the child process descriptor capacity failed");
    let capacity = limits();
    assert_eq!(capacity.1, 4096, "capacity setup changed the hard limit");
    assert!(capacity.0 >= 256, "capacity setup decreased the soft limit");

    let context = operation_context(&root, true, Duration::from_secs(110));
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    let baseline = open_descriptors();
    let sources = (0..PACKAGE_COUNT)
        .map(|index| {
            let name = format!("fdlimitpkg{index:03}");
            let path = format!("{name}.py");
            wheel(
                &context,
                &language,
                &name,
                [index as u8 + 1; 32],
                vec![],
                vec![(path.as_str(), b"VALUE = 1\n")],
                None,
            )
        })
        .collect();
    eprintln!(
        "descriptor-layout sources prepared: baseline={baseline}, soft={}, hard={}",
        capacity.0, capacity.1
    );
    let layout = prepare_layout(&context, &language, [241; 32], sources)
        .expect("64 real wheel trees must reach canonical layout reacquisition");
    assert_eq!(layout.packages().len(), PACKAGE_COUNT);
    layout.check_binding().unwrap();
    let canonical = acquire_layout(&context, layout.key())
        .unwrap()
        .expect("the returned layout must be canonically published");
    assert_eq!(canonical.packages().len(), PACKAGE_COUNT);
    assert_eq!(canonical.key(), layout.key());
    canonical.check_binding().unwrap();
    assert_eq!(
        capacity,
        (4096, 4096),
        "the child soft limit was not raised to the existing hard limit"
    );
    let held = open_descriptors();
    assert!(
        held >= baseline + PACKAGE_COUNT,
        "the real independent package owners did not retain descriptors: baseline={baseline}, held={held}"
    );
    drop(canonical);
    drop(layout);
    let released = open_descriptors();
    assert!(
        released <= baseline + 4,
        "package and layout owners leaked descriptors after drop: baseline={baseline}, held={held}, released={released}"
    );
    language.check_binding().unwrap();
    println!("{SUCCESS}: baseline={baseline}, held={held}, released={released}");
}
