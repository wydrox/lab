//! Katalog LAB: domyślna baza `lab.sqlite` i drzewo `data/` liczone od jednego miejsca,
//! a nie od katalogu, w którym akurat uruchomiono `lab`.

use crate::*;

/// Klucz katalogu LAB: zmienna sesji albo `~/.config/lab/env`. Tę samą nazwę ustawia
/// automatyzacja launchd (`scripts/install-launchd.sh`).
pub(crate) const LAB_ROOT_KEY: &str = "LAB_ROOT";

/// Skąd pochodzi katalog LAB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LabRootSource {
    /// `LAB_ROOT` w zmiennej procesu.
    Env,
    /// `LAB_ROOT` z pliku konfiguracji: ustawiony ręcznie albo zapamiętany przy pierwszym
    /// uruchomieniu w katalogu z danymi.
    Config,
    /// Bieżący katalog z `lab.sqlite`, `data/` albo checkout pakietu `lab-cli`.
    Cwd,
    /// `~/.local/share/lab`.
    Home,
}

impl LabRootSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LabRootSource::Env => "env",
            LabRootSource::Config => "config",
            LabRootSource::Cwd => "cwd",
            LabRootSource::Home => "home",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LabRoot {
    pub(crate) path: PathBuf,
    pub(crate) source: LabRootSource,
}

/// Wszystko, od czego zależy wybór katalogu; bez dostępu do środowiska procesu.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LabRootInputs<'a> {
    /// `LAB_ROOT` ze zmiennej procesu.
    pub(crate) env: Option<&'a str>,
    /// `LAB_ROOT` z pliku konfiguracji (`lab_config_var`).
    pub(crate) config: Option<&'a str>,
    pub(crate) cwd: Option<&'a Path>,
    pub(crate) home: Option<&'a Path>,
}

/// Kolejność: `LAB_ROOT` (zmienna procesu, potem plik konfiguracji, w którym ląduje też
/// katalog zapamiętany z reguły bieżącego katalogu), bieżący katalog z danymi LAB,
/// `~/.local/share/lab`.
pub(crate) fn resolve_lab_root(inputs: &LabRootInputs<'_>) -> LabRoot {
    for (value, source) in [
        (inputs.env, LabRootSource::Env),
        (inputs.config, LabRootSource::Config),
    ] {
        if let Some(path) = value.and_then(|value| lab_root_value_path(value, inputs)) {
            return LabRoot { path, source };
        }
    }
    if let Some(cwd) = inputs.cwd
        && cwd_holds_lab_state(cwd)
    {
        return LabRoot {
            path: cwd.to_path_buf(),
            source: LabRootSource::Cwd,
        };
    }
    let path = inputs
        .home
        .map(|home| home.join(".local").join("share").join("lab"))
        .or_else(|| inputs.cwd.map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    LabRoot {
        path,
        source: LabRootSource::Home,
    }
}

/// Pusta wartość = brak; `~` i `~/…` względem HOME; ścieżka względna względem bieżącego katalogu.
fn lab_root_value_path(value: &str, inputs: &LabRootInputs<'_>) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let path = if value == "~" {
        inputs.home?.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        inputs.home?.join(rest)
    } else {
        PathBuf::from(value)
    };
    if path.is_absolute() {
        return Some(path);
    }
    Some(match inputs.cwd {
        Some(cwd) => cwd.join(path),
        None => path,
    })
}

/// Katalog, w którym LAB już pracował (`lab.sqlite`, `data/`) albo checkout `lab-cli`.
pub(crate) fn cwd_holds_lab_state(cwd: &Path) -> bool {
    cwd.join("lab.sqlite").is_file()
        || cwd.join("data").is_dir()
        || crate::credentials::cwd_is_lab_package(cwd)
}

/// Komunikat na stderr, gdy katalog nie jest ani wskazany zmienną procesu, ani bieżącym katalogiem.
pub(crate) fn lab_root_notice(root: &LabRoot, cwd: Option<&Path>) -> Option<String> {
    match root.source {
        LabRootSource::Env | LabRootSource::Cwd => None,
        LabRootSource::Config if cwd == Some(root.path.as_path()) => None,
        LabRootSource::Config => Some(format!(
            "katalog LAB (baza i data/): {} — {LAB_ROOT_KEY} z {}",
            root.path.display(),
            lab_env_file_path().display()
        )),
        LabRootSource::Home => Some(format!(
            "katalog LAB (baza i data/): {} — bieżący katalog nie ma lab.sqlite ani data/, a {LAB_ROOT_KEY} nie jest ustawiony",
            root.path.display()
        )),
    }
}

