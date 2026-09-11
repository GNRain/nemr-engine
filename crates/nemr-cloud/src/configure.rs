//! `nemr server configure` — the interactive way to write `sync.env`.
//!
//! WHY IT EXISTS. Everything a server needs lives in one 0600 file, and until
//! now that file was written by hand from a template. This asks four
//! questions, checks the answers against the real database and the real store,
//! and writes the file — including generating the pepper, which a person
//! should never have to invent.
//!
//! WHAT IT WILL NOT DO. Overwrite a file that is already there; write anything
//! before the database and the store have answered; print a secret, in the
//! confirmation or anywhere else; or ask for a pepper.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use nemr_style::{Tone, Voice};

use crate::server;

/// What an existing settings file does NOT have. Taken from the server's own
/// preflight rather than by parsing the file here: there is one reader of that
/// file and it is the server.
struct Missing {
    storage: bool,
    database: bool,
    pepper: bool,
}

impl Missing {
    fn of(report: &server::Preflight) -> Missing {
        Missing {
            storage: report.state("backend_state") == "unconfigured",
            database: report.state("database_state") == "unconfigured",
            pepper: report.state("pepper") == "missing",
        }
    }
    fn none(&self) -> bool {
        !self.storage && !self.database && !self.pepper
    }
    fn names(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.storage {
            v.push("the storage backend");
        }
        if self.database {
            v.push("the database");
        }
        if self.pepper {
            v.push("the auth pepper");
        }
        v
    }
}

/// Add only what is missing, by appending. Existing lines are never read back
/// out, never rewritten and never reordered — the file may hold a pepper this
/// command has no business touching.
fn add_missing(v: &Voice, path: &Path, missing: &Missing) -> Result<()> {
    println!("{}", v.paint(Tone::Dim, "nemr server configure"));
    println!(
        "{}",
        v.warned(&format!(
            "{} exists, but it is missing {}.",
            path.display(),
            missing.names().join(" and ")
        ))
    );
    println!(
        "{}",
        v.note("Nothing already in it will be changed — this only adds what is absent.")
    );
    println!();

    let mut add = String::new();
    let mut storage = None;
    if missing.storage {
        storage = Some(ask_storage(v)?);
    }
    if missing.database {
        println!();
        let db = prompt(
            v,
            "Postgres connection",
            "postgres://nemr:nemr@127.0.0.1:5433/nemr",
        )?;
        add.push_str("\n# Postgres. nemr does not install or start one.\n");
        add.push_str(&format!("DATABASE_URL={db}\n"));
    }
    if let Some(s) = &storage {
        add.push_str(&storage_lines(s));
    }
    if missing.pepper {
        add.push_str(&pepper_lines());
    }

    // Checked before it is added, against a candidate that is the real file
    // plus the new lines — so what is verified is what will be there.
    println!();
    println!(
        "{}",
        v.paint(Tone::Dim, "Checking, before writing anything:")
    );
    let current =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let probe = write_private_temp(path, &format!("{current}{add}"))?;
    let report = match server::preflight_with(Some(probe.as_path())) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&probe);
            return Err(e);
        }
    };
    if !report_ok(v, &report) {
        let _ = std::fs::remove_file(&probe);
        eprint!(
            "{}",
            v.refusal(
                "Nothing was added.",
                "one of the things the server needs did not answer, and the file is \
                 left exactly as it was",
                "fix the one marked above, then run `nemr server configure` again",
            )
        );
        std::process::exit(1);
    }

    println!();
    println!(
        "{}",
        v.paint(Tone::Dim, &format!("Adding to {}:", path.display()))
    );
    for line in redacted(&add) {
        println!("  {}", v.paint(Tone::Dim, &line));
    }
    // Appended through the checked candidate, so the file on disk is the file
    // that answered — and the original is only replaced once that is true.
    std::fs::rename(&probe, path)
        .with_context(|| format!("moving the settings into place at {}", path.display()))?;
    println!();
    println!("{}", v.done(&format!("{} updated.", path.display())));
    println!("{}", v.field("Start it", "nemr server start", Tone::Good));
    Ok(())
}

/// What the questions produced, in the order the file is written.
struct Answers {
    addr: String,
    database: String,
    storage: Storage,
}

enum Storage {
    /// A folder on this machine, or a mounted network drive.
    Directory(String),
    /// Anything that speaks S3: a box on your own network, or a cloud.
    Objects {
        provider: String,
        bucket: String,
        endpoint: String,
        key_id: String,
        secret: String,
    },
}

