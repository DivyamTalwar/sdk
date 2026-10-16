use super::*;
#[path = "../../../sdk-docs/src/sync.rs"]
mod sync;

thread_local! {
    static GIT: PathBuf = resolve_git();
    static COMPILER: sync::Arc<PathBuf> = resolve_cargo();
}

fn resolve_git() -> PathBuf {
    let paths = std::env::var_os("PATH").unwrap();
    let selected = std::env::split_paths(&paths)
        .map(|directory| directory.join(format!("git{}", std::env::consts::EXE_SUFFIX)))
        .find(|path| path.is_file())
        .unwrap();
    if cfg!(windows)
        && selected.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("cmd"))
    {
        let concrete = selected
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("mingw64/bin/git.exe");
        if concrete.is_file() {
            return concrete;
        }
    }
    selected
}

pub(super) fn git() -> PathBuf {
    GIT.with(Clone::clone)
}

pub(super) fn path() -> std::ffi::OsString {
    std::env::join_paths(
        [
            cargo().parent().unwrap().to_owned(),
            git().parent().unwrap().to_owned(),
        ]
        .into_iter()
        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap()
}

// Leaf slot: release it before any Cargo work or fixture locks.
static CARGO: sync::Mutex<Option<sync::Arc<PathBuf>>> = sync::Mutex::new(None);
// Independent leaf slots: constructors do not acquire another fixture slot.
static FIXTURES: [sync::Mutex<Option<sync::Arc<Fixture>>>; 3] =
    [const { sync::Mutex::new(None) }; 3];

pub(super) fn fixture(kind: u8) -> sync::Arc<Fixture> {
    sync::get_or_try_init(&FIXTURES[usize::from(kind)], || {
        let fixture = if kind == 2 {
            let fixture = feature_profile_source();
            init_git(&fixture.root, "feature profile fixture");
            fixture
        } else {
            build_fixture(kind == 1)
        };
        Ok::<_, std::convert::Infallible>(fixture)
    })
    .unwrap()
}

pub(super) fn cargo() -> sync::Arc<PathBuf> {
    COMPILER.with(sync::Arc::clone)
}

pub(super) fn rustc() -> PathBuf {
    cargo()
        .parent()
        .unwrap()
        .join(format!("rustc{}", std::env::consts::EXE_SUFFIX))
}

fn resolve_cargo() -> sync::Arc<PathBuf> {
    sync::get_or_try_init(&CARGO, || {
        let output = sdk_docs::command("rustup")
            .args(["which", "cargo", "--toolchain", "1.98.1"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", output_message(&output));
        Ok::<_, std::convert::Infallible>(PathBuf::from(
            String::from_utf8(output.stdout).unwrap().trim(),
        ))
    })
    .unwrap()
}

pub(super) fn binary(name: &str) -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

struct Timing(std::time::Instant, String);
thread_local! {
    static TIMING: Timing = Timing(std::time::Instant::now(), std::thread::current().name().unwrap_or("unnamed").to_owned());
}
pub(super) fn time_control() {
    TIMING.with(|_| ());
    // Resolve this leaf before fixture initialization can acquire its own slot.
    let _ = cargo();
}
pub(super) fn phase(name: &str) {
    TIMING.with(|timer| {
        eprintln!(
            "CONTROL_PHASE {} {name} {:.6}",
            timer.1,
            timer.0.elapsed().as_secs_f64()
        )
    });
}
impl Drop for Timing {
    fn drop(&mut self) {
        eprintln!(
            "CONTROL_TIME {} {:.6}",
            self.1,
            self.0.elapsed().as_secs_f64()
        );
    }
}
