//! The install script, run against a release that is a folder on this
//! machine: what it installs, and what it refuses to.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_mux");
const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../install.sh");

/// Every name a release gives a build. The same file stands for each, so
/// that the test need not know which one this machine takes.
const BUILDS: [&str; 4] = [
    "mux-Darwin-arm64",
    "mux-Darwin-x86_64",
    "mux-Linux-aarch64",
    "mux-Linux-x86_64",
];

/// The SHA-256 digest of a file, by the tool the script uses for it.
fn digest(path: &Path) -> String {
    let by = |tool: &str, args: &[&str]| {
        let run = Command::new(tool).args(args).arg(path).output().ok()?;
        let said = String::from_utf8(run.stdout).ok()?;
        said.split_whitespace().next().map(str::to_owned)
    };
    by("sha256sum", &[])
        .or_else(|| by("shasum", &["-a", "256"]))
        .expect("neither sha256sum nor shasum is here")
}

/// Makes the folder `at` hold a release: this build of `mux` under every
/// name, and the list of digests, in which each is `listed`.
fn release(at: &Path, listed: Option<&str>) {
    std::fs::create_dir_all(at).unwrap();
    for name in BUILDS {
        std::fs::copy(BIN, at.join(name)).unwrap();
    }
    // A line of the list is a digest, two spaces and a name.
    let line = |name: &&str| listed.map(|listed| format!("{listed}  {name}\n"));
    let list: String = BUILDS.iter().filter_map(line).collect();
    std::fs::write(at.join("checksums-sha256.txt"), list).unwrap();
}

/// A folder to install from, one to install into, and the script.
struct Trial {
    tmp: tempfile::TempDir,
}

impl Trial {
    fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    /// Where the newest release is kept.
    fn latest(&self) -> PathBuf {
        self.tmp.path().join("releases/latest/download")
    }

    /// Where the release named `version` is kept.
    fn named(&self, version: &str) -> PathBuf {
        self.tmp.path().join("releases/download").join(version)
    }

    /// The folder `mux` is to be put in.
    fn folder(&self) -> PathBuf {
        self.tmp.path().join("bin")
    }

    /// The script, told where the releases are and where to install.
    fn script(&self) -> Command {
        let mut script = Command::new("sh");
        let releases = format!("file://{}/releases", self.tmp.path().display());
        script
            .arg(SCRIPT)
            .env("MUX_RELEASES", releases)
            .env("MUX_INSTALL", self.folder())
            .env_remove("MUX_VERSION");
        script
    }

    fn installed(&self) -> PathBuf {
        self.folder().join("mux")
    }
}

fn said(output: &Output) -> (String, String) {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    (text(&output.stdout), text(&output.stderr))
}

fn version_of(binary: &Path) -> String {
    let asked = Command::new(binary).arg("--version").output().unwrap();
    String::from_utf8(asked.stdout).unwrap()
}

#[test]
fn the_script_installs_the_build_for_this_machine_and_says_how_to_run_it() {
    let trial = Trial::new();
    release(&trial.latest(), Some(&digest(Path::new(BIN))));

    let run = trial.script().output().unwrap();
    let (out, err) = said(&run);
    assert!(run.status.success(), "{err}");
    assert_eq!(version_of(&trial.installed()), version_of(Path::new(BIN)));
    let mode = std::fs::metadata(trial.installed()).unwrap().permissions();
    assert_eq!(mode.mode() & 0o777, 0o755);

    // It says what it installed and where, how to run it from a folder
    // that is not on the path, and how to take it away again.
    let into = trial.folder();
    let version = version_of(Path::new(BIN));
    let installed = format!("installed {} at {}", version.trim(), into.display());
    assert!(out.contains(&installed), "{out}");
    assert!(
        out.contains(&format!("export PATH=\"{}:", into.display())),
        "{out}"
    );
    assert!(
        out.contains(&format!("rm {}", trial.installed().display())),
        "{out}"
    );

    // Run again, it puts the release in the place of what is there. With
    // the folder on the path, there is nothing to say of the path.
    std::fs::write(trial.installed(), "an older mux").unwrap();
    let path = format!("{}:{}", into.display(), std::env::var("PATH").unwrap());
    let again = trial.script().env("PATH", path).output().unwrap();
    let (out, err) = said(&again);
    assert!(again.status.success(), "{err}");
    assert!(!out.contains("export PATH"), "{out}");
    assert_eq!(version_of(&trial.installed()), version);
    // Nothing but `mux` is left in the folder.
    let left: Vec<_> = std::fs::read_dir(&into).unwrap().collect();
    assert_eq!(left.len(), 1, "{left:?}");
}

