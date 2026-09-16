use crate::*;
use std::ffi::{OsStr, OsString};

pub(crate) const MAX_OCR_PDF_BYTES: u64 = 40_000_000;

const SYSTEM_BIN: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

pub(crate) fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.lab-tmp-{}",
        path.file_name()
            .unwrap_or_else(|| OsStr::new("lab"))
            .to_string_lossy(),
        std::process::id()
    ));
    fs::write(&tmp, bytes).with_context(|| format!("zapis {}", tmp.display()))?;
    chmod_private(&tmp)
        .and_then(|_| fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display())))
        .map_err(|err| {
            let _ = fs::remove_file(&tmp);
            err
        })?;
    chmod_private(path)
}

pub(crate) fn chmod_sqlite_files(path: &Path) -> Result<()> {
    chmod_private(path)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.to_string_lossy()));
        if sidecar.exists() {
            chmod_private(&sidecar)?;
        }
    }
    Ok(())
}

pub(crate) fn chmod_private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    let _ = path;
    Ok(())
}

pub(crate) fn local_tool(name: &str) -> Result<PathBuf> {
    for dir in SYSTEM_BIN {
        let path = Path::new(dir).join(name);
        if is_executable(&path) {
            return Ok(path);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if !dir_is_safe_bin(&dir) {
                continue;
            }
            let path = dir.join(name);
            if is_executable(&path) {
                return Ok(path);
            }
        }
    }
    Err(anyhow!(
        "brak narzędzia {name} w katalogach /opt/homebrew/bin, /usr/local/bin, /usr/bin, /bin"
    ))
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn dir_is_safe_bin(dir: &Path) -> bool {
    if !dir.is_absolute() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return fs::metadata(dir)
            .map(|meta| meta.permissions().mode() & 0o002 == 0)
            .unwrap_or(false);
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) fn apply_isolated_env(command: &mut Command) -> &mut Command {
    command.env_clear();
    command.env("PATH", isolated_path());
    command.env("LC_ALL", "C");
    command.env("LANG", "C");
    command.env("HF_HUB_OFFLINE", "1");
    command.env("TRANSFORMERS_OFFLINE", "1");
    command.env("PYTHONNOUSERSITE", "1");
    if let Some(home) = std::env::var_os("HOME") {
        command.env("HOME", home);
    }
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        command.env("TMPDIR", tmp);
    }
    command
}

fn isolated_path() -> OsString {
    let mut dirs = SYSTEM_BIN
        .iter()
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .collect::<Vec<_>>();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir_is_safe_bin(&dir) && !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| OsString::from("/usr/bin:/bin"))
}

pub(crate) fn local_llm_base_url(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw.trim()).context("PPMLX_BASE_URL")?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(anyhow!("PPMLX_BASE_URL wymaga http albo https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(anyhow!("PPMLX_BASE_URL nie może zawierać danych logowania"));
    }
    match url.host_str() {
        Some("127.0.0.1" | "localhost" | "::1") => {}
        _ => {
            return Err(anyhow!(
                "PPMLX_BASE_URL musi wskazywać 127.0.0.1, localhost albo ::1"
            ));
        }
    }
    Ok(raw.trim().trim_end_matches('/').to_string())
}

pub(crate) fn require_existing_file(path: &Path, what: &str) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        return Err(anyhow!("{what} wymaga ścieżki bezwzględnej"));
    };
    if !path.is_file() {
        return Err(anyhow!("brak pliku {what}: {}", path.display()));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_file_is_owner_readable_only() {
        let dir = std::env::temp_dir().join(format!("lab-hardening-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret.json");
        write_private_file(&path, b"{\"k\":1}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read(&path).unwrap(), b"{\"k\":1}");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn llm_url_accepts_only_loopback() {
        assert_eq!(
            local_llm_base_url("http://127.0.0.1:6767/").unwrap(),
            "http://127.0.0.1:6767"
        );
        assert!(local_llm_base_url("http://example.com:6767").is_err());
        assert!(local_llm_base_url("http://user:pass@127.0.0.1:6767").is_err());
        assert!(local_llm_base_url("file:///tmp/ppmlx").is_err());
    }

    #[test]
    fn relative_python_path_is_rejected() {
        assert!(require_existing_file(Path::new("python3"), "LAB_OCR_PYTHON").is_err());
    }
}