pub fn run() -> Result<()> {
    let v = Voice::for_stdout();
    let path = env_file_path()?;

    if !std::io::stdin().is_terminal() {
        eprint!(
            "{}",
            v.refusal(
                "nemr server configure needs a terminal: it asks questions.",
                "stdin is not a terminal, so there is nobody to answer them",
                "run it in a terminal, or write the file yourself — `nemr server start` \
                 prints the template when there is nothing to start from",
            )
        );
        std::process::exit(2);
    }

    // A FILE THAT EXISTS IS NEVER OVERWRITTEN. But "it exists" and "it is
    // complete" are different things, and the common case is the second: a
    // file that has the database and the pepper because `install_server.sh`
    // wrote them, and no storage because the credentials were in a second file
    // the Product Owner sourced by hand every session. Refusing there would
    // leave the one job undone, so this ADDS what is missing and touches no
    // line that is already in the file.
    if path.exists() {
        let existing = server::preflight_with(Some(path.as_path()))?;
        let missing = Missing::of(&existing);
        if missing.none() {
            eprint!(
                "{}",
                v.refusal(
                    &format!("{} already has everything a server needs.", path.display()),
                    "this command will not write over settings you may be running a server on",
                    &format!(
                        "edit it, or move it aside and start again:  mv {} {}.old",
                        path.display(),
                        path.display()
                    ),
                )
            );
            std::process::exit(1);
        }
        return add_missing(&v, &path, &missing);
    }

    println!("{}", v.paint(Tone::Dim, "nemr server configure"));
    println!(
        "{}",
        v.paint(
            Tone::Dim,
            "  Four questions, then it checks the answers and writes the file.",
        )
    );
    println!(
        "{}",
        v.paint(
            Tone::Dim,
            "  Press enter to take the default in [brackets]."
        )
    );
    println!();

    let answers = ask(&v)?;

    // CHECKED BEFORE IT IS WRITTEN, by the server's own preflight, so a file
    // this command wrote is a file that started a server.
    println!();
    println!(
        "{}",
        v.paint(Tone::Dim, "Checking, before writing anything:")
    );
    let body = compose(&answers);
    let probe = write_private_temp(&path, &body)?;
    let report = server::preflight_with(Some(probe.as_path()));
    let report = match report {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&probe);
            return Err(e);
        }
    };
    if !report_ok(&v, &report) {
        let _ = std::fs::remove_file(&probe);
        eprint!(
            "{}",
            v.refusal(
                "Nothing was written.",
                "one of the things the server needs did not answer, and a settings file \
                 that cannot start a server is worse than none",
                "fix the one marked above, then run `nemr server configure` again",
            )
        );
        std::process::exit(1);
    }

    // WHAT IT WILL WRITE, shown before it is written — with the secrets left
    // out, because this is a terminal and terminals are read over shoulders
    // and pasted into issues.
    println!();
    println!(
        "{}",
        v.paint(Tone::Dim, &format!("Writing {}:", path.display()))
    );
    for line in redacted(&body) {
        println!("  {}", v.paint(Tone::Dim, &line));
    }

    std::fs::rename(&probe, &path)
        .with_context(|| format!("moving the settings into place at {}", path.display()))?;

    println!();
    println!("{}", v.done(&format!("{} written.", path.display())));
    println!(
        "{}",
        v.field(
            "mode",
            "0600 — it holds the pepper and the storage credential",
            Tone::Dim
        )
    );
    println!();
    println!("{}", v.field("Start it", "nemr server start", Tone::Good));
    Ok(())
}

fn ask(v: &Voice) -> Result<Answers> {
    let port = prompt(v, "Port to listen on", "8080")?;
    let addr = if port.contains(':') {
        port
    } else {
        format!("127.0.0.1:{port}")
    };

    let storage = ask_storage(v)?;

    println!();
    let database = prompt(
        v,
        "Postgres connection",
        "postgres://nemr:nemr@127.0.0.1:5433/nemr",
    )?;

    Ok(Answers {
        addr,
        database,
        storage,
    })
}

