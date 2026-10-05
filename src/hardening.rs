use crate::*;
use std::ffi::{OsStr, OsString};

pub(crate) const MAX_OCR_PDF_BYTES: u64 = 40_000_000;

const SYSTEM_BIN: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

const MAX_SYMLINK_HOPS: usize = 40;
const TEMP_ATTEMPTS: usize = 16;

/// Atomowy zapis pliku 600. Dowiązanie symboliczne w `path` zostaje: zapis trafia do jego celu.
/// Plik tymczasowy powstaje od razu z prawami 600 (`create_new` + `O_NOFOLLOW`, losowa nazwa),
/// jest fsync-owany przed `rename`, potem fsync katalogu.
pub(crate) fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    write_private_file_observed(path, bytes, |_, _| Ok(()))
}

/// `before_write` widzi plik tymczasowy tuż po utworzeniu, zanim trafią do niego dane.
fn write_private_file_observed(
    path: &Path,
    bytes: &[u8],
    before_write: impl FnOnce(&Path, &fs::File) -> Result<()>,
) -> Result<()> {
    use std::io::Write;

    let dest = resolve_symlink_destination(path)?;
    let name = dest
        .file_name()
        .ok_or_else(|| anyhow!("brak nazwy pliku w ścieżce {}", dest.display()))?
        .to_os_string();
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    create_dir_for_private_files(&dir)?;
    let (tmp, file) = create_private_temp(&dir, &name)?;
    let result = (|| -> Result<()> {
        let mut file = file;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Restrykcyjny umask mógł dać mniej niż 600; prawa ustawiamy na deskryptorze.
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 600 {}", tmp.display()))?;
        }
        before_write(&tmp, &file)?;
        file.write_all(bytes)
            .with_context(|| format!("zapis {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("fsync {}", tmp.display()))?;
        drop(file);
        fs::rename(&tmp, &dest).with_context(|| format!("rename {}", dest.display()))
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    sync_dir(&dir)
}

/// Cel zapisu: dla dowiązania symbolicznego jego (także nieistniejący) cel, inaczej sama ścieżka.
fn resolve_symlink_destination(path: &Path) -> Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = fs::read_link(&current)
                    .with_context(|| format!("odczyt dowiązania {}", current.display()))?;
                current = if target.is_absolute() {
                    target
                } else {
                    current
                        .parent()
                        .unwrap_or_else(|| Path::new(""))
                        .join(target)
                };
            }
            Ok(_) => return Ok(current),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(current),
            Err(err) => {
                return Err(err).with_context(|| format!("odczyt {}", current.display()));
            }
        }
    }
    Err(anyhow!(
        "zbyt wiele dowiązań symbolicznych: {}",
        path.display()
    ))
}

fn create_private_temp(dir: &Path, name: &OsStr) -> Result<(PathBuf, fs::File)> {
    for _ in 0..TEMP_ATTEMPTS {
        let tmp = dir.join(format!(
            ".{}.lab-tmp-{}",
            name.to_string_lossy(),
            random_suffix()
        ));
        match open_private_new(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            // Zajęta nazwa (także podłożone dowiązanie): losujemy następną.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("utworzenie {}", tmp.display()));
            }
        }
    }
    Err(anyhow!(
        "nie mogę utworzyć pliku tymczasowego w {}",
        dir.display()
    ))
}

/// Nowy plik 600 od chwili utworzenia; nie podąża za dowiązaniem i nie nadpisuje istniejącej nazwy.
fn open_private_new(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn random_suffix() -> String {
    use std::io::Read;

    let mut bytes = [0u8; 12];
    if fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok()
    {
        return hex::encode(bytes);
    }
    // Zapas bez /dev/urandom; `create_new` + `O_NOFOLLOW` i tak chronią przed podłożonym plikiem.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        "{}-{nanos:x}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn sync_dir(dir: &Path) -> Result<()> {
    let handle = fs::File::open(dir).with_context(|| format!("otwarcie {}", dir.display()))?;
    match handle.sync_all() {
        Ok(()) => Ok(()),
        // Niektóre systemy plików nie obsługują fsync katalogu.
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        Err(err) => Err(err).with_context(|| format!("fsync {}", dir.display())),
    }
}

/// Katalog konfiguracji LAB (`~/.config/lab`); powstaje z prawami 700.
fn lab_config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".config").join("lab"))
}

