//! Reads library audio from an SSD mirror when one is plugged in (fork only).
//!
//! The searcher keeps a one-way copy of the library's audio on a volume
//! labelled `ssd`, under `<volume>\lib\<drive letter>\<path>`. The HDD stays
//! the source of truth; this only picks the copy for reading when it is the
//! same file (same size and modification time, within 2 s). Anything else —
//! no such volume, no copy, a stale copy — reads the original path.
//!
//! `RBXPORT_SSD_MIRROR` overrides the mirror root (empty: off);
//! `RBXPORT_SSD_LABEL` changes the volume label looked for.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const LOOKUP_TTL: Duration = Duration::from_secs(60);

static ROOT: Mutex<Option<(Instant, Option<PathBuf>)>> = Mutex::new(None);

/// The mirror root (`F:\lib`), looked up at most once a minute.
fn mirror_root() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("RBXPORT_SSD_MIRROR") {
        return (!root.is_empty()).then(|| PathBuf::from(root));
    }
    let mut cached = ROOT.lock().ok()?;
    if let Some((at, root)) = cached.as_ref() {
        if at.elapsed() < LOOKUP_TTL {
            return root.clone();
        }
    }
    let label = std::env::var("RBXPORT_SSD_LABEL").unwrap_or_else(|_| "ssd".to_owned());
    let found = find_volume(&label).map(|drive| drive.join("lib"));
    *cached = Some((Instant::now(), found.clone()));
    found
}

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "GetLogicalDrives / GetVolumeInformationW are Win32 calls; the root is NUL-terminated and the name buffer's length is passed"
)]
fn find_volume(label: &str) -> Option<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::{GetLogicalDrives, GetVolumeInformationW};
    // SAFETY: no arguments; returns a bit mask of the drive letters in use.
    let mask = unsafe { GetLogicalDrives() };
    for (i, letter) in ('C'..='Z').enumerate() {
        if mask >> (i + 2) & 1 == 0 {
            continue;
        }
        let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain(Some(0)).collect();
        let mut name = [0u16; 261];
        // SAFETY: `root` is NUL-terminated, `name` is a writable buffer of the
        // length passed, and the optional out-pointers are null.
        let ok = unsafe {
            GetVolumeInformationW(root.as_ptr(), name.as_mut_ptr(), 261, std::ptr::null_mut(),
                std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0)
        };
        if ok != 0 {
            let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            if String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case(label) {
                return Some(PathBuf::from(format!("{letter}:\\")));
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn find_volume(_label: &str) -> Option<PathBuf> {
    None
}

/// Where the mirror would hold `path`: `D:\MUSIC\x.flac` → `<root>\D\MUSIC\x.flac`.
fn mirror_of(path: &Path, root: &Path) -> Option<PathBuf> {
    let mut parts = path.components();
    let drive = match parts.next()? {
        std::path::Component::Prefix(prefix) => prefix.as_os_str().to_string_lossy().trim_end_matches(':').to_owned(),
        _ => return None,
    };
    let rest: PathBuf = parts.filter(|c| !matches!(c, std::path::Component::RootDir)).collect();
    Some(root.join(drive).join(rest))
}

fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
    if a.len() != b.len() {
        return false;
    }
    match (a.modified(), b.modified()) {
        (Ok(x), Ok(y)) => x.duration_since(y).or_else(|_| y.duration_since(x)).is_ok_and(|d| d < Duration::from_secs(2)),
        _ => false,
    }
}

/// The path to read `path` from: its SSD copy when that is the same file, else `path` itself.
pub fn fast_path(path: &Path) -> PathBuf {
    if let Some(root) = mirror_root() {
        if let Some(copy) = mirror_of(path, &root) {
            if same_file(path, &copy) {
                return copy;
            }
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn maps_a_drive_path_under_the_root() {
        let got = mirror_of(Path::new(r"D:\MUSIC\a b\x.flac"), Path::new(r"F:\lib")).unwrap();
        assert_eq!(got, PathBuf::from(r"F:\lib\D\MUSIC\a b\x.flac"));
    }

    #[test]
    fn reads_the_copy_only_when_it_matches() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.flac");
        let copy = dir.path().join("copy.flac");
        std::fs::write(&src, b"abc").unwrap();
        std::fs::copy(&src, &copy).unwrap();
        let t = std::fs::metadata(&src).unwrap().modified().unwrap();
        std::fs::File::options().write(true).open(&copy).unwrap().set_modified(t).unwrap();
        assert!(same_file(&src, &copy));
        std::fs::write(&copy, b"abcd").unwrap();
        assert!(!same_file(&src, &copy));
    }
}