/// The storage question, asked on its own so that a settings file missing only
/// its storage can be completed without re-asking anything else.
fn ask_storage(v: &Voice) -> Result<Storage> {
    println!();
    println!(
        "{}",
        v.paint(Tone::Dim, "Where do the sessions get stored?")
    );
    println!("    1  a folder on this machine, or a mounted network drive");
    println!("    2  object storage on your own network (MinIO, Garage, anything S3)");
    println!("    3  cloud object storage (Cloudflare R2, Backblaze B2, Amazon S3)");
    let choice = prompt(v, "Choice", "1")?;
    Ok(match choice.as_str() {
        "1" => {
            let default = default_bundle_dir();
            let dir = prompt(v, "Folder", &default)?;
            let p = PathBuf::from(&dir);
            if !p.is_dir() {
                std::fs::create_dir_all(&p).with_context(|| format!("creating {}", p.display()))?;
                println!("{}", v.field("created", &dir, Tone::Dim));
            }
            Storage::Directory(dir)
        }
        "2" | "3" => {
            let (provider, endpoint) = if choice == "2" {
                // Self-hosted: the same S3 backend, pointed somewhere else.
                // `s3` is the generic provider, and the endpoint is the box.
                let ep = prompt(v, "Endpoint URL", "http://127.0.0.1:9000")?;
                ("s3".to_string(), ep)
            } else {
                println!(
                    "{}",
                    v.paint(
                        Tone::Dim,
                        "    r2  Cloudflare    b2  Backblaze    s3  Amazon"
                    )
                );
                let p = prompt(v, "Provider", "r2")?;
                let ep = prompt(v, "Endpoint URL", "")?;
                (p, ep)
            };
            let bucket = prompt(v, "Bucket", "nemr")?;
            let key_id = prompt(v, "Access key ID", "")?;
            let secret = prompt_secret(v, "Secret access key")?;
            if endpoint.is_empty() || key_id.is_empty() || secret.is_empty() {
                bail!("the endpoint, the access key id and the secret are all required for object storage");
            }
            Storage::Objects {
                provider,
                bucket,
                endpoint,
                key_id,
                secret,
            }
        }
        other => bail!("{other:?} is not one of 1, 2 or 3"),
    })
}

/// The file, as it will be written. The pepper is generated HERE: E-19 says a
/// server without one refuses to bind, and nobody should be asked to invent a
/// random string when a machine is standing right there.
fn compose(a: &Answers) -> String {
    let mut s = String::new();
    s.push_str(
        "# nemr sync server settings.\n\
         #\n\
         # THIS IS A SECRETS FILE. It holds the authentication pepper and, if you use\n\
         # object storage, the credential for it. Mode 0600 — the server refuses to read\n\
         # it if it is any wider. Do not commit it, do not paste it, do not copy it to\n\
         # another machine and expect the pepper to stay a secret.\n\
         #\n\
         # Written by `nemr server configure`. Everything a server needs is here, so\n\
         # `nemr server start` needs nothing exported.\n\n",
    );
    s.push_str("# Where the server listens.\n");
    s.push_str(&format!("NEMR_SERVER_ADDR={}\n\n", a.addr));
    s.push_str("# Postgres. nemr does not install or start one.\n");
    s.push_str(&format!("DATABASE_URL={}\n", a.database));
    s.push_str(&storage_lines(&a.storage));
    s.push_str(&pepper_lines());
    s
}

/// The storage settings as file lines. Shared by the file this command writes
/// and by the lines it appends to a file that was missing them.
fn storage_lines(storage: &Storage) -> String {
    let mut s = String::new();
    match storage {
        Storage::Directory(dir) => {
            s.push_str(
                "\n# Storage: a folder. It must exist; the server reads it, never creates it.\n",
            );
            s.push_str(&format!("NEMR_BUNDLE_DIR={dir}\n"));
        }
        Storage::Objects {
            provider,
            bucket,
            endpoint,
            key_id,
            secret,
        } => {
            s.push_str("\n# Storage: an object store. All five, or none (E-20).\n");
            s.push_str(&format!("NEMR_S3_PROVIDER={provider}\n"));
            s.push_str(&format!("NEMR_S3_BUCKET={bucket}\n"));
            s.push_str(&format!("NEMR_S3_ENDPOINT={endpoint}\n"));
            s.push_str(&format!("NEMR_S3_ACCESS_KEY_ID={key_id}\n"));
            s.push_str(&format!("NEMR_S3_SECRET_ACCESS_KEY={secret}\n"));
        }
    }
    s
}

/// The pepper, generated HERE: E-19 says a server without one refuses to bind,
/// and nobody should be asked to invent a random string when a machine is
/// standing right there.
fn pepper_lines() -> String {
    format!(
        "\n# The authentication pepper, generated for this server. It makes the\n\
         # account-lookup endpoint answer the same way for an unknown email every time\n\
         # (F-89). Changing it is safe; losing it only costs that property.\n\
         NEMR_AUTH_PEPPER={}\n",
        generate_pepper()
    )
}

