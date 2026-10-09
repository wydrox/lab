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

/// Narzędzia uruchamiane przez LAB z katalogów systemowych; `lab doctor` i
/// `lab onboard --check` raportują dla nich odrzucone kopie i katalogi zapisywalne dla grupy.
const CHECKED_TOOLS: [&str; 5] = ["pdftotext", "pdfinfo", "pdftoppm", "openssl", "git"];

/// Narzędzie z katalogów systemowych, potem z bezpiecznych katalogów PATH. Kopia,
/// która nie przechodzi `untrusted_tool_reason`, jest pomijana; gdy nie zostaje żadna,
/// błąd wymienia odrzucone ścieżki.
pub(crate) fn local_tool(name: &str) -> Result<PathBuf> {
    find_trusted_tool(name, std::env::var_os("PATH").as_deref(), current_uid())
}

/// Narzędzie tylko z katalogów systemowych, bez PATH użytkownika (z tą samą kontrolą zaufania).
pub(crate) fn system_tool(name: &str) -> Option<PathBuf> {
    find_trusted_tool(name, None, current_uid()).ok()
}

/// Tylko do raportu (doctor/onboard): katalogi systemowe, potem bezwzględne katalogi z PATH.
/// Niczego nie uruchamia.
pub(crate) fn tool_present_for_status(name: &str) -> bool {
    find_tool(name, std::env::var_os("PATH").as_deref(), false).is_some()
}

fn find_tool(name: &str, path_var: Option<&OsStr>, require_safe_dir: bool) -> Option<PathBuf> {
    tool_candidates(name, path_var, require_safe_dir, current_uid())
        .into_iter()
        .next()
}

fn find_trusted_tool(name: &str, path_var: Option<&OsStr>, uid: u32) -> Result<PathBuf> {
    let mut rejected = Vec::new();
    for candidate in tool_candidates(name, path_var, true, uid) {
        match untrusted_tool_reason(&candidate, uid) {
            None => return Ok(candidate),
            Some(reason) => rejected.push(reason),
        }
    }
    if rejected.is_empty() {
        Err(anyhow!(
            "brak narzędzia {name} w katalogach {}",
            SYSTEM_BIN.join(", ")
        ))
    } else {
        Err(anyhow!(
            "odrzucam narzędzie {name}: {}",
            rejected.join("; ")
        ))
    }
}

/// Pliki wykonywalne `name` w kolejności wyszukiwania: katalogi systemowe, potem PATH
/// (przy `require_safe_dir` tylko katalogi z `dir_is_safe_bin`).
fn tool_candidates(
    name: &str,
    path_var: Option<&OsStr>,
    require_safe_dir: bool,
    uid: u32,
) -> Vec<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return Vec::new();
    }
    let mut found = SYSTEM_BIN
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .filter(|path| is_executable(path))
        .collect::<Vec<_>>();
    for dir in path_var.map(std::env::split_paths).into_iter().flatten() {
        let usable = if require_safe_dir {
            dir_is_safe_bin(&dir, uid)
        } else {
            dir.is_absolute()
        };
        if !usable {
            continue;
        }
        let path = dir.join(name);
        if is_executable(&path) && !found.contains(&path) {
            found.push(path);
        }
    }
    found
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