/// Tworzy brakujący katalog na pliki prywatne; katalog LAB i jego podkatalogi dostają 700.
pub(crate) fn create_dir_for_private_files(dir: &Path) -> Result<()> {
    create_dir_with_private_root(dir, lab_config_dir().as_deref())
}

fn create_dir_with_private_root(dir: &Path, private_root: Option<&Path>) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    match private_root {
        Some(root) if dir.starts_with(root) => {
            if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)
                    .with_context(|| format!("mkdir {}", parent.display()))?;
            }
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(dir)
                .with_context(|| format!("mkdir {}", dir.display()))
        }
        _ => fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display())),
    }
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
    find_tool(name, std::env::var_os("PATH").as_deref(), true).ok_or_else(|| {
        anyhow!(
            "brak narzędzia {name} w katalogach /opt/homebrew/bin, /usr/local/bin, /usr/bin, /bin"
        )
    })
}

/// Narzędzie tylko z katalogów systemowych, bez PATH użytkownika.
pub(crate) fn system_tool(name: &str) -> Option<PathBuf> {
    find_tool(name, None, true)
}

/// Tylko do raportu (doctor/onboard): katalogi systemowe, potem bezwzględne katalogi z PATH.
/// Niczego nie uruchamia.
pub(crate) fn tool_present_for_status(name: &str) -> bool {
    find_tool(name, std::env::var_os("PATH").as_deref(), false).is_some()
}

