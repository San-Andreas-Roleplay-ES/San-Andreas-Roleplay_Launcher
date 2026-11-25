//! PowerShell helpers for running installers and for elevated copy/extract.
//!
//! `Start-Process` tolerates Mark-of-the-Web executables in `%TEMP%` better
//! than a direct CreateProcess, and with `-Verb RunAs` it raises UAC for the
//! child only — the launcher itself never runs elevated. The wrapper maps
//! every outcome to an exit code we control, so we never parse
//! locale-dependent stderr.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::install::fs::random_hex;

/// Win32 ERROR_CANCELLED: the user said "No" to UAC.
pub const EXIT_UAC_CANCELLED: i32 = 1223;
pub const EXIT_LAUNCH_FAILED: i32 = 1224;
pub const EXIT_NO_PROCESS: i32 = 1225;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What PowerShell takes for a single quote: `'` and the typographic ones
/// (U+2018 ‘, U+2019 ’, U+201A ‚, U+201B ‛). Any of them ends a single-quoted
/// literal unless doubled.
const SINGLE_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

/// A PowerShell single-quoted literal: every single-quote character doubled,
/// the same escaping PowerShell's own `EscapeSingleQuotedStringContent` does.
/// Nothing else is special inside single quotes, so any path round-trips
/// (`C:\Games\Dan’s` stays one literal).
pub fn quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for c in value.chars() {
        if SINGLE_QUOTES.contains(&c) {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// `Start-Process` wrapper that waits for the child and exits with its exit
/// code, or with one of our codes when it couldn't be started.
///
/// `args` are joined by `Start-Process` with spaces and *without* quoting —
/// exactly what NSIS wants for `/D=C:\Some Path`. Callers quote args that
/// need it themselves.
pub fn start_process(file: &str, args: &[String], cwd: Option<&Path>, elevate: bool) -> String {
    let arg_list = args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(",");
    let cwd = cwd
        .map(|dir| format!("-WorkingDirectory {} ", quote(&dir.to_string_lossy())))
        .unwrap_or_default();
    let verb = if elevate { "-Verb RunAs " } else { "" };
    format!(
        "try {{ \
           $proc = Start-Process -FilePath {file} -ArgumentList {arg_list} {cwd}\
         {verb}-Wait -PassThru -WindowStyle Hidden -ErrorAction Stop; \
           if ($null -eq $proc) {{ exit {EXIT_NO_PROCESS} }}; \
           exit $proc.ExitCode \
         }} catch [System.ComponentModel.Win32Exception] {{ \
           if ($_.Exception.NativeErrorCode -eq 1223) {{ exit {EXIT_UAC_CANCELLED} }} \
           else {{ exit {EXIT_LAUNCH_FAILED} }} \
         }} catch {{ exit {EXIT_LAUNCH_FAILED} }}",
        file = quote(file),
    )
}

/// Runs a `.ps1` in an elevated, hidden PowerShell.
pub fn run_script_elevated(script: &Path) -> String {
    let args: Vec<String> = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
        .into_iter()
        .map(String::from)
        // Quoted: Start-Process doesn't quote, and %TEMP% may contain spaces.
        .chain([format!("\"{}\"", script.display())])
        .collect();
    start_process("powershell.exe", &args, None, true)
}

/// Script body: copy every `(src, dst)` pair, overwriting.
pub fn copy_script(pairs: &[(PathBuf, PathBuf)]) -> String {
    let items = pairs
        .iter()
        .map(|(src, dst)| {
            format!(
                "  @{{Src={}; Dst={}}}",
                quote(&src.to_string_lossy()),
                quote(&dst.to_string_lossy())
            )
        })
        .collect::<Vec<_>>()
        .join(",\r\n");
    [
        "$ErrorActionPreference = 'Stop'".to_string(),
        "try {".into(),
        "  $items = @(".into(),
        items,
        "  )".into(),
        "  foreach ($i in $items) {".into(),
        "    Copy-Item -LiteralPath $i.Src -Destination $i.Dst -Force".into(),
        "  }".into(),
        "  exit 0".into(),
        "} catch {".into(),
        format!("  exit {EXIT_LAUNCH_FAILED}"),
        "}".into(),
    ]
    .join("\r\n")
}

/// Script body: `Expand-Archive` the zip into `dest`.
pub fn extract_script(zip: &Path, dest: &Path) -> String {
    [
        "$ErrorActionPreference = 'Stop'".to_string(),
        "try {".into(),
        format!(
            "  Expand-Archive -LiteralPath {} -DestinationPath {} -Force",
            quote(&zip.to_string_lossy()),
            quote(&dest.to_string_lossy())
        ),
        "  exit 0".into(),
        "} catch {".into(),
        format!("  exit {EXIT_LAUNCH_FAILED}"),
        "}".into(),
    ]
    .join("\r\n")
}

/// Writes a script to `%TEMP%\<prefix><random>.ps1`. UTF-8 *with* BOM:
/// Windows PowerShell 5.1 reads BOM-less scripts in the ANSI code page, which
/// would mangle non-ASCII paths (e.g. `C:\Users\José`).
pub async fn write_script(prefix: &str, body: &str) -> std::io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("{prefix}{}.ps1", random_hex(8)));
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(body.as_bytes());
    tokio::fs::write(&path, bytes).await?;
    Ok(path)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Exited(i32),
    TimedOut,
    /// powershell.exe itself couldn't be started.
    Failed(String),
}

