//! The server's settings: where they come from, and the two rules the
//! Product Owner set for them (E-19, E-20; docs/DECISIONS.md).
//!
//! **Source.** The process environment, then one file — `sync.env` —
//! that the server reads itself: `$NEMR_SYNC_ENV_FILE` if set (empty means
//! read no file), else `$XDG_CONFIG_HOME/nemr/sync.env`, else
//! `~/.config/nemr/sync.env`. `KEY=VALUE` lines; blank lines and `#`
//! comments ignored; a key already in the process environment wins. The
//! file is refused — not repaired — unless its mode is 0600 or tighter,
//! because it holds the pepper and, under E-20, the object-store
//! credential. Key names are logged at start; values never are. A
//! malformed line is reported by number, never by content.
//!
//! **The pepper (E-19).** `NEMR_AUTH_PEPPER` is required: a server with no
//! pepper refuses to bind, naming the file and the exact line to add. The
//! one escape hatch is the unmistakable value `ephemeral`, which starts the
//! server with a random per-process pepper and a loud warning — for
//! throwaway servers such as the acceptances, so a test server never looks
//! like a configured one and a configured one is never silently weaker
//! than it claims.
//!
//! **The storage backend (E-20).** The backend's own variables are the
//! switch: a complete `NEMR_S3_*` set names an object store;
//! `NEMR_BUNDLE_DIR` names a directory that must already exist. Exactly
//! one — both is refused naming both, neither is refused naming both, an
//! empty value means unset, and a half-configured S3 is S3's own refusal,
//! never a fall-back to a directory. No selector variable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

/// The name of the file, for messages.
pub const ENV_FILE_NAME: &str = "sync.env";

/// One setting, from wherever a SERVER would get it: the process environment
/// first, then `sync.env`. A refusal names the key and the file, because those
/// are the two things the reader needs and neither is guessable.
///
/// This exists so that nothing — not a test, not a script, not a command —
/// needs a human to export something a server's own settings file already
/// holds. The Product Owner's rule (2026-09-11): *"anything that needs me to
/// export something first either reads the file or refuses naming the file and
/// the key."*
pub fn setting(key: &str) -> Result<String> {
    let env = |k: &str| std::env::var(k).ok();
    if let Some(v) = env(key).filter(|v| !v.is_empty()) {
        return Ok(v);
    }
    let settings = Settings::load(&env)?;
    settings
        .get(key, &env)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "{key} is not set, and {} does not carry it either.\n  \
                 Put it in that file (mode 0600), or export it for this command:\n      \
                 {key}=...\n  \
                 `nemr server configure` writes that file for you.",
                settings.file_for_messages()
            )
        })
}

/// Where the file is looked for, in order, given the environment.
pub fn env_file_path(get: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(explicit) = get("NEMR_SYNC_ENV_FILE") {
        return if explicit.is_empty() {
            None
        } else {
            Some(PathBuf::from(explicit))
        };
    }
    if let Some(cfg) = get("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return Some(Path::new(&cfg).join("nemr").join(ENV_FILE_NAME));
    }
    get("HOME").filter(|s| !s.is_empty()).map(|h| {
        Path::new(&h)
            .join(".config")
            .join("nemr")
            .join(ENV_FILE_NAME)
    })
}

/// Parse `KEY=VALUE` lines. Returns the pairs in file order. An error
/// names the line number only — the line may hold a secret.
pub fn parse_env_file(text: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((k, v)) = line.split_once('=') else {
            bail!("{ENV_FILE_NAME}: line {} is not KEY=VALUE", i + 1);
        };
        let k = k.trim();
        if k.is_empty()
            || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || k.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            bail!("{ENV_FILE_NAME}: line {} has an invalid key name", i + 1);
        }
        let v = v.trim();
        // Optional surrounding quotes, as a shell would strip them.
        let v = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(v);
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

/// The file's mode must be 0600 or tighter: nothing for group or other.
/// Read, never repaired — a file another user can read has already been
/// readable; changing its mode now would hide that.
pub fn check_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "{} is mode {:04o}; it holds secrets and must be 0600 or tighter (chmod 600 {})",
            path.display(),
            mode,
            path.display()
        );
    }
    Ok(())
}

