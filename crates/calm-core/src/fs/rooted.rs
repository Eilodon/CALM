//! `RootedFilesystem` -- CCK-05
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//!
//! `path_policy::resolve_within_root` (this crate's existing containment
//! check) is a TEXTUAL guarantee: canonicalize the candidate, canonicalize
//! the root, compare prefixes. That proves where a path resolves to AT THE
//! MOMENT OF THE CHECK -- it says nothing about what happens a moment
//! later, when the actual open/write runs against the same path as a
//! freshly re-resolved string. Between those two steps, nothing stops a
//! path component from being swapped for a symlink pointing outside the
//! project root -- a classic check-then-use TOCTOU (CWE-367), not closed
//! by canonicalizing harder.
//!
//! This module closes that gap on Linux via `openat2(2)`'s
//! `RESOLVE_BENEATH` resolve flag (kernel 5.6+): the kernel itself refuses
//! to resolve any path component -- including through a symlink -- that
//! would step outside the directory file descriptor resolution started
//! from. There is no window between "check" and "use" because there is no
//! separate check: containment is an inherent, atomic property of the
//! syscall that does the open. Once that first resolution has produced a
//! directory fd, every further operation inside it (creating a temp file,
//! renaming over the target) uses that SAME fd with a bare
//! (single-component, `/`-free) file name -- never a fresh path string --
//! so nothing is left to race.
//!
//! **Platform scope (deliberately narrow, not overclaimed):** real,
//! kernel-enforced containment is wired up for `target_os = "linux"` +
//! `target_arch = "x86_64"` only -- the one platform this repo actually
//! builds and tests on. `openat2`'s raw syscall number (437) is stable
//! and shared across x86_64/aarch64/the generic 64-bit syscall ABI, but
//! this module makes no claim for an architecture it cannot exercise in
//! CI. Every other target (Linux on another architecture, or any
//! non-Linux OS) falls back to `path_policy::resolve_within_root`'s
//! textual check -- reported honestly via
//! `ContainmentMethod::TextualFallback` on every result rather than
//! silently claiming a guarantee that platform can't back up.
//!
//! **Not yet wired into `edit::atomic_write_with`** (CALM's actual
//! production write path, used by every `edit_lines`/`edit_symbol`/
//! `format_files` call): that needs its temp-file/rename dance redesigned
//! around an already-open directory fd end to end, plus every calling MCP
//! tool threading a `RootedFilesystem` through instead of the resolved
//! `PathBuf` `resolve_repo_path` returns today -- a substantial,
//! independently-reviewable change against the highest-traffic code path
//! in the server, not bundled into this module. This ships as a real,
//! tested, standalone primitive; wiring it into the live write path is
//! deliberately left as its own follow-up.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Whether a `RootedFilesystem` result's containment guarantee came from
/// the kernel (race-free) or a textual fallback (NOT race-free -- see this
/// module's own doc comment for which platforms land here and why).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainmentMethod {
    KernelEnforced,
    TextualFallback,
}

/// CCK-05C (audit 2026-08-10): whether a write's on-disk durability is
/// actually backed by a successful `fsync`, or merely assumed. Before this
/// existed, `write_atomic_beneath`'s directory `fsync` return code was
/// discarded (`unsafe { libc::fsync(...); }`, result unused) and every
/// write still came back as an unqualified `Ok(WriteReceipt { .. })` --
/// collapsing "the rename is durable" and "the rename applied but a crash
/// right now could still lose the new name -> inode link" into the same
/// reported outcome. A trust-kernel primitive should not make that
/// distinction unobservable to its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Every fsync this write depended on (file content, then the
    /// directory entry) reported success.
    Applied,
    /// The write itself succeeded and is visible to readers, but at least
    /// one fsync this write depended on reported failure, or (on a
    /// fallback path) was never attempted -- do not treat this write as
    /// safe against a crash/power-loss until a later, confirmed-durable
    /// write to the same directory.
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootedFsError {
    Io {
        detail: String,
    },
    /// `relative` was empty, absolute, or had no file-name component --
    /// never a valid target for any method on this type.
    InvalidRelativePath {
        relative: String,
    },
}