/// Runs `command` in a hidden `powershell.exe -NoProfile -Command`.
pub async fn run(command: &str, timeout: Duration) -> Outcome {
    let mut child = match tokio::process::Command::new(powershell_exe())
        .args(["-NoProfile", "-Command", command])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    match tokio::time::timeout(timeout, child.wait()).await {
        Err(_) => {
            let _ = child.kill().await;
            Outcome::TimedOut
        }
        Ok(Err(error)) => Outcome::Failed(error.to_string()),
        Ok(Ok(status)) => Outcome::Exited(status.code().unwrap_or(EXIT_LAUNCH_FAILED)),
    }
}

/// Absolute path, so a stray `powershell.exe` next to us or on PATH can't be
/// picked up instead.
fn powershell_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let exe = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    if exe.is_file() {
        exe
    } else {
        PathBuf::from("powershell.exe")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_single_quotes() {
        assert_eq!(quote(r"C:\it's here"), r"'C:\it''s here'");
        assert_eq!(quote("C:\\Dan\u{2019}s"), "'C:\\Dan\u{2019}\u{2019}s'");
        assert_eq!(
            quote("\u{2018}\u{201A}\u{201B}"),
            "'\u{2018}\u{2018}\u{201A}\u{201A}\u{201B}\u{201B}'"
        );
        // Double quotes, `$` and backticks are plain text in single quotes.
        assert_eq!(quote(r#"a"$b`c"#), r#"'a"$b`c'"#);
    }

    /// The payload the reviewer used, for every single-quote character. If a
    /// quote ever ends the literal, the `exit 99` runs.
    fn injections() -> Vec<String> {
        SINGLE_QUOTES
            .iter()
            .map(|q| format!("C:\\Games\\Dan{q}; exit 99; #{q}"))
            .collect()
    }

    fn read_utf8(path: &Path) -> String {
        let bytes = std::fs::read(path).unwrap();
        let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        String::from_utf8(text.to_vec())
            .unwrap()
            .trim_end()
            .to_string()
    }

    #[tokio::test]
    async fn quoted_paths_never_inject_and_round_trip_in_a_command() {
        let out = std::env::temp_dir().join(format!("sarp-quote-{}.txt", random_hex(4)));
        for evil in injections() {
            let command = format!(
                "Set-Content -LiteralPath {} -Value {} -Encoding UTF8; exit 0",
                quote(&out.to_string_lossy()),
                quote(&evil)
            );
            assert_eq!(
                run(&command, Duration::from_secs(60)).await,
                Outcome::Exited(0),
                "{evil}"
            );
            assert_eq!(read_utf8(&out), evil);
        }
        let _ = std::fs::remove_file(&out);
    }

    #[tokio::test]
    async fn quoted_paths_never_inject_and_round_trip_in_a_script() {
        // How copy_script / extract_script bodies run (minus RunAs).
        let out = std::env::temp_dir().join(format!("sarp-quote-{}.txt", random_hex(4)));
        for evil in injections() {
            let body = format!(
                "Set-Content -LiteralPath {} -Value {} -Encoding UTF8\r\nexit 0",
                quote(&out.to_string_lossy()),
                quote(&evil)
            );
            let script = write_script("sarp-test-", &body).await.unwrap();
            let command = format!(
                "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"; exit $LASTEXITCODE",
                script.display()
            );
            assert_eq!(
                run(&command, Duration::from_secs(60)).await,
                Outcome::Exited(0),
                "{evil}"
            );
            assert_eq!(read_utf8(&out), evil);
            let _ = std::fs::remove_file(&script);
        }
        let _ = std::fs::remove_file(&out);
    }

    #[tokio::test]
    async fn start_process_with_a_quote_in_the_working_dir() {
        // A real folder named like the payload (no `:` or `\` in a name).
        let root = tempfile::tempdir().unwrap();
        for q in SINGLE_QUOTES {
            let dir = root.path().join(format!("Dan{q}; exit 99; #{q}"));
            std::fs::create_dir(&dir).unwrap();
            let command = start_process(
                "cmd.exe",
                &["/c".into(), "echo ok> cwd.txt".into()],
                Some(&dir),
                false,
            );
            assert_eq!(
                run(&command, Duration::from_secs(60)).await,
                Outcome::Exited(0),
                "{}",
                dir.display()
            );
            assert!(dir.join("cwd.txt").is_file(), "ran in {}", dir.display());
        }
    }

    #[test]
    fn start_process_command_shape() {
        let cmd = start_process(
            r"C:\Temp\sa-mp-install.exe",
            &[
                "/S".into(),
                r"/D=C:\Program Files (x86)\Rockstar Games\GTA San Andreas".into(),
            ],
            Some(Path::new(
                r"C:\Program Files (x86)\Rockstar Games\GTA San Andreas",
            )),
            true,
        );
        assert!(cmd.contains(r"Start-Process -FilePath 'C:\Temp\sa-mp-install.exe'"));
        assert!(cmd.contains(
            r"-ArgumentList '/S','/D=C:\Program Files (x86)\Rockstar Games\GTA San Andreas'"
        ));
        assert!(cmd.contains(
            r"-WorkingDirectory 'C:\Program Files (x86)\Rockstar Games\GTA San Andreas'"
        ));
        assert!(cmd.contains("-Verb RunAs -Wait -PassThru -WindowStyle Hidden -ErrorAction Stop;"));
        assert!(cmd.contains("exit 1225"));
        assert!(cmd.contains("-eq 1223) { exit 1223 }"));
        assert!(cmd.contains("exit 1224"));

        let plain = start_process("x.exe", &["/S".into()], None, false);
        assert!(!plain.contains("RunAs"));
        assert!(!plain.contains("WorkingDirectory"));
        assert!(plain.contains("-ArgumentList '/S' -Wait"));
    }

    #[test]
    fn elevated_script_path_is_double_quoted() {
        let cmd = run_script_elevated(Path::new(
            r"C:\Users\John Doe\AppData\Local\Temp\sarp-copy-1.ps1",
        ));
        assert!(cmd.contains(
            r#"-ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','"C:\Users\John Doe\AppData\Local\Temp\sarp-copy-1.ps1"'"#
        ));
        assert!(cmd.contains("-FilePath 'powershell.exe'"));
        assert!(cmd.contains("-Verb RunAs"));
    }

    #[test]
    fn copy_script_lists_pairs_on_separate_lines() {
        let script = copy_script(&[
            (
                PathBuf::from(r"C:\T\samp.dll"),
                PathBuf::from(r"C:\Game's\samp.dll"),
            ),
            (
                PathBuf::from(r"C:\T\samp.exe"),
                PathBuf::from(r"C:\Game's\samp.exe"),
            ),
        ]);
        assert!(script.contains(
            "  @{Src='C:\\T\\samp.dll'; Dst='C:\\Game''s\\samp.dll'},\r\n  @{Src='C:\\T\\samp.exe'"
        ));
        assert!(script.contains("Copy-Item -LiteralPath $i.Src -Destination $i.Dst -Force"));
        assert!(script.ends_with("  exit 1224\r\n}"));
    }

    #[test]
    fn extract_script_shape() {
        let script = extract_script(Path::new(r"C:\T\GTA_RIP.zip"), Path::new(r"D:\Games\GTA"));
        assert!(script.contains(
            r"Expand-Archive -LiteralPath 'C:\T\GTA_RIP.zip' -DestinationPath 'D:\Games\GTA' -Force"
        ));
    }

    #[tokio::test]
    async fn runs_hidden_and_reports_exit_codes() {
        assert_eq!(
            run("exit 7", Duration::from_secs(60)).await,
            Outcome::Exited(7)
        );
        assert_eq!(
            run("Start-Sleep -Seconds 30", Duration::from_millis(500)).await,
            Outcome::TimedOut
        );
    }

    #[tokio::test]
    async fn script_has_a_bom_and_runs_non_ascii_paths() {
        let marker = std::env::temp_dir().join(format!("sarp-test-ñandú-{}.txt", random_hex(4)));
        let body = format!(
            "Set-Content -LiteralPath {} -Value 'ok'\r\nexit 0",
            quote(&marker.to_string_lossy())
        );
        let script = write_script("sarp-test-", &body).await.unwrap();
        assert_eq!(&std::fs::read(&script).unwrap()[..3], &[0xEF, 0xBB, 0xBF]);
        // Same nested `-File` invocation the elevated path uses, minus RunAs:
        // tests must never raise a UAC prompt.
        let command = format!(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"; exit $LASTEXITCODE",
            script.display()
        );
        let outcome = run(&command, Duration::from_secs(60)).await;
        assert_eq!(outcome, Outcome::Exited(0));
        assert!(
            marker.is_file(),
            "script did not write {}",
            marker.display()
        );
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(&script);
    }
}
