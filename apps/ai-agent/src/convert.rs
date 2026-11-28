//! Documents as pages for the viewer: Word, spreadsheet, slides, OpenDocument
//! and RTF files become PDFs that the window shows like any PDF
//! (`POST /api/ai/view/convert?name=`, `GET /api/ai/view/converted/{id}`).
//!
//! * LibreOffice on this server converts them, in a systemd unit of its own
//!   started by the root terminal helper (as the agent's browser is): as
//!   `nobody`, without any network, home folders and RAST's data hidden, a
//!   private `/tmp`, 1 GiB of memory and two minutes.
//! * The document goes in on the unit's standard input and the PDF comes
//!   out on its standard output (files systemd opens for it), so the unit
//!   sees no file of the server.
//! * A PDF is kept per account, named by the document's SHA-256, for a
//!   week: opening the same document again is instant.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, SystemTime},
};

use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::{auth::AuthUser, guard, host::Agent, terminal::TerminalError};

/// The largest document taken.
pub const MAX_DOCUMENT_BYTES: u64 = crate::view::MAX_DOCUMENT_BYTES;
/// How long a converted document is kept.
const KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How long one conversion may take.
const RUN_SECS: u32 = 120;
/// Where Ubuntu's LibreOffice keeps what each part brings.
const REGISTRY: &str = "/usr/lib/libreoffice/share/registry";

/// Conversions at a time (each one is a whole LibreOffice).
static RUNNING: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(2));

/// The LibreOffice part a document needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Text,
    Sheet,
    Slides,
}

impl Family {
    /// A document's family by its extension.
    pub fn of(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "docx" | "docm" | "dotx" | "doc" | "dot" | "odt" | "ott" | "rtf" => Some(Self::Text),
            "xlsx" | "xlsm" | "xls" | "ods" | "ots" => Some(Self::Sheet),
            "pptx" | "pptm" | "ppsx" | "ppt" | "pps" | "odp" | "otp" => Some(Self::Slides),
            _ => None,
        }
    }

    /// The package that adds this part.
    fn package(self) -> &'static str {
        match self {
            Self::Text => "libreoffice-writer-nogui",
            Self::Sheet => "libreoffice-calc-nogui",
            Self::Slides => "libreoffice-impress-nogui",
        }
    }

    fn registry(self) -> &'static str {
        match self {
            Self::Text => "writer.xcd",
            Self::Sheet => "calc.xcd",
            Self::Slides => "impress.xcd",
        }
    }

    fn what(self) -> &'static str {
        match self {
            Self::Text => "text documents",
            Self::Sheet => "spreadsheets",
            Self::Slides => "slides",
        }
    }
}

/// The extension of a file name, when it is one the converter knows.
pub fn extension_of(name: &str) -> Option<String> {
    let last = name.rsplit('/').next().unwrap_or(name);
    let (stem, extension) = last.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    let extension = extension.to_ascii_lowercase();
    Family::of(&extension).map(|_| extension)
}

/// Whether the bytes look like a document of that extension: a ZIP (Office
/// Open XML, OpenDocument), an OLE file (older Office) or RTF.
pub fn looks_like(extension: &str, bytes: &[u8]) -> bool {
    const ZIP: &[u8] = b"PK\x03\x04";
    const OLE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    match extension {
        "doc" | "dot" | "xls" | "ppt" | "pps" => bytes.starts_with(OLE),
        "rtf" => bytes.starts_with(b"{\\rtf"),
        _ => bytes.starts_with(ZIP),
    }
}

/// A converted document's file, when `id` is a SHA-256 in hex.
pub fn converted_path(data_dir: &Path, user_id: i64, id: &str) -> Option<PathBuf> {
    let valid = id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    valid.then(|| {
        data_dir
            .join("converted")
            .join(user_id.to_string())
            .join(format!("{id}.pdf"))
    })
}