/// What the preflight said, in the same two lines whether this is a new file
/// or an old one being completed. True when the server would start.
fn report_ok(v: &Voice, report: &server::Preflight) -> bool {
    let db_ok = report.state("database_state") == "ok";
    let store_ok = report.state("backend_state") == "ok";
    println!(
        "{}",
        v.field(
            "database",
            if db_ok { "answers" } else { "no answer" },
            if db_ok { Tone::Good } else { Tone::Bad }
        )
    );
    if !db_ok {
        println!("{}", v.field("", report.state("database_error"), Tone::Dim));
    }
    println!(
        "{}",
        v.field(
            "storage",
            if store_ok { "answers" } else { "no answer" },
            if store_ok { Tone::Good } else { Tone::Bad }
        )
    );
    if !store_ok {
        println!("{}", v.field("", report.state("backend_error"), Tone::Dim));
    }
    db_ok && store_ok
}

/// The confirmation, with every secret left out. What is shown is the shape of
/// the file, not its contents.
fn redacted(body: &str) -> Vec<String> {
    let secret_keys = [
        "NEMR_AUTH_PEPPER",
        "NEMR_S3_SECRET_ACCESS_KEY",
        "NEMR_S3_ACCESS_KEY_ID",
        // THE ENDPOINT IS A SECRET TOO, for the provider most people use: an
        // R2 endpoint is https://<account id>.r2.cloudflarestorage.com, so
        // printing it prints the account id. It is redacted for every
        // provider rather than guessed at per provider — a rule you have to
        // reason about is a rule that leaks the day the reasoning is wrong.
        "NEMR_S3_ENDPOINT",
    ];
    let mut out = Vec::new();
    for line in body.lines() {
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let shown = match line.split_once('=') {
            Some((k, _)) if secret_keys.contains(&k) => {
                // Accurate about WHY it is hidden: the pepper is the machine's
                // own, the rest are the operator's and were typed in.
                let why = if k == "NEMR_AUTH_PEPPER" {
                    "generated, not shown"
                } else {
                    "not shown"
                };
                format!("{k}=<{why}>")
            }
            // The database URL can carry a password, and does by default.
            Some((k, v)) if k == "DATABASE_URL" => format!("{k}={}", redact_url(v)),
            _ => line.to_string(),
        };
        out.push(shown);
    }
    out
}

fn redact_url(s: &str) -> String {
    let Some(scheme_end) = s.find("://") else {
        return s.to_string();
    };
    let rest = &s[scheme_end + 3..];
    let Some(at) = rest.find('@') else {
        return s.to_string();
    };
    let userinfo = &rest[..at];
    match userinfo.find(':') {
        Some(colon) => format!(
            "{}{}:***@{}",
            &s[..scheme_end + 3],
            &userinfo[..colon],
            &rest[at + 1..]
        ),
        None => s.to_string(),
    }
}

/// 32 bytes of randomness, base64. From the kernel, not from a crate's idea of
/// a seed: this is the one value in the file that has to be unguessable.
fn generate_pepper() -> String {
    use std::io::Read;
    let mut buf = [0u8; 32];
    let mut f = std::fs::File::open("/dev/urandom").expect("/dev/urandom");
    f.read_exact(&mut buf).expect("reading /dev/urandom");
    base64_standard(&buf)
}

fn base64_standard(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn default_bundle_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/var/lib".into());
    format!("{home}/.local/share/nemr/bundles")
}

fn env_file_path() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("NEMR_SYNC_ENV_FILE") {
        let p = PathBuf::from(&explicit);
        if p.as_os_str().is_empty() {
            bail!("NEMR_SYNC_ENV_FILE is empty, which means \"read no file\" — unset it to configure one");
        }
        return Ok(p);
    }
    if let Some(cfg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return Ok(Path::new(&cfg).join("nemr").join("sync.env"));
    }
    let home = std::env::var("HOME").context("HOME is not set, so there is nowhere to write")?;
    Ok(Path::new(&home)
        .join(".config")
        .join("nemr")
        .join("sync.env"))
}

/// Write the candidate beside its destination, 0600 from the moment it exists.
fn write_private_temp(dest: &Path, body: &str) -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = dest
        .parent()
        .context("the settings path has no directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!("sync.env.configure.{}", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(body.as_bytes())?;
    f.flush()?;
    Ok(tmp)
}

