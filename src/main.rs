#[cfg(not(unix))]
compile_error!("codex-auth supports macOS and Linux only");

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{self, ExitCode};

const HELP: &str = "codex-auth - back up and swap file-based Codex credentials

Usage:
  codex-auth backup NAME [--force]
  codex-auth swap NAME
  codex-auth --help
  codex-auth --version

Profile names may contain ASCII letters, digits, '_' and '-' only.";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Backup { name: String, force: bool },
    Swap { name: String },
    Help,
    Version,
}

#[derive(Debug)]
enum Outcome {
    Help,
    Version,
    Success(String),
}

#[derive(Debug)]
enum Failure {
    Usage(String),
    Operational(String),
}

impl Failure {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Operational(_) => 1,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Usage(message) | Self::Operational(message) => message,
        }
    }
}

fn main() -> ExitCode {
    match execute(
        env::args_os().skip(1),
        env::var_os("CODEX_HOME"),
        env::var_os("HOME"),
    ) {
        Ok(Outcome::Help) => println!("{HELP}"),
        Ok(Outcome::Version) => println!("codex-auth {}", env!("CARGO_PKG_VERSION")),
        Ok(Outcome::Success(message)) => println!("{message}"),
        Err(error) => {
            eprintln!("codex-auth: {}", error.message());
            if matches!(error, Failure::Usage(_)) {
                eprintln!("Try 'codex-auth --help' for usage.");
            }
            return ExitCode::from(error.exit_code());
        }
    }

    ExitCode::SUCCESS
}

fn execute<I>(
    args: I,
    codex_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<Outcome, Failure>
where
    I: IntoIterator<Item = OsString>,
{
    let command = parse_args(args).map_err(Failure::Usage)?;

    match command {
        Command::Help => Ok(Outcome::Help),
        Command::Version => Ok(Outcome::Version),
        Command::Backup { name, force } => {
            let root = resolve_codex_home(codex_home.as_deref(), home.as_deref())
                .map_err(Failure::Operational)?;
            backup(&root, &name, force).map_err(Failure::Operational)?;
            Ok(Outcome::Success(format!(
                "backed up auth.json as auth-{name}.json"
            )))
        }
        Command::Swap { name } => {
            let root = resolve_codex_home(codex_home.as_deref(), home.as_deref())
                .map_err(Failure::Operational)?;
            swap(&root, &name).map_err(Failure::Operational)?;
            Ok(Outcome::Success(format!(
                "swapped auth.json with auth-{name}.json"
            )))
        }
    }
}

fn parse_args<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();

    match args.as_slice() {
        [arg] if arg == "--help" || arg == "-h" => Ok(Command::Help),
        [arg] if arg == "--version" || arg == "-V" => Ok(Command::Version),
        [command, name] if command == "backup" => Ok(Command::Backup {
            name: parse_name(name)?,
            force: false,
        }),
        [command, name, force] if command == "backup" && force == "--force" => {
            Ok(Command::Backup {
                name: parse_name(name)?,
                force: true,
            })
        }
        [command, name] if command == "swap" => Ok(Command::Swap {
            name: parse_name(name)?,
        }),
        [] => Err("missing command".into()),
        _ => Err("invalid arguments".into()),
    }
}

fn parse_name(name: &OsStr) -> Result<String, String> {
    let name = name
        .to_str()
        .ok_or_else(|| "profile name must be valid UTF-8".to_string())?;

    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("profile name may contain only ASCII letters, digits, '_' and '-'".into());
    }

    Ok(name.to_string())
}

fn resolve_codex_home(codex_home: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf, String> {
    if let Some(path) = codex_home.filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    home.filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(path).join(".codex"))
        .ok_or_else(|| "neither CODEX_HOME nor HOME is set".into())
}

fn backup(root: &Path, name: &str, force: bool) -> Result<(), String> {
    let source = root.join("auth.json");
    let destination = profile_path(root, name);
    validate_source(&source)?;

    atomic_copy(&source, &destination, force).map_err(|error| {
        if !force && error.kind() == io::ErrorKind::AlreadyExists {
            format!(
                "{} already exists; rerun with --force to replace it",
                destination.display()
            )
        } else {
            format!(
                "could not copy {} to {}: {error}",
                source.display(),
                destination.display()
            )
        }
    })
}

fn swap(root: &Path, name: &str) -> Result<(), String> {
    let source = profile_path(root, name);
    let destination = root.join("auth.json");
    validate_source(&source)?;

    atomic_copy(&source, &destination, true).map_err(|error| {
        format!(
            "could not copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })
}

fn profile_path(root: &Path, name: &str) -> PathBuf {
    root.join(format!("auth-{name}.json"))
}

fn validate_source(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot use {}: {error}", path.display()))?;

    if !metadata.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() == 0 {
        return Err(format!("{} is empty", path.display()));
    }

    Ok(())
}

fn atomic_copy(source: &Path, destination: &Path, overwrite: bool) -> io::Result<()> {
    let mut source = File::open(source)?;
    let (temporary, mut output) = TemporaryFile::create(destination)?;
    let copied = io::copy(&mut source, &mut output)?;

    if copied == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source file is empty",
        ));
    }

    output.flush()?;
    output.sync_all()?;
    drop(output);

    if overwrite {
        fs::rename(&temporary.path, destination)?;
    } else {
        fs::hard_link(&temporary.path, destination)?;
    }

    Ok(())
}

struct TemporaryFile {
    path: PathBuf,
}