/// Turn a document into a PDF (or find it done already) → its id.
pub async fn to_pdf(
    agent: &Agent,
    user: &AuthUser,
    name: &str,
    bytes: Vec<u8>,
) -> Result<String, String> {
    let extension = extension_of(name).ok_or_else(|| {
        "The panel turns Word, spreadsheet, slides, OpenDocument and RTF files into pages"
            .to_string()
    })?;
    let family = Family::of(&extension).expect("a known extension");
    if !looks_like(&extension, &bytes) {
        return Err(format!("{name} has no .{extension} document inside"));
    }
    let id = hex::encode(Sha256::digest(&bytes));
    let target = converted_path(agent.data_dir(), user.id, &id).expect("a SHA-256 id");
    if target.exists() {
        // Kept for another week from now.
        if let Ok(file) = std::fs::File::options().append(true).open(&target) {
            let _ = file.set_modified(SystemTime::now());
        }
        return Ok(id);
    }
    if !soffice_installed() {
        return Err(
            "Showing documents as pages needs LibreOffice on this server (an administrator can install it: sudo apt install libreoffice-writer-nogui libreoffice-calc-nogui libreoffice-impress-nogui)"
                .into(),
        );
    }
    if !Path::new(REGISTRY).join(family.registry()).exists() {
        return Err(format!(
            "This server's LibreOffice opens no {} yet (an administrator can add it: sudo apt install {})",
            family.what(),
            family.package()
        ));
    }
    let _turn = RUNNING
        .acquire()
        .await
        .map_err(|_| "The converter is stopping".to_string())?;
    let jobs = agent.data_dir().join("convert");
    private_dir(&jobs)?;
    let job = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
    let input = JobFile(jobs.join(format!("{job}.in")));
    let output = JobFile(jobs.join(format!("{job}.out")));
    write_private(&input.0, &bytes)?;
    write_private(&output.0, &[])?;
    let script = convert_script(
        &extension,
        &format!("rast-convert-{job}"),
        &input.0,
        &output.0,
        &guard::hidden_paths(agent.host.private_paths()),
    )?;
    let ran = agent
        .host
        .terminal_run(user, &script, RUN_SECS + 20)
        .await
        .map_err(|e| match e {
            TerminalError::Off => {
                "Showing documents as pages needs Terminal turned on on this server (sudo rast terminal enable)".to_string()
            }
            TerminalError::Io(error) => format!(
                "Showing documents as pages needs RAST's terminal helper, which this app could not reach ({error})"
            ),
            TerminalError::Refused { message, .. } => {
                format!("The terminal helper turned the converter away: {message}")
            }
        })?;
    let pdf = std::fs::read(&output.0).unwrap_or_default();
    if !pdf.starts_with(b"%PDF") {
        let said = if ran.exit_code == Some(0) {
            String::new()
        } else {
            let said = if ran.stderr.trim().is_empty() {
                ran.stdout.trim()
            } else {
                ran.stderr.trim()
            };
            let said = said.lines().last().unwrap_or("").trim();
            if said.is_empty() {
                String::new()
            } else {
                format!(" ({said})")
            }
        };
        return Err(format!("LibreOffice could not open {name}{said}"));
    }
    let dir = target.parent().expect("an account folder");
    private_dir(&dir.parent().expect("the converted folder").to_path_buf())?;
    private_dir(&dir.to_path_buf())?;
    let partial = dir.join(format!("{id}.{job}.part"));
    write_private(&partial, &pdf)?;
    std::fs::rename(&partial, &target).map_err(|e| format!("The pages could not be kept: {e}"))?;
    Ok(id)
}

