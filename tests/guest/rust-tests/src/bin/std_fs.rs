use std::fs::{self, File};
use std::mem::ManuallyDrop;
use std::os::toyos::fs::symlink;
use std::os::toyos::io::{AsRawFd, FromRawFd};
use std::process::{Command, Stdio};

fn main() {
    let entries: Vec<_> = fs::read_dir("/system/bin")
        .expect("should be able to read /system/bin")
        .filter_map(|e| e.ok())
        .collect();
    assert!(!entries.is_empty(), "/system/bin should not be empty");

    let self_exists = std::path::Path::new("/system/bin/test_rs_std_fs").exists();
    assert!(self_exists, "our own binary should exist in /system/bin");

    let data = fs::read("/system/bin/test_rs_std_fs")
        .expect("should be able to read our own binary");
    assert!(!data.is_empty(), "binary should not be empty");

    // Non-existent file should return NotFound
    let err = fs::read("/system/bin/nonexistent").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

    file_types();
    create_new_is_exclusive_on_tmp();
    the_empty_path_names_nothing();

    println!("all fs tests passed");
}

/// The empty path names nothing, wherever the working directory is served:
/// an open, a stat and a removal of it are `NotFound`, and the empty working
/// directory a removal would take is still there.
fn the_empty_path_names_nothing() {
    for cwd in ["/tmp", "/home"] {
        let dir = format!("{cwd}/std_fs_empty_path");
        let _ = fs::remove_dir(&dir);
        fs::create_dir(&dir).expect("make the working directory");
        std::env::set_current_dir(&dir).expect("enter it");
        let kind = |r: std::io::Result<()>| r.map_err(|e| e.kind());
        assert_eq!(kind(fs::File::open("").map(drop)), Err(std::io::ErrorKind::NotFound), "open of \"\" in {cwd}");
        assert_eq!(kind(fs::metadata("").map(drop)), Err(std::io::ErrorKind::NotFound), "stat of \"\" in {cwd}");
        assert_eq!(kind(fs::remove_dir("")), Err(std::io::ErrorKind::NotFound), "remove_dir of \"\" in {cwd}");
        std::env::set_current_dir("/").expect("leave it");
        assert!(fs::metadata(&dir).is_ok_and(|m| m.is_dir()), "{dir} went with a removal of \"\"");
        fs::remove_dir(&dir).expect("remove the directory");
    }
}

/// `create_new` on the kernel's `/tmp` makes a file once and refuses the name
/// after, as a lock file needs.
fn create_new_is_exclusive_on_tmp() {
    let path = "/tmp/std_fs_create_new";
    let _ = fs::remove_file(path);
    let new = || fs::OpenOptions::new().write(true).create_new(true).open(path);
    new().expect("the first create_new makes the file");
    let again = new().expect_err("a second create_new of one path opened the file the first made");
    assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
    fs::remove_file(path).expect("remove the file");

    // A dangling link is a name that is there, and its target stays absent.
    let (link, target) = ("/tmp/std_fs_create_new_link", "/tmp/std_fs_create_new_target");
    let _ = fs::remove_file(link);
    let _ = fs::remove_file(target);
    symlink(target, link).expect("make a dangling link");
    let through = fs::OpenOptions::new().write(true).create_new(true).open(link);
    assert_eq!(through.map(drop).map_err(|e| e.kind()), Err(std::io::ErrorKind::AlreadyExists), "create_new through a dangling link");
    assert!(fs::symlink_metadata(target).is_err(), "create_new made the dangling link's target");
    fs::remove_file(link).expect("remove the link");

    // So is a directory.
    let dir = "/tmp/std_fs_create_new_dir";
    let _ = fs::remove_dir(dir);
    fs::create_dir(dir).expect("make a directory on /tmp");
    let over = fs::OpenOptions::new().write(true).create_new(true).open(dir);
    assert_eq!(over.map(drop).map_err(|e| e.kind()), Err(std::io::ErrorKind::AlreadyExists), "create_new over a directory");
    fs::remove_dir(dir).expect("remove the directory");
}

/// What `Metadata::file_type` answers for each kind of thing this machine has.
///
/// Every arm here is an exclusion as much as an assertion: a `FileType` that
/// answered yes to two questions at once is the shape all three defects took.
fn file_types() {
    let ty = fs::metadata("/system/bin/test_rs_std_fs").expect("stat our own binary").file_type();
    assert!(ty.is_file() && !ty.is_dir() && !ty.is_symlink(), "a regular file typed as {ty:?}");

    let ty = fs::metadata("/system/bin").expect("stat /system/bin").file_type();
    assert!(ty.is_dir() && !ty.is_file() && !ty.is_symlink(), "a directory typed as {ty:?}");

    pipe_is_not_a_directory();
    symlink_is_not_also_a_file();

    println!("  file types: ok");
}

/// ToyOS has no directory file type, so `stat` reported a directory's as `Pipe`
/// and `is_dir()` compared against it — which made an `fstat` of a real pipe
/// answer yes.
fn pipe_is_not_a_directory() {
    let mut child = Command::new("/system/bin/echo")
        .arg("down the pipe")
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn echo");
    let pipe = child.stdout.take().expect("echo was given a stdout pipe");

    // The pipe still owns the descriptor; this only borrows it to reach `fstat`.
    let borrowed = ManuallyDrop::new(unsafe { File::from_raw_fd(pipe.as_raw_fd()) });
    let ty = borrowed.metadata().expect("fstat a pipe").file_type();
    assert!(!ty.is_dir(), "an fstat of a pipe answered is_dir()");
    assert!(!ty.is_file(), "an fstat of a pipe answered is_file()");
    assert!(!ty.is_symlink(), "an fstat of a pipe answered is_symlink()");

    drop(pipe);
    child.wait().expect("echo never exited");
}

/// `lstat` reported a symlink as a file *and* a symlink, which no other
/// platform does.
fn symlink_is_not_also_a_file() {
    let target = "/tmp/std_fs_symlink_target";
    let link = "/tmp/std_fs_symlink";
    fs::write(target, b"pointed at\n").expect("write the symlink's target");
    let _ = fs::remove_file(link);
    symlink(target, link).expect("create a symlink on /tmp");

    let ty = fs::symlink_metadata(link).expect("lstat the symlink").file_type();
    assert!(ty.is_symlink(), "lstat of a symlink typed it as {ty:?}");
    assert!(!ty.is_file(), "lstat of a symlink also answered is_file()");
    assert!(!ty.is_dir(), "lstat of a symlink also answered is_dir()");
}