#[test]
fn a_release_that_is_named_is_fetched_and_no_other() {
    let trial = Trial::new();
    // There is no newest release to fall back on.
    release(&trial.named("v1.2.3"), Some(&digest(Path::new(BIN))));

    let run = trial
        .script()
        .env("MUX_VERSION", "v1.2.3")
        .output()
        .unwrap();
    let (out, err) = said(&run);
    assert!(run.status.success(), "{err}");
    assert!(out.contains("(v1.2.3)"), "{out}");
    assert!(trial.installed().exists());

    let other = trial
        .script()
        .env("MUX_VERSION", "v9.9.9")
        .output()
        .unwrap();
    assert_eq!(other.status.code(), Some(1));
    assert!(said(&other).1.contains("could not download"));
}

/// A download that is not what the release says it is, is not installed,
/// and a `mux` that was there before stays as it was.
#[test]
fn a_download_with_another_digest_is_not_installed() {
    let trial = Trial::new();
    release(&trial.latest(), Some(&"0".repeat(64)));
    std::fs::create_dir_all(trial.folder()).unwrap();
    std::fs::write(trial.installed(), "an older mux").unwrap();

    let run = trial.script().output().unwrap();
    let (out, err) = said(&run);
    assert_eq!(run.status.code(), Some(1), "{out}");
    assert!(err.contains("was not installed"), "{err}");
    assert!(err.contains(&digest(Path::new(BIN))), "{err}");
    assert!(!out.contains("installed"), "{out}");
    assert_eq!(std::fs::read(trial.installed()).unwrap(), b"an older mux");
}

#[test]
fn a_release_whose_list_does_not_name_the_build_is_not_installed() {
    let trial = Trial::new();
    release(&trial.latest(), None);

    let run = trial.script().output().unwrap();
    assert_eq!(run.status.code(), Some(1));
    assert!(said(&run).1.contains("does not name mux-"));
    assert!(!trial.installed().exists());
}

/// On a system there is no build for, the script says so and downloads
/// nothing. The system is one that a `uname` put first on the path
/// describes.
#[test]
fn a_machine_there_is_no_build_for_is_told_so() {
    let trial = Trial::new();
    release(&trial.latest(), Some(&digest(Path::new(BIN))));
    let fake = trial.tmp.path().join("fake");
    std::fs::create_dir(&fake).unwrap();
    let uname = "#!/bin/sh\ncase \"$1\" in -s) echo Plan9 ;; -m) echo mips ;; esac\n";
    std::fs::write(fake.join("uname"), uname).unwrap();
    let executable = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(fake.join("uname"), executable).unwrap();

    let path = format!("{}:{}", fake.display(), std::env::var("PATH").unwrap());
    let run = trial.script().env("PATH", path).output().unwrap();
    let (out, err) = said(&run);
    assert_eq!(run.status.code(), Some(1), "{out}");
    assert!(err.contains("no build of mux for Plan9 on mips"), "{err}");
    assert!(!out.contains("downloading"), "{out}");
    assert!(!trial.installed().exists());
}

/// The line that installs mux is written in four places. They say the
/// same, so that changing the address in one cannot leave another behind.
#[test]
fn the_install_line_is_the_same_wherever_it_is_written() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap();
    // As the script says it of itself, in the comment at its top.
    let script = read("install.sh");
    let line = script
        .lines()
        .find_map(|line| line.strip_prefix("#     curl "))
        .map(|rest| format!("curl {rest}"))
        .expect("the script says how it is run");
    assert!(line.ends_with("/install.sh | sh"), "{line}");
    for path in ["README.md", "site/index.html", "site/llms.txt"] {
        assert!(read(path).contains(&line), "{path} does not say `{line}`");
    }
}