/// The root script that converts one document (see the module docs).
fn convert_script(
    extension: &str,
    unit: &str,
    input: &Path,
    output: &Path,
    hidden: &[PathBuf],
) -> Result<String, String> {
    let plain = |path: &Path| {
        let text = path.to_string_lossy();
        !text.is_empty()
            && !text
                .chars()
                .any(|c| c == '"' || c == '\\' || c.is_control() || c.is_whitespace())
    };
    if !plain(input) || !plain(output) {
        return Err("RAST's data folder has an unusual name; documents show as files only".into());
    }
    if !extension
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err("an unusual document extension".into());
    }
    let mut args: Vec<String> = [
        "--quiet",
        "--wait",
        "--collect",
        "--service-type=exec",
        "--expand-environment=no",
        "--description=RAST AI Agent document pages",
        "--property=User=nobody",
        "--property=Group=nogroup",
        "--property=ProtectSystem=strict",
        "--property=ProtectHome=yes",
        "--property=PrivateTmp=yes",
        "--property=PrivateDevices=yes",
        "--property=PrivateNetwork=yes",
        "--property=PrivateIPC=yes",
        "--property=ProtectKernelTunables=yes",
        "--property=ProtectKernelModules=yes",
        "--property=ProtectKernelLogs=yes",
        "--property=ProtectControlGroups=yes",
        "--property=ProtectClock=yes",
        "--property=ProtectHostname=yes",
        "--property=ProtectProc=invisible",
        "--property=NoNewPrivileges=yes",
        "--property=RestrictSUIDSGID=yes",
        "--property=CapabilityBoundingSet=",
        "--property=LockPersonality=yes",
        "--property=RestrictRealtime=yes",
        "--property=RestrictNamespaces=yes",
        "--property=RestrictAddressFamilies=AF_UNIX",
        "--property=IPAddressDeny=any",
        "--property=MemoryMax=1G",
        "--property=TasksMax=256",
        "--property=CPUQuota=200%",
        "--property=TimeoutStopSec=5",
        "--property=WorkingDirectory=/tmp",
        "--property=Environment=HOME=/tmp LANG=C.UTF-8 SAL_USE_VCLPLUGIN=svp",
        "--property=StandardError=null",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(format!("--unit={unit}"));
    args.push(format!("--property=RuntimeMaxSec={RUN_SECS}"));
    args.push(format!(
        "--property=StandardInput=file:{}",
        input.to_string_lossy()
    ));
    args.push(format!(
        "--property=StandardOutput=file:{}",
        output.to_string_lossy()
    ));
    for path in hidden {
        let text = path.to_string_lossy();
        if text
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_control())
        {
            return Err(
                "a server folder RAST hides has an unusual name; documents show as files only"
                    .into(),
            );
        }
        args.push(format!("--property=InaccessiblePaths=\"-{text}\""));
    }
    // The document from standard input, the PDF to standard output; what
    // LibreOffice says goes nowhere.
    let inner = format!(
        "set -e; cat > /tmp/document.{extension}; mkdir /tmp/pages; \
         soffice --headless --norestore --nologo --nodefault --nolockcheck --nofirststartwizard \
         -env:UserInstallation=file:///tmp/profile --convert-to pdf --outdir /tmp/pages \
         /tmp/document.{extension} </dev/null >/dev/null 2>&1; \
         exec cat /tmp/pages/document.pdf"
    );
    args.extend(
        ["--", "/bin/bash", "--noprofile", "--norc", "-c"]
            .iter()
            .map(|s| s.to_string()),
    );
    args.push(inner);
    let quoted: Vec<String> = args.iter().map(|a| guard::shell_quote(a)).collect();
    Ok(format!("exec systemd-run {}", quoted.join(" ")))
}

fn soffice_installed() -> bool {
    ["/usr/bin/soffice", "/usr/lib/libreoffice/program/soffice"]
        .iter()
        .any(|p| Path::new(p).exists())
}