/// Katalog bezwzględny, którego właścicielem jest root albo `uid` i który nie jest
/// zapisywalny dla innych. Zapis dla grupy jest dozwolony (Homebrew: `drwxrwxr-x <user>:admin`).
#[cfg(unix)]
fn dir_is_safe_bin(dir: &Path, uid: u32) -> bool {
    dir.is_absolute()
        && fs::metadata(dir)
            .map(|meta| trust_problem(&meta, uid, TOOL_RULE).is_none())
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn dir_is_safe_bin(dir: &Path, _uid: u32) -> bool {
    dir.is_absolute()
}

/// Dlaczego narzędzia `path` nie wolno uruchomić: plik albo jego katalog, a po rozwinięciu
/// dowiązań także plik docelowy i jego katalog, jest zapisywalny dla innych albo należy do
/// kogoś innego niż root i `uid`. `None` = można uruchomić.
#[cfg(unix)]
fn untrusted_tool_reason(path: &Path, uid: u32) -> Option<String> {
    let real = match fs::canonicalize(path) {
        Ok(real) => real,
        Err(err) => return Some(format!("{}: {err}", path.display())),
    };
    let via = if real.as_path() != path {
        format!(" (dowiązanie do {})", real.display())
    } else {
        String::new()
    };
    let mut checked = Vec::new();
    for entry in [
        path.parent().map(Path::to_path_buf),
        Some(path.to_path_buf()),
        real.parent().map(Path::to_path_buf),
        Some(real.clone()),
    ]
    .into_iter()
    .flatten()
    {
        if checked.contains(&entry) {
            continue;
        }
        let meta = match fs::metadata(&entry) {
            Ok(meta) => meta,
            Err(err) => return Some(format!("{}: {err}", entry.display())),
        };
        let what = if meta.is_dir() { "katalog" } else { "plik" };
        match trust_problem(&meta, uid, TOOL_RULE) {
            None => {}
            Some(TrustProblem::Owner(owner)) => {
                return Some(format!(
                    "{}{via}: {what} {} należy do uid {owner}, nie do root ani bieżącego użytkownika",
                    path.display(),
                    entry.display()
                ));
            }
            Some(TrustProblem::Writable(mode)) => {
                return Some(format!(
                    "{}{via}: {what} {} ma prawa {:o} (zapis dla wszystkich)",
                    path.display(),
                    entry.display(),
                    mode & 0o7777
                ));
            }
        }
        checked.push(entry);
    }
    None
}

#[cfg(not(unix))]
fn untrusted_tool_reason(_path: &Path, _uid: u32) -> Option<String> {
    None
}

/// Ostrzeżenia dla `lab doctor` / `lab onboard --check`: odrzucone kopie narzędzi oraz
/// katalogi narzędzi w użyciu (PATH procesów potomnych i katalogi znalezionych narzędzi,
/// także po rozwinięciu dowiązań), które są zapisywalne dla grupy.
pub(crate) fn tool_dir_warnings() -> Vec<String> {
    tool_dir_warnings_for(std::env::var_os("PATH").as_deref(), current_uid())
}

fn tool_dir_warnings_for(path_var: Option<&OsStr>, uid: u32) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut in_use = isolated_path_dirs(path_var, uid);
    for tool in CHECKED_TOOLS {
        let mut chosen = None;
        for candidate in tool_candidates(tool, path_var, true, uid) {
            match untrusted_tool_reason(&candidate, uid) {
                Some(reason) => warnings.push(format!("pominięto {tool}: {reason}")),
                None => {
                    chosen = Some(candidate);
                    break;
                }
            }
        }
        let Some(path) = chosen else {
            continue;
        };
        let real = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        for dir in [path.parent(), real.parent()].into_iter().flatten() {
            if !in_use.iter().any(|known| known == dir) {
                in_use.push(dir.to_path_buf());
            }
        }
    }
    warnings.extend(in_use.iter().filter_map(|dir| group_writable_warning(dir)));
    warnings
}

#[cfg(unix)]
fn group_writable_warning(dir: &Path) -> Option<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = fs::metadata(dir).ok()?;
    let mode = meta.permissions().mode();
    (mode & 0o020 != 0).then(|| {
        format!(
            "katalog narzędzi {} jest zapisywalny dla grupy {} (prawa {:o}): inni członkowie tej grupy mogą podmienić uruchamiane przez LAB narzędzia",
            dir.display(),
            group_label(meta.gid()),
            mode & 0o7777
        )
    })
}

#[cfg(not(unix))]
fn group_writable_warning(_dir: &Path) -> Option<String> {
    None
}

/// `admin (gid 80)`; sam numer, gdy grupy nie ma w bazie.
#[cfg(unix)]
fn group_label(gid: u32) -> String {
    let mut buf = vec![0 as libc::c_char; 1024];
    loop {
        // SAFETY: `group` to zwykła struktura C; getgrgid_r wypełnia ją wskaźnikami do `buf`,
        // który żyje do końca odczytu poniżej. `result` jest NULL albo wskazuje na `group`.
        let mut group: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        let rc =
            unsafe { libc::getgrgid_r(gid, &mut group, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc == 0 && !result.is_null() && !group.gr_name.is_null() {
            // SAFETY: gr_name to zakończony zerem napis w `buf`.
            let name = unsafe { std::ffi::CStr::from_ptr(group.gr_name) };
            return format!("{} (gid {gid})", name.to_string_lossy());
        }
        return format!("gid {gid}");
    }
}

/// Co jest nie tak z właścicielem albo prawami wpisu w systemie plików.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustProblem {
    /// uid właściciela, który nie jest dozwolony.
    Owner(u32),
    /// Pełne prawa wpisu z zabronionym bitem zapisu.
    Writable(u32),
}

/// Reguła zaufania do pliku albo katalogu: kto może być właścicielem i kto nie może pisać.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct TrustRule {
    root_owner_ok: bool,
    forbidden_write: u32,
    sticky_ok: bool,
}

