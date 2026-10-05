// These tests redirect a private descriptor (never fd 2) so the test harness output is
// untouched.
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;

fn scratch_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "lab-tui-stderr-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_fd(fd: std::os::unix::io::RawFd, text: &str) {
    let written = unsafe { libc::write(fd, text.as_ptr().cast(), text.len()) };
    assert_eq!(written, text.len() as isize);
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn redirect_is_restored_and_happens_at_most_once() {
    let dir = scratch_dir("slot");
    let original_path = dir.join("terminal.txt");
    let log_path = dir.join("lab.log");
    let original = fs::File::create(&original_path).unwrap();
    let target = original.as_raw_fd();

    let mut slot = StderrRedirectSlot::new();
    let mut opened = 0;
    assert!(slot.ensure(target, || {
        opened += 1;
        open_lab_log_file(&log_path, true)
    }));
    assert!(slot.is_active());
    write_fd(target, "to-log;");
    // A second request keeps the first redirect and does not reopen the log.
    assert!(slot.ensure(target, || {
        opened += 1;
        open_lab_log_file(&log_path, true)
    }));
    assert_eq!(opened, 1);
    write_fd(target, "still-log;");

    slot.restore();
    assert!(!slot.is_active());
    write_fd(target, "back-to-terminal;");
    slot.restore(); // idempotent

    assert_eq!(fs::read_to_string(&log_path).unwrap(), "to-log;still-log;");
    assert_eq!(
        fs::read_to_string(&original_path).unwrap(),
        "back-to-terminal;"
    );

    // After a restore the slot can redirect again (next TUI session).
    assert!(slot.ensure(target, || open_lab_log_file(&log_path, true)));
    write_fd(target, "again;");
    slot.restore();
    assert_eq!(
        fs::read_to_string(&log_path).unwrap(),
        "to-log;still-log;again;"
    );
    drop(original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn failed_open_leaves_descriptor_untouched() {
    let dir = scratch_dir("fail");
    let original_path = dir.join("terminal.txt");
    let original = fs::File::create(&original_path).unwrap();
    let mut slot = StderrRedirectSlot::new();
    assert!(!slot.ensure(original.as_raw_fd(), || Err(std::io::Error::other("nope"))));
    assert!(!slot.is_active());
    write_fd(original.as_raw_fd(), "ok");
    assert_eq!(fs::read_to_string(&original_path).unwrap(), "ok");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn dropping_a_redirect_restores_the_descriptor() {
    let dir = scratch_dir("drop");
    let original_path = dir.join("terminal.txt");
    let log_path = dir.join("lab.log");
    let original = fs::File::create(&original_path).unwrap();
    {
        let log = open_lab_log_file(&log_path, true).unwrap();
        let _redirect = FdRedirect::redirect(original.as_raw_fd(), &log).unwrap();
        write_fd(original.as_raw_fd(), "log");
    }
    write_fd(original.as_raw_fd(), "terminal");
    assert_eq!(fs::read_to_string(&log_path).unwrap(), "log");
    assert_eq!(fs::read_to_string(&original_path).unwrap(), "terminal");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn default_log_lives_in_library_logs_and_is_private() {
    assert_eq!(
        default_lab_log_path(Some(std::ffi::OsStr::new("/Users/someone"))),
        Some(PathBuf::from("/Users/someone/Library/Logs/lab/lab.log"))
    );
    assert_eq!(default_lab_log_path(Some(std::ffi::OsStr::new(""))), None);
    assert_eq!(default_lab_log_path(None), None);

    let dir = scratch_dir("perm");
    let path = default_lab_log_path(Some(dir.as_os_str())).unwrap();
    drop(open_lab_log_file(&path, true).unwrap());
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);

    // An existing world-readable default log is tightened.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    drop(open_lab_log_file(&path, true).unwrap());
    assert_eq!(mode(&path), 0o600);

    // A LAB_LOG override is created private but an existing file's mode is left alone.
    let custom = dir.join("custom.log");
    drop(open_lab_log_file(&custom, false).unwrap());
    assert_eq!(mode(&custom), 0o600);
    fs::set_permissions(&custom, fs::Permissions::from_mode(0o640)).unwrap();
    drop(open_lab_log_file(&custom, false).unwrap());
    assert_eq!(mode(&custom), 0o640);
    let _ = fs::remove_dir_all(&dir);
}
