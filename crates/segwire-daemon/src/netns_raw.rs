//! Low-level namespace lifecycle syscall helpers.
//!
//! This module owns **all** namespace-related kernel interactions:
//! - Namespace creation via `unshare(CLONE_NEWNET)` + bind-mount
//! - Namespace deletion via `umount2` + file removal
//! - Namespace switching via `setns`
//! - Namespace file descriptor management via `nix` (open, close)
//! - Privilege checks via `nix::unistd::Uid`
//!
//! The companion `netlink_raw` module handles netlink socket operations
//! via raw netlink sockets and contains **zero** `nix` imports.

use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::Uid;
use std::os::fd::BorrowedFd;
use std::path::Path;

// ---------------------------------------------------------------------------
// Privilege check
// ---------------------------------------------------------------------------

/// Check whether the effective UID is root.
pub(crate) fn is_root() -> bool {
    Uid::effective().is_root()
}

// ---------------------------------------------------------------------------
// Namespace lifecycle (nix syscalls)
// ---------------------------------------------------------------------------

/// Create a new named network namespace.
///
/// Spawns a thread that calls `unshare(CLONE_NEWNET)` and then bind-mounts
/// `/proc/self/ns/net` onto `ns_path` to persist the namespace.
pub(crate) fn create_netns(ns_path: &Path) -> Result<(), String> {
    let ns_path = ns_path.to_path_buf();
    let result = std::thread::spawn(move || -> Result<(), String> {
        // Create a new network namespace for THIS thread only
        unshare(CloneFlags::CLONE_NEWNET)
            .map_err(|e| format!("unshare(CLONE_NEWNET) failed: {}", e))?;

        // Bind-mount /proc/self/ns/net onto the placeholder file.
        // This persists the namespace beyond the lifetime of the thread.
        let src = "/proc/self/ns/net";
        mount(
            Some(src),
            &ns_path,
            None::<&str>,
            MsFlags::MS_BIND,
            None::<&str>,
        )
        .map_err(|e| format!("bind mount failed: {}", e))?;

        Ok(())
    })
    .join()
    .map_err(|_| "thread panicked".to_string())?;

    result
}

/// Delete a named network namespace by unmounting and removing the file.
pub(crate) fn delete_netns(ns_path: &Path) -> Result<(), String> {
    // Unmount (lazy detach) the bind-mount
    umount2(ns_path, MntFlags::MNT_DETACH).map_err(|e| format!("umount2 failed: {}", e))?;

    // Remove the file
    std::fs::remove_file(ns_path).map_err(|e| format!("remove file: {}", e))?;

    Ok(())
}

/// Get the inode number of a path (used as namespace ID).
pub(crate) fn ns_inode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.ino() as u32
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Namespace file-descriptor helpers
// ---------------------------------------------------------------------------

/// Open a namespace file descriptor (read-only, close-on-exec).
pub(crate) fn open_ns_fd(path: &str) -> Result<i32, String> {
    nix::fcntl::open(
        path,
        nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|e| format!("open ns fd {}: {}", path, e))
}

/// Close a raw file descriptor.
pub(crate) fn close_fd(fd: i32) {
    let _ = nix::unistd::close(fd);
}

// ---------------------------------------------------------------------------
// Run-in-namespace helper
// ---------------------------------------------------------------------------

/// Run a closure inside the given namespace's network context.
///
/// Spawns a dedicated thread, switches it to the target namespace via
/// `setns()`, runs the closure, then restores the original namespace.
pub(crate) fn run_in_namespace<F, T>(ns_path: &str, f: F) -> Result<T, String>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let ns_path = ns_path.to_string();

    let result = std::thread::spawn(move || -> Result<T, String> {
        // Save current network namespace
        let orig_ns =
            open_ns_fd("/proc/self/ns/net").map_err(|e| format!("open current ns: {}", e))?;

        // Open target namespace
        let target_ns = open_ns_fd(&ns_path).map_err(|e| format!("open target ns: {}", e))?;

        // Switch to target namespace
        // SAFETY: the fd was just opened and is valid for the lifetime of this scope
        nix::sched::setns(
            unsafe { BorrowedFd::borrow_raw(target_ns) },
            CloneFlags::CLONE_NEWNET,
        )
        .map_err(|e| format!("setns to target: {}", e))?;
        close_fd(target_ns);

        // Run the closure
        let result = f();

        // Restore original namespace
        let restore_result = nix::sched::setns(
            unsafe { BorrowedFd::borrow_raw(orig_ns) },
            CloneFlags::CLONE_NEWNET,
        );
        close_fd(orig_ns);

        if let Err(e) = restore_result {
            // This is serious — the thread is now stuck in the wrong namespace.
            // Best we can do is log and continue (the thread will be destroyed anyway).
            eprintln!("CRITICAL: Failed to restore original namespace: {}", e);
        }

        Ok(result)
    })
    .join()
    .map_err(|_| "namespace thread panicked".to_string())?;

    result
}