impl TemporaryFile {
    fn create(destination: &Path) -> io::Result<(Self, File)> {
        let parent = destination.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent")
        })?;
        let file_name = destination.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
        })?;

        for attempt in 0..100 {
            let path = parent.join(format!(
                ".{}.{}.{}.tmp",
                file_name.to_string_lossy(),
                process::id(),
                attempt
            ));

            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => return Ok((Self { path }, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique temporary file",
        ))
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "codex-auth-test-{}-{}",
                process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn write_file(path: &Path, contents: &[u8], mode: u32) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)
            .unwrap();
        file.write_all(contents).unwrap();
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn temporary_files(root: &Path) -> Vec<PathBuf> {
        fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension() == Some(OsStr::new("tmp")))
            .collect()
    }

    #[test]
    fn parses_the_supported_command_grammar() {
        assert_eq!(parse_args(args(&["--help"])), Ok(Command::Help));
        assert_eq!(parse_args(args(&["-V"])), Ok(Command::Version));
        assert_eq!(
            parse_args(args(&["backup", "personal"])),
            Ok(Command::Backup {
                name: "personal".into(),
                force: false
            })
        );
        assert_eq!(
            parse_args(args(&["backup", "work-main", "--force"])),
            Ok(Command::Backup {
                name: "work-main".into(),
                force: true
            })
        );
        assert_eq!(
            parse_args(args(&["swap", "work_2"])),
            Ok(Command::Swap {
                name: "work_2".into()
            })
        );
    }

    #[test]
    fn rejects_invalid_arguments_and_profile_names() {
        for invalid in [
            args(&[]),
            args(&["list"]),
            args(&["backup", ""]),
            args(&["backup", "../work"]),
            args(&["backup", "work profile"]),
            args(&["backup", "work", "--force", "extra"]),
            args(&["backup", "--force", "work"]),
            args(&["swap", "work", "--force"]),
        ] {
            assert!(parse_args(invalid).is_err());
        }
    }

    #[test]
    fn resolves_codex_home_with_the_documented_precedence() {
        assert_eq!(
            resolve_codex_home(Some(OsStr::new("/custom")), Some(OsStr::new("/home/me"))),
            Ok(PathBuf::from("/custom"))
        );
        assert_eq!(
            resolve_codex_home(Some(OsStr::new("")), Some(OsStr::new("/home/me"))),
            Ok(PathBuf::from("/home/me/.codex"))
        );
        assert!(resolve_codex_home(None, None).is_err());
    }

    #[test]
    fn backup_refuses_overwrite_then_force_replaces_atomically() {
        let root = TestDirectory::new();
        let active = root.path().join("auth.json");
        let saved = root.path().join("auth-personal.json");
        write_file(&active, b"first secret", 0o644);

        backup(root.path(), "personal", false).unwrap();
        assert_eq!(fs::read(&saved).unwrap(), b"first secret");
        assert_eq!(mode(&saved), 0o600);

        fs::write(&active, b"second secret").unwrap();
        assert!(backup(root.path(), "personal", false).is_err());
        assert_eq!(fs::read(&saved).unwrap(), b"first secret");

        backup(root.path(), "personal", true).unwrap();
        assert_eq!(fs::read(&saved).unwrap(), b"second secret");
        assert_eq!(mode(&saved), 0o600);
        assert!(temporary_files(root.path()).is_empty());
    }

    #[test]
    fn swap_replaces_active_credentials_and_secures_permissions() {
        let root = TestDirectory::new();
        let active = root.path().join("auth.json");
        let saved = root.path().join("auth-work.json");
        write_file(&active, b"personal secret", 0o644);
        write_file(&saved, b"work secret", 0o644);

        swap(root.path(), "work").unwrap();

        assert_eq!(fs::read(&active).unwrap(), b"work secret");
        assert_eq!(mode(&active), 0o600);
        assert!(temporary_files(root.path()).is_empty());
    }

    #[test]
    fn invalid_sources_do_not_modify_existing_credentials() {
        let root = TestDirectory::new();
        let active = root.path().join("auth.json");
        write_file(&active, b"keep me", 0o600);

        assert!(swap(root.path(), "missing").is_err());
        assert_eq!(fs::read(&active).unwrap(), b"keep me");

        let empty = root.path().join("auth-empty.json");
        write_file(&empty, b"", 0o600);
        assert!(swap(root.path(), "empty").is_err());
        assert_eq!(fs::read(&active).unwrap(), b"keep me");

        fs::create_dir(root.path().join("auth-directory.json")).unwrap();
        assert!(swap(root.path(), "directory").is_err());
        assert_eq!(fs::read(&active).unwrap(), b"keep me");
        assert!(temporary_files(root.path()).is_empty());
    }

    #[test]
    fn failures_use_the_documented_exit_codes() {
        let usage = execute(args(&["unknown"]), None, None).unwrap_err();
        assert!(matches!(usage, Failure::Usage(_)));
        assert_eq!(usage.exit_code(), 2);

        let operational = execute(args(&["swap", "work"]), None, None).unwrap_err();
        assert!(matches!(operational, Failure::Operational(_)));
        assert_eq!(operational.exit_code(), 1);
    }

    #[test]
    fn help_and_version_do_not_require_a_home_directory() {
        assert!(matches!(
            execute(args(&["--help"]), None, None),
            Ok(Outcome::Help)
        ));
        assert!(matches!(
            execute(args(&["--version"]), None, None),
            Ok(Outcome::Version)
        ));
    }
}
