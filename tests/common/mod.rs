//! What every end-to-end test binary shares: the host, the plugin built for
//! it, and the lock directory that ties the two. `BDDKIT_BIN` names a `bddkit`
//! built with `--features dev-unreserve`. Without it the tests skip, except
//! under `CI`, where a missing host is a failure: a skipped suite must never
//! look green there.

#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "each test binary uses a part of this module; `allow-*-in-tests` covers only `#[test]` functions, not the helpers here"
)]

pub mod db;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

pub fn bddkit_bin() -> Option<PathBuf> {
    std::env::var("BDDKIT_BIN")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

/// Returns from the test, with a note, when there is no host to run it on.
#[macro_export]
macro_rules! require_bddkit {
    () => {
        match $crate::common::bddkit_bin() {
            Some(bin) => bin,
            None if std::env::var_os("CI").is_some() => {
                panic!("BDDKIT_BIN does not name a bddkit binary, and CI must not skip")
            }
            None => {
                eprintln!("SKIP: set BDDKIT_BIN to a bddkit built with --features dev-unreserve");
                return;
            }
        }
    };
}

pub fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Builds the plugin once and writes a lock file for it into a directory
/// handed to `--bddkit-dir`.
pub fn lock_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let out = Command::new(env!("CARGO"))
            .arg("build")
            .current_dir(root())
            .output()
            .expect("cargo build");
        assert!(
            out.status.success(),
            "plugin build failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let library = root().join("target/debug").join(format!(
            "{}bddkit_db{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ));
        let dir = std::env::temp_dir().join(format!("bddkit-db-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("plugins.yaml"),
            format!("plugin:\n  - name: db\n    path: {}\n", library.display()),
        )
        .expect("lock");
        dir
    })
}

/// `bddkit <args> --bddkit-dir <lock dir>`, not yet run.
pub fn command(bin: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(bin);
    command.args(args).arg("--bddkit-dir").arg(lock_dir());
    command
}

pub fn bddkit(bin: &Path, args: &[&str]) -> Output {
    command(bin, args).output().expect("bddkit")
}

/// Standard output and standard error as one string.
pub fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