/// Zapisuje katalog jako `LAB_ROOT` w `~/.config/lab/env`, gdy nic tam jeszcze nie ma.
/// `Ok(false)`: wartość już była. Nieczytelny plik = błąd, bez zapisu.
pub(crate) fn remember_lab_root(root: &Path) -> Result<bool> {
    let mut vars = read_lab_env_file()?;
    if vars
        .get(LAB_ROOT_KEY)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(false);
    }
    vars.insert(LAB_ROOT_KEY.to_string(), root.display().to_string());
    write_lab_env_file(&vars)?;
    Ok(true)
}

#[cfg_attr(test, allow(dead_code))]
fn resolve_process_lab_root() -> LabRoot {
    let env = std::env::var(LAB_ROOT_KEY).ok();
    let config = lab_config_var(LAB_ROOT_KEY);
    let cwd = std::env::current_dir().ok();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let root = resolve_lab_root(&LabRootInputs {
        env: env.as_deref(),
        config: config.as_deref(),
        cwd: cwd.as_deref(),
        home: home.as_deref(),
    });
    if root.source == LabRootSource::Cwd {
        match remember_lab_root(&root.path) {
            Ok(true) => eprintln!(
                "  [LAB] zapamiętano katalog LAB {} jako {LAB_ROOT_KEY} w {}; uruchomienie z innego katalogu użyje tej samej bazy i data/",
                root.path.display(),
                lab_env_file_path().display()
            ),
            Ok(false) => {}
            Err(err) => eprintln!(
                "  [LAB] nie zapisuję {LAB_ROOT_KEY} w {}: {err:#}",
                lab_env_file_path().display()
            ),
        }
    }
    if let Some(notice) = lab_root_notice(&root, cwd.as_deref()) {
        eprintln!("  [LAB] {notice}");
    }
    root
}

/// Katalog LAB procesu; wybierany raz, przy pierwszym użyciu.
#[cfg(not(test))]
pub(crate) fn lab_root_info() -> LabRoot {
    static ROOT: std::sync::OnceLock<LabRoot> = std::sync::OnceLock::new();
    ROOT.get_or_init(resolve_process_lab_root).clone()
}

/// W testach: katalog ustawiony przez `set_test_lab_root`, inaczej pusty (ścieżki względne
/// jak dawniej). Nigdy nie czyta HOME ani nie zapisuje konfiguracji.
#[cfg(test)]
pub(crate) fn lab_root_info() -> LabRoot {
    LabRoot {
        path: test_root::current(),
        source: LabRootSource::Cwd,
    }
}

pub(crate) fn lab_root() -> PathBuf {
    lab_root_info().path
}

/// Ścieżka w katalogu LAB; bezwzględna zostaje bez zmian.
pub(crate) fn lab_path(relative: impl AsRef<Path>) -> PathBuf {
    lab_root().join(relative)
}

pub(crate) fn default_db_path() -> PathBuf {
    lab_path("lab.sqlite")
}

/// `source_path` zapisany w rekordzie. Względny (np. `data/mail-all-pdf-2026-pdfs/x.pdf`
/// sprzed katalogu LAB) jest liczony od katalogu LAB; gdy tam pliku nie ma, a jest względem
/// bieżącego katalogu (rekordy z jawnego `--mail`), zostaje ścieżka względna.
pub(crate) fn resolve_lab_source_path(source_path: impl AsRef<Path>) -> PathBuf {
    resolve_source_path_in(&lab_root(), source_path.as_ref())
}

pub(crate) fn resolve_source_path_in(root: &Path, source_path: &Path) -> PathBuf {
    if source_path.is_absolute() {
        return source_path.to_path_buf();
    }
    let in_root = root.join(source_path);
    if !in_root.exists() && source_path.exists() {
        return source_path.to_path_buf();
    }
    in_root
}

#[cfg(test)]
mod test_root {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub(super) fn current() -> PathBuf {
        ROOT.with(|root| root.borrow().clone()).unwrap_or_default()
    }

    pub(super) fn set(path: Option<PathBuf>) {
        ROOT.with(|root| *root.borrow_mut() = path);
    }
}

/// Przywraca pusty katalog LAB wątku testu przy drop.
#[cfg(test)]
pub(crate) struct TestLabRootGuard;

#[cfg(test)]
impl Drop for TestLabRootGuard {
    fn drop(&mut self) {
        test_root::set(None);
    }
}

/// Katalog LAB dla bieżącego wątku testu.
#[cfg(test)]
pub(crate) fn set_test_lab_root(path: &Path) -> TestLabRootGuard {
    test_root::set(Some(path.to_path_buf()));
    TestLabRootGuard
}

#[cfg(test)]
mod lab_root_tests;