/// What the server knows after reading its sources: the merged settings and
/// where each came from, for the start-up line.
#[derive(Debug)]
pub struct Settings {
    pub values: BTreeMap<String, String>,
    pub file: Option<PathBuf>,
    /// Keys that came from the file (the environment did not carry them).
    pub from_file: Vec<String>,
}

impl Settings {
    /// Merge the process environment with the file, environment winning key
    /// by key. A missing file is not an error (a bare box); a present file
    /// with a wide mode or a malformed line is.
    pub fn load(get: &dyn Fn(&str) -> Option<String>) -> Result<Settings> {
        let file = env_file_path(get);
        let mut values = BTreeMap::new();
        let mut from_file = Vec::new();
        if let Some(path) = file.as_ref() {
            if path.exists() {
                check_mode(path)?;
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                for (k, v) in parse_env_file(&text)? {
                    if get(&k).is_none() {
                        values.insert(k.clone(), v);
                        from_file.push(k);
                    }
                }
            }
        }
        Ok(Settings {
            values,
            file,
            from_file,
        })
    }

    /// A setting: the process environment first, then the file. Empty
    /// means unset in both.
    pub fn get(&self, key: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
        env(key)
            .or_else(|| self.values.get(key).cloned())
            .filter(|s| !s.is_empty())
    }

    /// The file's path for messages: the one that would have been read.
    pub fn file_for_messages(&self) -> String {
        self.file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("~/.config/nemr/{ENV_FILE_NAME}"))
    }
}

/// The pepper, as ruled (E-19): required; `ephemeral` is the loud escape
/// hatch for a throwaway server.
#[derive(Debug)]
pub enum Pepper {
    Configured([u8; 32]),
    Ephemeral,
}

pub fn pepper(settings: &Settings, env: &dyn Fn(&str) -> Option<String>) -> Result<Pepper> {
    match settings.get("NEMR_AUTH_PEPPER", env) {
        None => Err(anyhow!(
            "NEMR_AUTH_PEPPER is not set, so this server will not bind: an unset pepper makes \
             /v1/auth/params an account-enumeration oracle after every restart (F-89).\n\
             Add this line to {file} (mode 0600):\n\n    NEMR_AUTH_PEPPER=<any long random string>\n\n\
             for example:  umask 077; mkdir -p \"$(dirname {file})\"; \
             printf 'NEMR_AUTH_PEPPER=%s\\n' \"$(head -c 32 /dev/urandom | base64 -w0)\" >> {file}\n\
             For a THROWAWAY server only, NEMR_AUTH_PEPPER=ephemeral starts with a random pepper and a warning.",
            file = settings.file_for_messages()
        )),
        Some(v) if v == "ephemeral" => Ok(Pepper::Ephemeral),
        Some(v) => {
            use sha2::{Digest, Sha256};
            Ok(Pepper::Configured(Sha256::digest(v.as_bytes()).into()))
        }
    }
}

/// Which backend the settings name (E-20). Pure: takes a lookup, returns
/// a choice or a refusal, touches nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum StorageChoice {
    Local(PathBuf),
    S3,
}

pub fn select_storage(get: &dyn Fn(&str) -> Option<String>) -> Result<StorageChoice> {
    let set = |k: &str| get(k).filter(|v| !v.is_empty());
    let dir = set("NEMR_BUNDLE_DIR");
    let s3_names = [
        "NEMR_S3_PROVIDER",
        "NEMR_S3_BUCKET",
        "NEMR_S3_ENDPOINT",
        "NEMR_S3_ACCESS_KEY_ID",
        "NEMR_S3_SECRET_ACCESS_KEY",
    ];
    let s3_any = s3_names.iter().any(|k| set(k).is_some());
    match (dir, s3_any) {
        (Some(_), true) => bail!(
            "both NEMR_BUNDLE_DIR and NEMR_S3_* are set; exactly one backend is allowed — \
             unset one (a directory for tests and self-hosting, an object store for anything shared)"
        ),
        (None, false) => bail!(
            "no storage backend is configured; set exactly one: NEMR_BUNDLE_DIR=<existing directory>, \
             or NEMR_S3_PROVIDER, NEMR_S3_BUCKET, NEMR_S3_ENDPOINT, NEMR_S3_ACCESS_KEY_ID and \
             NEMR_S3_SECRET_ACCESS_KEY for an object store"
        ),
        (Some(d), false) => {
            let p = PathBuf::from(&d);
            if !p.is_dir() {
                bail!(
                    "NEMR_BUNDLE_DIR={d} is not an existing directory; it is read, not created, so a \
                     typo cannot quietly become a fresh empty store"
                );
            }
            Ok(StorageChoice::Local(p))
        }
        (None, true) => {
            // A half-configured object store is the store's own refusal
            // naming what is missing — never a fall-back to a directory.
            let missing: Vec<&str> = s3_names
                .iter()
                .copied()
                .filter(|k| set(k).is_none())
                .collect();
            if !missing.is_empty() {
                bail!(
                    "the object store is half-configured; missing: {}",
                    missing.join(", ")
                );
            }
            let provider = set("NEMR_S3_PROVIDER").unwrap_or_default();
            if !matches!(provider.as_str(), "r2" | "b2" | "s3") {
                bail!(
                    "NEMR_S3_PROVIDER={provider:?} is not one of r2, b2, s3 — a typo must not \
                     silently become a generic endpoint"
                );
            }
            Ok(StorageChoice::S3)
        }
    }
}