/// Skrypt użytkownika: tylko jego właściciel, bez zapisu dla grupy i innych.
#[cfg(unix)]
const SCRIPT_FILE_RULE: TrustRule = TrustRule {
    root_owner_ok: false,
    forbidden_write: 0o022,
    sticky_ok: false,
};

/// Katalog nad skryptem: użytkownik albo root, bez zapisu dla grupy/innych chyba że sticky.
#[cfg(unix)]
const SCRIPT_DIR_RULE: TrustRule = TrustRule {
    root_owner_ok: true,
    forbidden_write: 0o022,
    sticky_ok: true,
};

/// Narzędzie systemowe i jego katalog: użytkownik albo root, bez zapisu dla innych.
/// Zapis dla grupy jest dozwolony i raportowany (`tool_dir_warnings`).
#[cfg(unix)]
const TOOL_RULE: TrustRule = TrustRule {
    root_owner_ok: true,
    forbidden_write: 0o002,
    sticky_ok: false,
};

#[cfg(unix)]
fn trust_problem(meta: &fs::Metadata, uid: u32, rule: TrustRule) -> Option<TrustProblem> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let owner = meta.uid();
    if owner != uid && !(rule.root_owner_ok && owner == 0) {
        return Some(TrustProblem::Owner(owner));
    }
    let mode = meta.permissions().mode();
    if mode & rule.forbidden_write != 0 && !(rule.sticky_ok && mode & 0o1000 != 0) {
        return Some(TrustProblem::Writable(mode));
    }
    None
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

/// Zmienne z sekretami LAB i tokenem Gmail; procesy potomne, które ich nie potrzebują
/// (node, npm, skrypt logowania Saldeo), dostają środowisko bez nich.
pub(crate) const CHILD_SECRET_ENV_KEYS: [&str; 8] = [
    "KSEF_TOKEN",
    "KSEF_CERT_PASSWORD",
    "KSEF_ACCESS_TOKEN",
    "SALDEO_USERNAME",
    "SALDEO_PASSWORD",
    "OPENROUTER_API_KEY",
    "GMAIL_ACCESS_TOKEN",
    "GMAIL_REFRESH_TOKEN",
];

/// Reszta środowiska zostaje (PATH, HOME, proxy npm); znikają tylko sekrety LAB.
pub(crate) fn remove_secret_env(command: &mut Command) -> &mut Command {
    for key in crate::SECRET_ENV_KEYS
        .iter()
        .chain(CHILD_SECRET_ENV_KEYS.iter())
    {
        command.env_remove(key);
    }
    command
}

#[cfg(unix)]
pub(crate) fn current_uid() -> u32 {
    // SAFETY: geteuid nie ma warunków wstępnych i zawsze się udaje.
    unsafe { libc::geteuid() }
}

/// Skrypt do uruchomienia z pełnymi prawami użytkownika: zwykły plik wykonywalny,
/// właściciel `uid`, bez zapisu dla grupy i innych. Każdy katalog nad nim należy do `uid`
/// albo do root i nie jest zapisywalny dla grupy/innych (chyba że ma bit sticky, jak /tmp),
/// więc nikt inny nie podmieni pliku ani katalogu. Zwraca ścieżkę bez dowiązań.
#[cfg(unix)]
pub(crate) fn trusted_user_script(path: &Path, uid: u32) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let real = fs::canonicalize(path).with_context(|| format!("skrypt {}", path.display()))?;
    let meta = fs::metadata(&real).with_context(|| format!("skrypt {}", real.display()))?;
    if !meta.is_file() {
        return Err(anyhow!("{} nie jest zwykłym plikiem", real.display()));
    }
    match trust_problem(&meta, uid, SCRIPT_FILE_RULE) {
        None => {}
        Some(TrustProblem::Owner(owner)) => {
            return Err(anyhow!(
                "odmawiam uruchomienia {}: właścicielem jest uid {owner}, nie bieżący użytkownik",
                real.display()
            ));
        }
        Some(TrustProblem::Writable(mode)) => {
            return Err(anyhow!(
                "odmawiam uruchomienia {}: prawa {:o} pozwalają na zapis grupie lub innym",
                real.display(),
                mode & 0o777
            ));
        }
    }
    if meta.permissions().mode() & 0o100 == 0 {
        return Err(anyhow!(
            "{} nie jest wykonywalny (chmod u+x)",
            real.display()
        ));
    }
    for dir in real.ancestors().skip(1) {
        let meta = fs::metadata(dir).with_context(|| format!("katalog {}", dir.display()))?;
        match trust_problem(&meta, uid, SCRIPT_DIR_RULE) {
            None => {}
            Some(TrustProblem::Owner(owner)) => {
                return Err(anyhow!(
                    "odmawiam uruchomienia {}: katalog {} należy do uid {owner}",
                    real.display(),
                    dir.display()
                ));
            }
            Some(TrustProblem::Writable(mode)) => {
                return Err(anyhow!(
                    "odmawiam uruchomienia {}: katalog {} ma prawa {:o} (zapis dla grupy lub innych)",
                    real.display(),
                    dir.display(),
                    mode & 0o7777
                ));
            }
        }
    }
    Ok(real)
}