impl std::fmt::Display for RootedFsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { detail } => write!(f, "{detail}"),
            Self::InvalidRelativePath { relative } => {
                write!(f, "{relative:?} is not a valid relative path")
            }
        }
    }
}
impl std::error::Error for RootedFsError {}

impl From<io::Error> for RootedFsError {
    fn from(e: io::Error) -> Self {
        Self::Io {
            detail: e.to_string(),
        }
    }
}

/// Receipt for one `write_atomic_beneath` call -- what was written, how
/// strong a containment guarantee backed it, and (CCK-05C) whether it's
/// actually confirmed durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReceipt {
    pub relative_path: String,
    pub bytes_written: u64,
    pub containment: ContainmentMethod,
    pub durability: Durability,
}

pub struct RootedFilesystem {
    root: PathBuf,
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    root_fd: std::os::fd::OwnedFd,
}

impl RootedFilesystem {
    /// This root, as given to `open` -- exposed for callers/tests that
    /// need it for error messages or re-deriving a display path; never
    /// used by this type's own containment logic (that's the whole point).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Opens `root` once as a directory file descriptor (Linux x86_64) --
    /// every later `open_read_beneath`/`write_atomic_beneath` call reuses
    /// this same fd as the `openat2(RESOLVE_BENEATH)` starting point, so
    /// containment for every one of them traces back to this single,
    /// explicit root, never a re-derived path string.
    pub fn open(root: &Path) -> io::Result<Self> {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY)
                .open(root)?;
            Ok(Self {
                root: root.to_path_buf(),
                root_fd: std::os::fd::OwnedFd::from(file),
            })
        }
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            if !root.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} is not a directory", root.display()),
                ));
            }
            Ok(Self {
                root: root.to_path_buf(),
            })
        }
    }

    /// Splits `relative` into (parent-relative-dir, bare file name),
    /// rejecting anything that isn't a plain, relative, non-empty path.
    /// The trailing component this returns is always a single, `/`-free
    /// name, so every fd-relative syscall downstream operates on a bare
    /// filename -- never a fresh multi-component path string that could
    /// itself need re-checking.
    fn split_relative(relative: &str) -> Result<(&str, &str), RootedFsError> {
        if relative.is_empty() {
            return Err(RootedFsError::InvalidRelativePath {
                relative: relative.to_string(),
            });
        }
        let path = Path::new(relative);
        if path.is_absolute() {
            return Err(RootedFsError::InvalidRelativePath {
                relative: relative.to_string(),
            });
        }
        let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
            RootedFsError::InvalidRelativePath {
                relative: relative.to_string(),
            }
        })?;
        let parent = path.parent().and_then(|p| p.to_str()).unwrap_or("");
        Ok((parent, file_name))
    }

    /// Opens `relative` for reading. `ContainmentMethod::KernelEnforced` on
    /// Linux x86_64 (via `openat2(RESOLVE_BENEATH)`, race-free); every
    /// other target falls back to `path_policy::resolve_within_root`
    /// (`ContainmentMethod::TextualFallback` -- NOT race-free, see this
    /// module's own doc comment).
    pub fn open_read_beneath(
        &self,
        relative: &str,
    ) -> Result<(File, ContainmentMethod), RootedFsError> {
        let (parent, name) = Self::split_relative(relative)?;
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let dir_fd = self.open_dir_beneath(parent)?;
            let file_fd =
                linux_impl::openat2_raw(std::os::fd::AsRawFd::as_raw_fd(&dir_fd), name, 0)?;
            Ok((File::from(file_fd), ContainmentMethod::KernelEnforced))
        }
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            let _ = parent;
            let real = crate::path_policy::resolve_within_root(
                &self.root,
                relative,
                crate::path_policy::SymlinkPolicy::FollowInternalSymlinks,
            )
            .map_err(|e| RootedFsError::Io {
                detail: format!("{e:?}"),
            })?;
            let file = File::open(real)?;
            Ok((file, ContainmentMethod::TextualFallback))
        }
    }

    /// Atomically writes `content` to `relative` beneath this root: same
    /// temp-file-then-rename contract as `edit::atomic_write` (a
    /// concurrent reader can never observe a half-written file), but on
    /// Linux x86_64 both the temp file's creation and the final rename are
    /// fd-relative to an already-`openat2`-verified parent directory --
    /// never a re-resolved absolute path string -- so there is no window
    /// between containment verification and the write itself.
    pub fn write_atomic_beneath(
        &self,
        relative: &str,
        content: &str,
    ) -> Result<WriteReceipt, RootedFsError> {
        let (parent, name) = Self::split_relative(relative)?;
        let bytes_written = content.len() as u64;

        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            use std::os::fd::AsRawFd;
            let dir_fd = self.open_dir_beneath(parent)?;
            // Preserved verbatim (bypassing umask via `fchmod`, not the
            // creation `mode` argument) when `name` already exists --
            // follows a leaf symlink the same way `edit::atomic_write_with`'s
            // own `std::fs::metadata(path)` does. `None` (no existing
            // target) leaves the new file at its natural `0o666 & !umask`
            // default, same as any other fresh create.
            let original_mode = linux_impl::fstatat_mode(dir_fd.as_raw_fd(), name)?;
            let tmp_name = format!(".{name}.ci-rooted-{}.tmp", write_nonce());
            let tmp_fd = linux_impl::openat_create_new(dir_fd.as_raw_fd(), &tmp_name, 0o666)?;
            let mut tmp_file = File::from(tmp_fd);
            let write_result = (|| -> io::Result<()> {
                std::io::Write::write_all(&mut tmp_file, content.as_bytes())?;
                tmp_file.sync_all()?;
                if let Some(mode) = original_mode {
                    linux_impl::fchmod_fd(tmp_file.as_raw_fd(), mode)?;
                }
                Ok(())
            })();
            drop(tmp_file);
            if let Err(e) = write_result {
                let _ = linux_impl::unlinkat(dir_fd.as_raw_fd(), &tmp_name);
                return Err(e.into());
            }
            linux_impl::renameat_same_dir(dir_fd.as_raw_fd(), &tmp_name, name)?;
            // fsync the directory itself so the new name -> inode link is
            // durable, not just the renamed file's content -- same
            // rationale as `edit::fsync_parent_dir`. CCK-05C: the return
            // code used to be discarded here (`unsafe { libc::fsync(..); }`,
            // result unused) while still reporting an unqualified `Ok` --
            // a failed directory fsync (ENOSPC, EIO, ...) was silently
            // reported as a fully successful, durable write. Captured now
            // so the receipt can say which one actually happened.
            let dir_fsync_ok = unsafe { libc::fsync(dir_fd.as_raw_fd()) } == 0;
            Ok(WriteReceipt {
                relative_path: relative.to_string(),
                bytes_written,
                containment: ContainmentMethod::KernelEnforced,
                durability: if dir_fsync_ok {
                    Durability::Applied
                } else {
                    Durability::Uncertain
                },
            })
        }
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            // The parent must exist for any write to succeed anyway, so
            // checking containment of the PARENT (rather than the full
            // leaf path) correctly covers both an overwrite and a
            // brand-new file -- `resolve_within_root` requires its target
            // to already exist, which a new file's own path never does.
            let parent_real = if parent.is_empty() {
                self.root.clone()
            } else {
                crate::path_policy::resolve_within_root(
                    &self.root,
                    parent,
                    crate::path_policy::SymlinkPolicy::FollowInternalSymlinks,
                )
                .map_err(|e| RootedFsError::Io {
                    detail: format!("{e:?}"),
                })?
            };
            let target = parent_real.join(name);
            crate::edit::atomic_write(&target, content)?;
            // CCK-05C: `atomic_write` doesn't return a durability signal
            // (its own directory fsync is likewise best-effort, see
            // `edit::fsync_parent_dir`), so this path -- already the
            // weaker `TextualFallback` containment guarantee -- honestly
            // can't claim `Applied` the way the Linux fast path above can
            // now verify.
            Ok(WriteReceipt {
                relative_path: relative.to_string(),
                bytes_written,
                containment: ContainmentMethod::TextualFallback,
                durability: Durability::Uncertain,
            })
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn open_dir_beneath(&self, relative_dir: &str) -> io::Result<std::os::fd::OwnedFd> {
        use std::os::fd::{AsRawFd, FromRawFd};
        if relative_dir.is_empty() {
            let dup = unsafe { libc::fcntl(self.root_fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            if dup < 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(dup) });
        }
        linux_impl::openat2_raw(self.root_fd.as_raw_fd(), relative_dir, libc::O_DIRECTORY)
    }
}