fn private_dir(dir: &PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)))
        .map_err(|e| format!("The converter's folder could not be made: {e}"))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("The converter's file could not be made: {e}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

/// A job's file, removed when the job ends.
struct JobFile(PathBuf);

impl Drop for JobFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Remove converted documents older than a week, and jobs a stop left.
pub fn sweep(data_dir: &Path) {
    let old = |path: &Path, age: Duration| {
        path.metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|elapsed| elapsed > age)
    };
    if let Ok(jobs) = std::fs::read_dir(data_dir.join("convert")) {
        for job in jobs.flatten() {
            if old(&job.path(), Duration::from_secs(60 * 60)) {
                let _ = std::fs::remove_file(job.path());
            }
        }
    }
    let Ok(accounts) = std::fs::read_dir(data_dir.join("converted")) else {
        return;
    };
    for account in accounts.flatten() {
        let Ok(files) = std::fs::read_dir(account.path()) else {
            continue;
        };
        for file in files.flatten() {
            if old(&file.path(), KEEP) {
                let _ = std::fs::remove_file(file.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_are_known_by_extension_and_contents() {
        assert_eq!(
            extension_of("/shared/team/Q3 Report.DOCX").as_deref(),
            Some("docx")
        );
        assert_eq!(extension_of("budget.xlsx").as_deref(), Some("xlsx"));
        assert_eq!(extension_of("deck.pptx").as_deref(), Some("pptx"));
        assert_eq!(extension_of("notes.txt"), None);
        assert_eq!(extension_of(".docx"), None);
        assert_eq!(extension_of("docx"), None);
        assert_eq!(Family::of("odt"), Some(Family::Text));
        assert_eq!(Family::of("ods"), Some(Family::Sheet));
        assert_eq!(Family::of("odp"), Some(Family::Slides));
        assert!(looks_like("docx", b"PK\x03\x04rest"));
        assert!(!looks_like("docx", b"<html>"));
        assert!(looks_like(
            "doc",
            &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0]
        ));
        assert!(!looks_like("doc", b"PK\x03\x04"));
        assert!(looks_like("rtf", b"{\\rtf1\\ansi"));
    }

    #[test]
    fn converted_documents_are_named_by_their_hash_only() {
        let dir = Path::new("/data");
        let id = "a".repeat(64);
        assert_eq!(
            converted_path(dir, 7, &id),
            Some(PathBuf::from(format!("/data/converted/7/{id}.pdf")))
        );
        for bad in [
            "../../etc/passwd",
            "",
            &"A".repeat(64),
            &"a".repeat(63),
            &format!("{}/", "a".repeat(63)),
        ] {
            assert_eq!(converted_path(dir, 7, bad), None, "{bad}");
        }
    }

    #[test]
    fn the_unit_has_no_network_and_sees_no_file_of_the_server() {
        let script = convert_script(
            "docx",
            "rast-convert-abc",
            Path::new("/var/lib/rast/apps/agent/data/convert/abc.in"),
            Path::new("/var/lib/rast/apps/agent/data/convert/abc.out"),
            &[PathBuf::from("/var/lib/rast"), PathBuf::from("/srv/rast")],
        )
        .unwrap();
        assert!(script.starts_with("exec systemd-run "));
        for needle in [
            "'--wait'",
            "'--property=User=nobody'",
            "'--property=PrivateNetwork=yes'",
            "'--property=RestrictAddressFamilies=AF_UNIX'",
            "'--property=ProtectHome=yes'",
            "'--property=InaccessiblePaths=\"-/var/lib/rast\"'",
            "'--property=InaccessiblePaths=\"-/srv/rast\"'",
            "'--property=StandardInput=file:/var/lib/rast/apps/agent/data/convert/abc.in'",
            "'--property=StandardOutput=file:/var/lib/rast/apps/agent/data/convert/abc.out'",
            "'--unit=rast-convert-abc'",
            "--convert-to pdf",
            "/tmp/document.docx",
        ] {
            assert!(script.contains(needle), "{needle} in {script}");
        }
        assert!(convert_script("docx", "u", Path::new("/a b/in"), Path::new("/out"), &[]).is_err());
        assert!(convert_script("do;cx", "u", Path::new("/in"), Path::new("/out"), &[]).is_err());
    }
}