#[cfg(not(unix))]
pub(crate) fn current_uid() -> u32 {
    0
}

#[cfg(not(unix))]
pub(crate) fn trusted_user_script(path: &Path, _uid: u32) -> Result<PathBuf> {
    Err(anyhow!(
        "uruchamianie skryptu {} wymaga systemu Unix",
        path.display()
    ))
}

fn isolated_path() -> OsString {
    let dirs = isolated_path_dirs(std::env::var_os("PATH").as_deref(), current_uid());
    std::env::join_paths(dirs).unwrap_or_else(|_| OsString::from("/usr/bin:/bin"))
}

/// Katalogi PATH procesów potomnych: systemowe, potem z PATH użytkownika; każdy
/// przechodzi `dir_is_safe_bin` (root albo użytkownik, bez zapisu dla innych).
fn isolated_path_dirs(path_var: Option<&OsStr>, uid: u32) -> Vec<PathBuf> {
    let mut dirs = SYSTEM_BIN
        .iter()
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir() && dir_is_safe_bin(dir, uid))
        .collect::<Vec<_>>();
    for dir in path_var.map(std::env::split_paths).into_iter().flatten() {
        if dir_is_safe_bin(&dir, uid) && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
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

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Katalog `bin` (755) z wykonywalnym `name` (755) w katalogu testu.
    #[cfg(unix)]
    fn tool_in(root: &Path, dir: &str, name: &str) -> PathBuf {
        let bin = root.join(dir);
        fs::create_dir_all(&bin).unwrap();
        set_mode(&bin, 0o755);
        let tool = bin.join(name);
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        set_mode(&tool, 0o755);
        tool
    }

    #[cfg(unix)]
    fn unique_tool_name() -> String {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        format!(
            "lab-test-trusted-tool-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    #[cfg(unix)]
    #[test]
    fn tool_trust_rejects_world_writable_and_foreign_owner_but_allows_group_write() {
        let dir = TempDir::new("tool-trust");
        let uid = current_uid();
        let name = unique_tool_name();
        let tool = tool_in(&dir.0, "bin", &name);
        let bin = tool.parent().unwrap().to_path_buf();
        assert_eq!(untrusted_tool_reason(&tool, uid), None);

        // Zapis dla grupy (układ Homebrew): dozwolony.
        set_mode(&bin, 0o775);
        set_mode(&tool, 0o775);
        assert_eq!(untrusted_tool_reason(&tool, uid), None);

        set_mode(&tool, 0o757);
        let reason = untrusted_tool_reason(&tool, uid).unwrap();
        assert!(reason.contains(&tool.display().to_string()), "{reason}");
        assert!(reason.contains("757"), "{reason}");
        set_mode(&tool, 0o755);

        set_mode(&bin, 0o777);
        let reason = untrusted_tool_reason(&tool, uid).unwrap();
        assert!(reason.contains("katalog"), "{reason}");
        assert!(reason.contains("777"), "{reason}");
        set_mode(&bin, 0o755);

        // Właściciel inny niż root i bieżący użytkownik (symulowany innym uid).
        if uid != 0 {
            let reason = untrusted_tool_reason(&tool, uid.wrapping_add(1)).unwrap();
            assert!(reason.contains(&format!("uid {uid}")), "{reason}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn tool_trust_checks_symlink_target_and_its_directory() {
        let dir = TempDir::new("tool-link");
        let uid = current_uid();
        let name = unique_tool_name();
        // Jak Homebrew: bin/<name> -> ../Cellar/x/bin/<name>.
        let real = tool_in(&dir.0, "Cellar/x/bin", &name);
        let bin = dir.0.join("bin");
        fs::create_dir_all(&bin).unwrap();
        set_mode(&bin, 0o755);
        let link = bin.join(&name);
        std::os::unix::fs::symlink(format!("../Cellar/x/bin/{name}"), &link).unwrap();
        assert_eq!(untrusted_tool_reason(&link, uid), None);

        let cellar_bin = real.parent().unwrap();
        set_mode(cellar_bin, 0o777);
        let reason = untrusted_tool_reason(&link, uid).unwrap();
        assert!(reason.contains(&link.display().to_string()), "{reason}");
        assert!(reason.contains("dowiązanie"), "{reason}");
        set_mode(cellar_bin, 0o755);

        set_mode(&real, 0o777);
        assert!(untrusted_tool_reason(&link, uid).is_some());
        set_mode(&real, 0o755);

        // Dowiązanie wiszące: odrzucone, nie uruchamiane.
        let dangling = bin.join(format!("{name}-dangling"));
        std::os::unix::fs::symlink("../missing", &dangling).unwrap();
        assert!(untrusted_tool_reason(&dangling, uid).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn trusted_lookup_skips_rejected_copy_and_names_it_when_none_is_left() {
        let dir = TempDir::new("tool-lookup");
        let uid = current_uid();
        let name = unique_tool_name();
        let bad_real = tool_in(&dir.0, "evil", &name);
        let first = dir.0.join("first");
        fs::create_dir_all(&first).unwrap();
        set_mode(&first, 0o755);
        std::os::unix::fs::symlink(&bad_real, first.join(&name)).unwrap();
        set_mode(&bad_real, 0o777);
        let good = tool_in(&dir.0, "second", &name);
        let path_var =
            std::env::join_paths([first.clone(), good.parent().unwrap().into()]).unwrap();

        assert_eq!(
            find_trusted_tool(&name, Some(&path_var), uid).unwrap(),
            good
        );

        let only_bad = std::env::join_paths([first.clone()]).unwrap();
        let err = find_trusted_tool(&name, Some(&only_bad), uid)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&first.join(&name).display().to_string()),
            "{err}"
        );
        assert!(err.contains("odrzucam"), "{err}");

        let none = find_trusted_tool(&name, None, uid).unwrap_err().to_string();
        assert!(none.contains("brak narzędzia"), "{none}");
    }

    #[cfg(unix)]
    #[test]
    fn isolated_path_keeps_only_trusted_dirs() {
        let dir = TempDir::new("tool-path");
        let uid = current_uid();
        let mine = dir.0.join("mine");
        let group = dir.0.join("group");
        let world = dir.0.join("world");
        for (path, mode) in [(&mine, 0o755), (&group, 0o775), (&world, 0o777)] {
            fs::create_dir_all(path).unwrap();
            set_mode(path, mode);
        }
        let path_var = std::env::join_paths([
            mine.clone(),
            group.clone(),
            world.clone(),
            PathBuf::from("relative/bin"),
        ])
        .unwrap();
        let dirs = isolated_path_dirs(Some(&path_var), uid);
        assert!(dirs.contains(&mine));
        assert!(dirs.contains(&group));
        assert!(!dirs.contains(&world));
        assert!(!dirs.iter().any(|d| d.is_relative()));
        if uid != 0 {
            let foreign = isolated_path_dirs(Some(&path_var), uid.wrapping_add(1));
            assert!(!foreign.contains(&mine));
        }
    }

    #[cfg(unix)]
    #[test]
    fn group_writable_tool_dir_is_reported_with_group() {
        let dir = TempDir::new("tool-warn");
        let uid = current_uid();
        let group = dir.0.join("group");
        fs::create_dir_all(&group).unwrap();
        set_mode(&group, 0o775);
        let world = dir.0.join("world");
        fs::create_dir_all(&world).unwrap();
        set_mode(&world, 0o777);
        let path_var = std::env::join_paths([group.clone(), world.clone()]).unwrap();
        let warnings = tool_dir_warnings_for(Some(&path_var), uid);
        let line = warnings
            .iter()
            .find(|w| w.contains(&group.display().to_string()))
            .unwrap_or_else(|| panic!("{warnings:?}"));
        assert!(line.contains("dla grupy"), "{line}");
        assert!(line.contains("gid "), "{line}");
        assert!(line.contains("podmienić"), "{line}");
        // Katalog zapisywalny dla wszystkich nie jest w użyciu, więc nie ma o nim linii.
        assert!(
            !warnings
                .iter()
                .any(|w| w.contains(&world.display().to_string()))
        );
        set_mode(&group, 0o755);
        assert!(group_writable_warning(&group).is_none());
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