#[cfg(test)]
mod tests {
    /// The rule the Product Owner set: nothing should need a human to export
    /// what the server's own settings file already holds. This is that rule,
    /// as a test — and the thing a neuter removes.
    #[test]
    fn a_setting_comes_from_the_file_when_the_environment_does_not_have_it() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sync.env");
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&file)
                .unwrap();
            writeln!(f, "NEMR_TEST_ONLY_SETTING=from-the-file").unwrap();
        }
        std::env::set_var("NEMR_SYNC_ENV_FILE", &file);
        std::env::remove_var("NEMR_TEST_ONLY_SETTING");
        assert_eq!(
            setting("NEMR_TEST_ONLY_SETTING").unwrap(),
            "from-the-file",
            "a setting the environment does not carry must come from the file"
        );
        // And the environment still wins, key by key.
        std::env::set_var("NEMR_TEST_ONLY_SETTING", "from-the-environment");
        assert_eq!(
            setting("NEMR_TEST_ONLY_SETTING").unwrap(),
            "from-the-environment"
        );
        // A key neither has is a refusal that names the key AND the file.
        std::env::remove_var("NEMR_TEST_ONLY_SETTING");
        let err = setting("NEMR_TEST_ABSENT_SETTING").unwrap_err().to_string();
        assert!(err.contains("NEMR_TEST_ABSENT_SETTING"), "{err}");
        assert!(err.contains(&file.display().to_string()), "{err}");
        std::env::remove_var("NEMR_SYNC_ENV_FILE");
    }

    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    /// E-20: exactly one backend, no default; both or neither refused
    /// naming both; empty means unset; half-configured S3 is S3's refusal.
    #[test]
    fn storage_is_exactly_one_backend_and_never_a_default() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_str().unwrap();
        let s3 = [
            ("NEMR_S3_PROVIDER", "r2"),
            ("NEMR_S3_BUCKET", "b"),
            ("NEMR_S3_ENDPOINT", "https://x"),
            ("NEMR_S3_ACCESS_KEY_ID", "k"),
            ("NEMR_S3_SECRET_ACCESS_KEY", "s"),
        ];
        assert_eq!(
            select_storage(&env(&[("NEMR_BUNDLE_DIR", d)])).unwrap(),
            StorageChoice::Local(dir.path().to_path_buf())
        );
        assert_eq!(select_storage(&env(&s3)).unwrap(), StorageChoice::S3);
        let e = select_storage(&env(&[])).unwrap_err().to_string();
        assert!(
            e.contains("NEMR_BUNDLE_DIR") && e.contains("NEMR_S3_BUCKET"),
            "{e}"
        );
        let mut both = s3.to_vec();
        both.push(("NEMR_BUNDLE_DIR", d));
        let e = select_storage(&env(&both)).unwrap_err().to_string();
        assert!(e.contains("both") && e.contains("exactly one"), "{e}");
        // Empty means unset.
        let mut emptied = s3.to_vec();
        emptied.push(("NEMR_BUNDLE_DIR", ""));
        assert_eq!(select_storage(&env(&emptied)).unwrap(), StorageChoice::S3);
        // Half-configured S3 with a directory beside it is still refused as
        // "both", and alone it names what is missing — never a directory.
        let half = [("NEMR_S3_BUCKET", "b"), ("NEMR_S3_PROVIDER", "r2")];
        let e = select_storage(&env(&half)).unwrap_err().to_string();
        assert!(
            e.contains("half-configured") && e.contains("NEMR_S3_ENDPOINT"),
            "{e}"
        );
        let mut half_plus_dir = half.to_vec();
        half_plus_dir.push(("NEMR_BUNDLE_DIR", d));
        assert!(select_storage(&env(&half_plus_dir))
            .unwrap_err()
            .to_string()
            .contains("both"));
        // A typo'd provider is refused, not a generic endpoint.
        let mut typo = s3.to_vec();
        typo[0] = ("NEMR_S3_PROVIDER", "r3");
        let e = select_storage(&env(&typo)).unwrap_err().to_string();
        assert!(e.contains("r3") && e.contains("not one of"), "{e}");
        // The directory must exist.
        let e = select_storage(&env(&[("NEMR_BUNDLE_DIR", "/nonexistent/nemr-x")]))
            .unwrap_err()
            .to_string();
        assert!(e.contains("not an existing directory"), "{e}");
    }

    /// E-19: the file's lines, the environment winning, a malformed line
    /// named by number and never by content, a wide mode refused.
    #[test]
    fn the_env_file_is_merged_under_the_environment_and_refused_when_wide() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ENV_FILE_NAME);
        std::fs::write(
            &path,
            "# comment\nNEMR_AUTH_PEPPER=from-file\nexport NEMR_SERVER_ADDR=\"127.0.0.1:1\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let p = path.to_str().unwrap().to_string();
        let pairs = [
            ("NEMR_SYNC_ENV_FILE", p.as_str()),
            ("NEMR_AUTH_PEPPER", "from-env"),
        ];
        let e = env(&pairs);
        let s = Settings::load(&e).unwrap();
        assert_eq!(
            s.get("NEMR_AUTH_PEPPER", &e).as_deref(),
            Some("from-env"),
            "the environment wins"
        );
        assert_eq!(
            s.get("NEMR_SERVER_ADDR", &e).as_deref(),
            Some("127.0.0.1:1"),
            "quotes stripped"
        );
        assert_eq!(s.from_file, vec!["NEMR_SERVER_ADDR".to_string()]);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = Settings::load(&e).unwrap_err().to_string();
        assert!(err.contains("0644") && err.contains("0600"), "{err}");
        assert!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777 == 0o644,
            "refused, not repaired"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, "NEMR_AUTH_PEPPER=ok\nthis line holds a s3cr3t\n").unwrap();
        let err = Settings::load(&e).unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
        assert!(
            !err.contains("s3cr3t"),
            "the line's content must never appear: {err}"
        );

        // An explicitly empty NEMR_SYNC_ENV_FILE reads nothing.
        let none = env(&[("NEMR_SYNC_ENV_FILE", "")]);
        assert!(Settings::load(&none).unwrap().file.is_none());
    }

    /// E-19's ruling: no pepper refuses and names the file and the line;
    /// `ephemeral` is the loud escape hatch; anything else is hashed.
    #[test]
    fn a_missing_pepper_refuses_naming_the_line_and_ephemeral_is_the_hatch() {
        let none = env(&[("NEMR_SYNC_ENV_FILE", "")]);
        let s = Settings::load(&none).unwrap();
        let err = pepper(&s, &none).unwrap_err().to_string();
        assert!(
            err.contains("NEMR_AUTH_PEPPER=<any long random string>"),
            "{err}"
        );
        assert!(err.contains("sync.env"), "{err}");
        let eph = env(&[
            ("NEMR_SYNC_ENV_FILE", ""),
            ("NEMR_AUTH_PEPPER", "ephemeral"),
        ]);
        assert!(matches!(pepper(&s, &eph).unwrap(), Pepper::Ephemeral));
        let real = env(&[("NEMR_SYNC_ENV_FILE", ""), ("NEMR_AUTH_PEPPER", "x")]);
        let a = match pepper(&s, &real).unwrap() {
            Pepper::Configured(p) => p,
            _ => panic!(),
        };
        let b = match pepper(&s, &real).unwrap() {
            Pepper::Configured(p) => p,
            _ => panic!(),
        };
        assert_eq!(a, b, "a configured pepper is stable");
    }
}