/// Best-effort-unique temp-file suffix -- deliberately the same shape as
/// `edit::write_nonce` (process-local counter + wall-clock nanos + PID;
/// uniqueness, not unpredictability, is all `O_CREAT|O_EXCL` needs from
/// it), duplicated rather than shared across a crate-visibility bump on
/// that module's own private helper for one caller.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn write_nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos:x}-{counter:x}", std::process::id())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux_impl {
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{FromRawFd, OwnedFd, RawFd};

    /// Raw `openat2(2)` syscall number -- stable since its 5.6 introduction
    /// and shared across x86_64/aarch64/the generic 64-bit syscall ABI, but
    /// not exposed as a named `libc::SYS_openat2` constant for the glibc
    /// target this crate builds against (only musl targets have it in the
    /// locked libc version) -- see this module's own doc comment for the
    /// platform-scope reasoning behind hardcoding it here rather than
    /// generalizing to every architecture blind.
    const SYS_OPENAT2: libc::c_long = 437;

    /// `openat2(dir_fd, relative, { flags: O_RDONLY | extra_flags, resolve:
    /// RESOLVE_BENEATH })` -- the one call in this module that does real
    /// kernel-enforced containment. `RESOLVE_BENEATH` (not
    /// `RESOLVE_NO_SYMLINKS`) matches `path_policy::SymlinkPolicy::
    /// FollowInternalSymlinks`'s existing semantics: an internal symlink is
    /// still followed, but the kernel refuses ANY component resolution --
    /// through a symlink or a `..` -- that would step outside `dir_fd`,
    /// atomically, with no separate check-then-use window.
    pub(super) fn openat2_raw(
        dir_fd: RawFd,
        relative: &str,
        extra_flags: libc::c_int,
    ) -> io::Result<OwnedFd> {
        let c_path = CString::new(relative)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        // `open_how` is `#[non_exhaustive]` in this libc version (no direct
        // struct-literal construction, no `Default` impl either) -- it is a
        // plain-old-data struct of three `u64`s per the kernel ABI
        // (linux/openat2.h), so a zeroed value is always a valid starting
        // point to then set the fields we actually use.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (libc::O_RDONLY | extra_flags) as u64;
        how.resolve = libc::RESOLVE_BENEATH;
        let ret = unsafe {
            libc::syscall(
                SYS_OPENAT2,
                dir_fd,
                c_path.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(ret as RawFd) })
    }

    /// Plain `openat(dir_fd, name, O_WRONLY|O_CREAT|O_EXCL, mode)` -- `name`
    /// is always a bare, `/`-free component (enforced by
    /// `RootedFilesystem::split_relative`'s caller), and `dir_fd` was
    /// already verified safe by `openat2_raw` before this is ever called,
    /// so a plain (non-`openat2`) create here cannot itself traverse
    /// anywhere new to race on. `O_EXCL` makes a name collision (including
    /// a planted dangling symlink at that exact temp name) a loud
    /// `EEXIST`, never a silent follow. `mode` is subject to the process
    /// umask same as any other `open(2)` create -- callers that need an
    /// EXACT mode regardless of umask (e.g. preserving an overwritten
    /// file's original permissions) must `fchmod` the resulting fd
    /// afterward rather than rely on this parameter alone.
    pub(super) fn openat_create_new(
        dir_fd: RawFd,
        name: &str,
        mode: libc::mode_t,
    ) -> io::Result<OwnedFd> {
        let c_name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let ret = unsafe {
            libc::openat(
                dir_fd,
                c_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                mode,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(ret) })
    }

    /// `fstatat(dir_fd, name, flags = 0)` -- follows a symlink at `name`
    /// (matching `std::fs::metadata`'s own follow-symlink semantics, which
    /// is what `edit::atomic_write_with` already uses to capture a target's
    /// original permissions before overwriting it). Returns `Ok(None)` for
    /// "no such file" specifically -- a target that doesn't exist yet is
    /// not an error here, just "nothing to preserve" -- and propagates
    /// every other error (permission, I/O, a symlink loop) instead of
    /// treating it the same way.
    pub(super) fn fstatat_mode(dir_fd: RawFd, name: &str) -> io::Result<Option<u32>> {
        let c_name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::fstatat(dir_fd, c_name.as_ptr(), &mut stat, 0) };
        if ret < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(err);
        }
        Ok(Some(stat.st_mode & 0o7777))
    }

    /// `fchmod(fd, mode)` -- sets permissions on an already-open fd rather
    /// than a re-resolved path string (staying consistent with this
    /// module's fd-relative-only discipline), and bypasses the process
    /// umask the same way `std::fs::set_permissions` does, so an exact
    /// preserved mode is not silently narrowed by it.
    pub(super) fn fchmod_fd(fd: RawFd, mode: u32) -> io::Result<()> {
        let ret = unsafe { libc::fchmod(fd, mode) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `renameat(dir_fd, from, dir_fd, to)` -- both `from`/`to` bare names
    /// in the SAME already-verified directory fd, so the rename itself
    /// needs no further containment check.
    pub(super) fn renameat_same_dir(dir_fd: RawFd, from: &str, to: &str) -> io::Result<()> {
        let c_from = CString::new(from)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let c_to = CString::new(to)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let ret = unsafe { libc::renameat(dir_fd, c_from.as_ptr(), dir_fd, c_to.as_ptr()) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn unlinkat(dir_fd: RawFd, name: &str) -> io::Result<()> {
        let c_name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let ret = unsafe { libc::unlinkat(dir_fd, c_name.as_ptr(), 0) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_relative_rejects_absolute_and_empty_paths() {
        assert!(RootedFilesystem::split_relative("").is_err());
        assert!(RootedFilesystem::split_relative("/etc/passwd").is_err());
    }

    #[test]
    fn split_relative_separates_parent_and_bare_file_name() {
        assert_eq!(
            RootedFilesystem::split_relative("src/foo.rs").unwrap(),
            ("src", "foo.rs")
        );
        assert_eq!(
            RootedFilesystem::split_relative("foo.rs").unwrap(),
            ("", "foo.rs")
        );
    }

    #[test]
    fn open_rejects_a_root_that_is_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("not_a_dir.txt");
        std::fs::write(&file_path, "x").unwrap();
        assert!(RootedFilesystem::open(&file_path).is_err());
    }

    #[test]
    fn write_atomic_beneath_writes_a_new_file_and_overwrites_it() {
        let dir = tempfile::tempdir().unwrap();
        let fs = RootedFilesystem::open(dir.path()).unwrap();

        let receipt = fs.write_atomic_beneath("a.txt", "hello").unwrap();
        assert_eq!(receipt.relative_path, "a.txt");
        assert_eq!(receipt.bytes_written, 5);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "hello"
        );

        fs.write_atomic_beneath("a.txt", "goodbye").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "goodbye"
        );
    }

    #[test]
    fn write_atomic_beneath_creates_intermediate_relative_dirs_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let fs = RootedFilesystem::open(dir.path()).unwrap();

        fs.write_atomic_beneath("src/foo.rs", "fn main() {}")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/foo.rs")).unwrap(),
            "fn main() {}"
        );
    }

    #[test]
    fn open_read_beneath_round_trips_file_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi\n").unwrap();
        let fs = RootedFilesystem::open(dir.path()).unwrap();

        let (mut file, _method) = fs.open_read_beneath("a.txt").unwrap();
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut file, &mut buf).unwrap();
        assert_eq!(buf, "hi\n");
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn on_linux_x86_64_every_operation_reports_kernel_enforced_containment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi\n").unwrap();
        let fs = RootedFilesystem::open(dir.path()).unwrap();

        let (_file, read_method) = fs.open_read_beneath("a.txt").unwrap();
        assert_eq!(read_method, ContainmentMethod::KernelEnforced);

        let receipt = fs.write_atomic_beneath("b.txt", "bye").unwrap();
        assert_eq!(receipt.containment, ContainmentMethod::KernelEnforced);
        // CCK-05C: a normal successful write on a real, writable directory
        // must report a confirmed-durable receipt, not just "applied".
        assert_eq!(receipt.durability, Durability::Applied);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn write_atomic_beneath_preserves_an_overwritten_files_original_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.txt");
        std::fs::write(&target, "original").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let fs = RootedFilesystem::open(dir.path()).unwrap();

        fs.write_atomic_beneath("a.txt", "overwritten").unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            mode, 0o600,
            "overwrite must preserve the target's original permission bits, not fall back \
             to a hardcoded default"
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn write_atomic_beneath_preserves_the_executable_bit_on_overwrite() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("run.sh");
        std::fs::write(&target, "#!/bin/sh\necho old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fs = RootedFilesystem::open(dir.path()).unwrap();

        fs.write_atomic_beneath("run.sh", "#!/bin/sh\necho new")
            .unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            mode, 0o755,
            "overwriting an executable file must not clear its exec bit"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "#!/bin/sh\necho new"
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn write_atomic_beneath_on_a_leaf_symlink_replaces_the_symlink_itself_not_its_target() {
        // Documents (rather than leaves implicit) the intended semantics:
        // when the leaf name itself is a symlink, `renameat` over it
        // replaces the symlink -- exactly like `std::fs::rename` already
        // does for `edit::atomic_write_with`'s fallback path -- rather than
        // writing through it into whatever it points at.
        let dir = tempfile::tempdir().unwrap();
        let real_target = dir.path().join("real.txt");
        std::fs::write(&real_target, "untouched").unwrap();
        let link_path = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real_target, &link_path).unwrap();
        let fs = RootedFilesystem::open(dir.path()).unwrap();

        fs.write_atomic_beneath("link.txt", "via the link").unwrap();

        assert!(
            !std::fs::symlink_metadata(&link_path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the rename must have replaced the symlink itself with a regular file"
        );
        assert_eq!(std::fs::read_to_string(&link_path).unwrap(), "via the link");
        assert_eq!(
            std::fs::read_to_string(&real_target).unwrap(),
            "untouched",
            "the symlink's old target must be left alone -- the write must not have \
             followed the symlink"
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn open_dir_beneath_refuses_a_dotdot_escape_at_the_kernel_level() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(root.path().join("outside.txt"), "OUTSIDE").unwrap();

        let fs = RootedFilesystem::open(&project).unwrap();
        let err = fs.open_read_beneath("../outside.txt").unwrap_err();
        assert!(
            matches!(err, RootedFsError::Io { .. }),
            "a `..` escape must be refused by the kernel resolution itself, got {err:?}"
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn concurrent_symlink_swap_never_leaks_content_from_outside_root() {
        // CCK-05's actual reproduction of the TOCTOU class this module
        // exists to close: a symlink is repeatedly swapped, concurrently,
        // between "points inside root" and "points outside root" while
        // this thread hammers open_read_beneath in a loop. A textual
        // canonicalize-then-open check (path_policy's existing guarantee)
        // can be caught mid-swap and hand back the outside file's content;
        // openat2(RESOLVE_BENEATH) must not, on any single iteration.
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("inside.txt"), "inside-content").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "OUTSIDE-SECRET").unwrap();
        let link_path = root.path().join("target.txt");
        std::os::unix::fs::symlink(root.path().join("inside.txt"), &link_path).unwrap();

        let fs = RootedFilesystem::open(root.path()).unwrap();

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let inside_target = root.path().join("inside.txt");
        let outside_target = outside.path().join("secret.txt");
        let link_path2 = link_path.clone();
        let swapper = std::thread::spawn(move || {
            let mut toggle = false;
            while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = std::fs::remove_file(&link_path2);
                let target = if toggle {
                    &outside_target
                } else {
                    &inside_target
                };
                let _ = std::os::unix::fs::symlink(target, &link_path2);
                toggle = !toggle;
            }
        });

        for _ in 0..3000 {
            if let Ok((mut file, method)) = fs.open_read_beneath("target.txt") {
                assert_eq!(method, ContainmentMethod::KernelEnforced);
                let mut buf = String::new();
                let _ = std::io::Read::read_to_string(&mut file, &mut buf);
                assert_ne!(
                    buf, "OUTSIDE-SECRET",
                    "openat2(RESOLVE_BENEATH) must never resolve through a symlink \
                     escaping root, even mid-race"
                );
            }
        }

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        swapper.join().unwrap();
    }
}
