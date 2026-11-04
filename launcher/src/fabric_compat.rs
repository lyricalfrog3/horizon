//! Horizon Fabric Compatibility (HFC) pre-launch hook.
//!
//! HFC is a separate tool, not part of the launcher build: its conversion
//! pipeline lives in the compatibility package. The launcher's only job is to
//! notice an installation and run it with the instance's mods folder before
//! the game starts.
//!
//! The installation is a folder named `HorizonFabricCompat` with an `hfc.toml`
//! inside the instance's `horizon-mods`. No marker means no work and no cost.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

pub const COMPAT_DIR: &str = "HorizonFabricCompat";
pub const MARKER: &str = "hfc.toml";
/// An explicit HFC binary wins over discovery. Set for a packaged install or
/// a development build.
pub const BIN_ENV: &str = "HFC_BIN";

/// Whether this game folder has an HFC installation.
pub fn installed(home: &Path) -> bool {
    home.join("horizon-mods")
        .join(COMPAT_DIR)
        .join(MARKER)
        .is_file()
}

/// Run HFC for this game folder when it is installed.
///
/// `None` means HFC is not installed (nothing ran). `Some(Err(..))` means it
/// is installed but could not be run or reported a failure; the caller should
/// show it and may still start the game, exactly as it does for an offline
/// session.
pub fn bootstrap(home: &Path) -> Option<Result<String, String>> {
    if !installed(home) {
        return None;
    }
    let binary = match find_binary() {
        Some(binary) => binary,
        None => {
            return Some(Err(format!(
                "found {COMPAT_DIR}/{MARKER} but no `hfc` program; put the HFC executable \
                 beside the launcher or set {BIN_ENV}"
            )));
        }
    };
    Some(run(home, &binary))
}

/// Run one explicit binary. Kept separate so tests do not depend on the
/// environment or on HFC being installed on the machine.
pub fn run(home: &Path, binary: &Path) -> Result<String, String> {
    let mods = home.join("horizon-mods");
    let output = Command::new(binary)
        .arg("bootstrap")
        .arg("--mods")
        .arg(&mods)
        .current_dir(home)
        .output()
        .map_err(|e| format!("could not run {}: {e}", binary.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let last = stdout
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("finished")
            .trim()
            .to_owned();
        return Ok(format!("Horizon Fabric Compatibility: {last}"));
    }
    let detail = if stderr.trim().is_empty() {
        stdout.trim().to_owned()
    } else {
        stderr.trim().to_owned()
    };
    Err(format!(
        "{} exited with {}: {detail}",
        binary.display(),
        output.status
    ))
}

/// `HFC_BIN`, then `hfc` beside the launcher, then `hfc` on `PATH`.
fn find_binary() -> Option<PathBuf> {
    if let Some(path) = env::var_os(BIN_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in binary_names() {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .flat_map(|dir| binary_names().map(move |name| dir.join(name)))
            .find(|candidate| candidate.is_file())
    })
}

fn binary_names() -> impl Iterator<Item = &'static str> {
    // `.exe` discovery on Windows; Unix ignores the second name naturally.
    ["hfc", "hfc.exe"].into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horizon-hfc-hook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("horizon-mods")).unwrap();
        dir
    }

    #[test]
    fn no_marker_means_no_work() {
        let home = temp("absent");
        assert!(!installed(&home));
        assert!(bootstrap(&home).is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    fn script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-hfc");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        path
    }

    #[test]
    #[cfg(unix)]
    fn runs_the_bootstrap_with_the_instance_mods_folder() {
        let home = temp("runs");
        std::fs::create_dir_all(home.join("horizon-mods/HorizonFabricCompat")).unwrap();
        std::fs::write(home.join("horizon-mods/HorizonFabricCompat/hfc.toml"), "").unwrap();
        let binary = script(
            &home,
            "printf '%s\\n' \"$@\" > \"$PWD/args.txt\"; echo 'HFC: 1 installed'",
        );
        let message = run(&home, &binary).unwrap();
        assert!(message.contains("1 installed"));
        let args = std::fs::read_to_string(home.join("args.txt")).unwrap();
        assert!(args.contains("bootstrap"));
        assert!(args.contains("horizon-mods"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    #[cfg(unix)]
    fn a_failing_binary_is_reported_not_hidden() {
        let home = temp("fails");
        std::fs::create_dir_all(home.join("horizon-mods/HorizonFabricCompat")).unwrap();
        std::fs::write(home.join("horizon-mods/HorizonFabricCompat/hfc.toml"), "").unwrap();
        let binary = script(&home, "echo 'no permission' >&2; exit 3");
        let error = run(&home, &binary).unwrap_err();
        assert!(error.contains("no permission"));
        assert!(error.contains("exited with"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