fn find_tool(name: &str, path_var: Option<&OsStr>, require_safe_dir: bool) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    for dir in SYSTEM_BIN {
        let path = Path::new(dir).join(name);
        if is_executable(&path) {
            return Some(path);
        }
    }
    for dir in std::env::split_paths(path_var?) {
        let usable = if require_safe_dir {
            dir_is_safe_bin(&dir)
        } else {
            dir.is_absolute()
        };
        if !usable {
            continue;
        }
        let path = dir.join(name);
        if is_executable(&path) {
            return Some(path);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn dir_is_safe_bin(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    dir.is_absolute()
        && fs::metadata(dir)
            .map(|meta| meta.permissions().mode() & 0o002 == 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn dir_is_safe_bin(dir: &Path) -> bool {
    dir.is_absolute()
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

    /// Katalog testu, unikalny w procesie; usuwany w Drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("lab-hardening-{name}-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn leftover_temps(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".lab-tmp-"))
            .collect()
    }

    #[test]
    fn private_file_is_owner_readable_only() {
        let dir = TempDir::new("mode");
        let path = dir.0.join("secret.json");
        write_private_file(&path, b"{\"k\":1}").unwrap();
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600);
        assert_eq!(fs::read(&path).unwrap(), b"{\"k\":1}");
        assert!(leftover_temps(&dir.0).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn replaces_wide_existing_file_with_private_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("replace");
        let path = dir.0.join("env");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_private_file(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(mode(&path), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn temp_file_is_private_and_empty_before_data_is_written() {
        let dir = TempDir::new("window");
        let path = dir.0.join(".env");
        let mut seen = None;
        write_private_file_observed(&path, b"KSEF_TOKEN='dummy'\n", |tmp, file| {
            let meta = file.metadata()?;
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
            assert_eq!(meta.len(), 0);
            // Ten sam plik widziany po nazwie: zwykły plik 600, nie dowiązanie.
            let by_name = fs::symlink_metadata(tmp)?;
            assert!(by_name.file_type().is_file());
            assert_eq!(by_name.permissions().mode() & 0o777, 0o600);
            seen = Some(tmp.to_path_buf());
            Ok(())
        })
        .unwrap();
        let tmp = seen.expect("hook nie został wywołany");
        let tmp_name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(tmp_name.starts_with("..env.lab-tmp-"), "{tmp_name}");
        let suffix = tmp_name.trim_start_matches("..env.lab-tmp-");
        assert_ne!(suffix, std::process::id().to_string());
        assert_eq!(suffix.len(), 24, "{suffix}");
        assert_eq!(tmp.parent(), Some(dir.0.as_path()));
        assert!(!tmp.exists());
        assert_eq!(fs::read(&path).unwrap(), b"KSEF_TOKEN='dummy'\n");
    }

    #[test]
    fn temp_names_are_unpredictable() {
        let a = random_suffix();
        let b = random_suffix();
        assert_ne!(a, b);
        assert!(a.len() >= 16);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_destination_survives_and_target_gets_content() {
        let dir = TempDir::new("symlink");
        let store = dir.0.join("store");
        fs::create_dir_all(&store).unwrap();
        let target = store.join(".env");
        fs::write(&target, "old").unwrap();
        let link = dir.0.join(".env");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_private_file(&link, b"new").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&link).unwrap(), target);
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(mode(&target), 0o600);
        assert!(leftover_temps(&dir.0).is_empty());
        assert!(leftover_temps(&store).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn relative_symlink_chain_is_followed() {
        let dir = TempDir::new("chain");
        fs::create_dir_all(dir.0.join("a")).unwrap();
        fs::write(dir.0.join("a/real"), "old").unwrap();
        std::os::unix::fs::symlink("a/real", dir.0.join("hop")).unwrap();
        std::os::unix::fs::symlink("hop", dir.0.join("link")).unwrap();

        write_private_file(&dir.0.join("link"), b"new").unwrap();

        assert_eq!(fs::read(dir.0.join("a/real")).unwrap(), b"new");
        assert!(
            fs::symlink_metadata(dir.0.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(dir.0.join("hop"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_writes_to_its_target_path() {
        let dir = TempDir::new("dangling");
        let target = dir.0.join("missing-dir").join(".env");
        let link = dir.0.join(".env");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_private_file(&link, b"KSEF_TOKEN='dummy'\n").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), b"KSEF_TOKEN='dummy'\n");
        assert_eq!(fs::read(&link).unwrap(), b"KSEF_TOKEN='dummy'\n");
        assert_eq!(mode(&target), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loop_is_an_error() {
        let dir = TempDir::new("loop");
        std::os::unix::fs::symlink("b", dir.0.join("a")).unwrap();
        std::os::unix::fs::symlink("a", dir.0.join("b")).unwrap();
        assert!(write_private_file(&dir.0.join("a"), b"x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn planted_symlink_at_temp_name_receives_nothing() {
        let dir = TempDir::new("planted");
        let victim = dir.0.join("victim");
        fs::write(&victim, "untouched").unwrap();
        // Dawna, przewidywalna nazwa: `.secret.json.lab-tmp-<pid>`.
        let old_name = dir
            .0
            .join(format!(".secret.json.lab-tmp-{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &old_name).unwrap();
        // Nawet gdy atakujący trafi w nazwę, `create_new` + `O_NOFOLLOW` odmawia.
        let guessed = dir.0.join(".secret.json.lab-tmp-guessed");
        std::os::unix::fs::symlink(&victim, &guessed).unwrap();
        let err = open_private_new(&guessed).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        let dangling = dir.0.join(".secret.json.lab-tmp-dangling");
        let dangling_target = dir.0.join("would-be-created");
        std::os::unix::fs::symlink(&dangling_target, &dangling).unwrap();
        assert!(open_private_new(&dangling).is_err());
        assert!(!dangling_target.exists());

        let path = dir.0.join("secret.json");
        write_private_file(&path, b"tajne-dummy").unwrap();

        assert_eq!(fs::read(&victim).unwrap(), b"untouched");
        assert!(
            fs::symlink_metadata(&old_name)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&path).unwrap(), b"tajne-dummy");
    }

    #[cfg(unix)]
    #[test]
    fn lab_config_dir_is_created_private() {
        let dir = TempDir::new("mkdir");
        let lab = dir.0.join("home/.config/lab");
        let nested = lab.join("playwright");
        create_dir_with_private_root(&nested, Some(&lab)).unwrap();
        assert_eq!(mode(&lab), 0o700);
        assert_eq!(mode(&nested), 0o700);
        // Katalogi poza katalogiem LAB zostają z domyślnymi prawami.
        assert_ne!(mode(&dir.0.join("home/.config")), 0o700);
        let other = dir.0.join("out/reports");
        create_dir_with_private_root(&other, Some(&lab)).unwrap();
        assert!(other.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn status_lookup_uses_absolute_path_dirs_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("which");
        let bin = dir.0.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let name = format!("lab-test-tool-{}", std::process::id());
        let tool = bin.join(&name);
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let path_var = std::env::join_paths([bin.clone()]).unwrap();
        assert_eq!(find_tool(&name, Some(&path_var), false), Some(tool.clone()));
        assert_eq!(find_tool(&name, Some(OsStr::new("bin")), false), None);
        assert_eq!(find_tool(&name, None, false), None);
        assert_eq!(find_tool("../bin/x", Some(&path_var), false), None);
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(find_tool(&name, Some(&path_var), false), None);
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