fn prompt(v: &Voice, question: &str, default: &str) -> Result<String> {
    let shown = if default.is_empty() {
        format!("{question}: ")
    } else {
        format!("{question} [{}]: ", v.paint(Tone::Dim, default))
    };
    print!("  {shown}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim().to_string();
    Ok(if line.is_empty() {
        default.to_string()
    } else {
        line
    })
}

fn prompt_secret(v: &Voice, question: &str) -> Result<String> {
    let _ = v;
    let s = rpassword::prompt_password(format!("  {question} (not shown): "))?;
    Ok(s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers() -> Answers {
        Answers {
            addr: "127.0.0.1:8080".into(),
            database: "postgres://nemr:hunter2@127.0.0.1:5433/nemr".into(),
            storage: Storage::Objects {
                provider: "s3".into(),
                bucket: "b".into(),
                endpoint: "http://box:9000".into(),
                key_id: "AKIAEXAMPLE".into(),
                secret: "s3cr3t-value".into(),
            },
        }
    }

    #[test]
    fn the_confirmation_shows_no_secret() {
        let body = compose(&answers());
        let shown = redacted(&body).join("\n");
        assert!(!shown.contains("s3cr3t-value"), "{shown}");
        assert!(!shown.contains("AKIAEXAMPLE"), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
        // AND THE ENDPOINT. For the provider most people use it reads
        // https://<account id>.r2.cloudflarestorage.com, so showing it shows
        // the account id.
        assert!(
            !shown.contains("box:9000"),
            "the endpoint is in the confirmation: {shown}"
        );
        // And the pepper, which is generated, never appears either.
        let pepper = body
            .lines()
            .find_map(|l| l.strip_prefix("NEMR_AUTH_PEPPER="))
            .unwrap();
        assert!(!shown.contains(pepper), "the pepper is in the confirmation");
        // What it DOES show: the shape.
        assert!(shown.contains("NEMR_S3_BUCKET=b"));
        assert!(shown.contains("NEMR_SERVER_ADDR=127.0.0.1:8080"));
    }

    #[test]
    fn a_file_missing_only_its_storage_is_completed_not_refused() {
        // The shape `install_server.sh` leaves behind: a database and a pepper
        // and no storage, because the credentials lived in a second file.
        let half = server::Preflight::from_pairs(&[
            ("database_state", "ok"),
            ("backend_state", "unconfigured"),
            ("pepper", "configured"),
        ]);
        let m = Missing::of(&half);
        assert!(m.storage && !m.database && !m.pepper);
        assert!(!m.none(), "there is something to add");
        assert_eq!(m.names(), vec!["the storage backend"]);

        let whole = server::Preflight::from_pairs(&[
            ("database_state", "ok"),
            ("backend_state", "ok"),
            ("pepper", "configured"),
        ]);
        assert!(Missing::of(&whole).none(), "a complete file is left alone");
    }

    #[test]
    fn what_is_added_carries_its_own_blank_line() {
        // Appended to a file whose last line has a newline and nothing else,
        // so each block must open with one — otherwise the first added key
        // lands on the end of somebody's existing line.
        let lines = storage_lines(&Storage::Directory("/tmp/x".into()));
        assert!(lines.starts_with('\n'), "{lines:?}");
        assert!(pepper_lines().starts_with('\n'));
        assert!(lines.ends_with('\n'), "{lines:?}");
    }

    #[test]
    fn the_file_says_it_is_a_secrets_file() {
        let body = compose(&answers());
        assert!(body.contains("THIS IS A SECRETS FILE"));
        assert!(body.contains("0600"));
    }

    #[test]
    fn a_pepper_is_generated_and_never_asked_for() {
        let a = compose(&answers());
        let b = compose(&answers());
        let p = |s: &str| {
            s.lines()
                .find_map(|l| l.strip_prefix("NEMR_AUTH_PEPPER="))
                .unwrap()
                .to_string()
        };
        assert_ne!(p(&a), p(&b), "two runs must not share a pepper");
        assert!(p(&a).len() >= 40, "32 bytes of base64: {}", p(&a));
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64_standard(b""), "");
        assert_eq!(base64_standard(b"f"), "Zg==");
        assert_eq!(base64_standard(b"fo"), "Zm8=");
        assert_eq!(base64_standard(b"foo"), "Zm9v");
        assert_eq!(base64_standard(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn exactly_one_backend_is_written() {
        let dir = Answers {
            storage: Storage::Directory("/tmp/x".into()),
            ..answers()
        };
        let body = compose(&dir);
        assert!(body.contains("NEMR_BUNDLE_DIR=/tmp/x"));
        assert!(
            !body.contains("NEMR_S3_"),
            "both backends would be refused (E-20)"
        );
        let obj = compose(&answers());
        assert!(
            !obj.contains("NEMR_BUNDLE_DIR"),
            "both backends would be refused (E-20)"
        );
    }
}
