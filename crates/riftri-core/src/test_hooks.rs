use std::cell::{Cell, RefCell};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilesystemRacePoint {
    JournalOpen,
    UnpublishedIntentOpen,
    DestinationParentProbe,
    JournalOpened,
    EmptyDirectoryRemoval,
    ForceRemovalRevalidation,
    RemovalQuarantined,
    RollbackGitRemoval,
    JournalRetirementLocked,
    BaseReadMiss,
    BaseReuseAfterExclusiveWait,
    AddIntentPersist,
    AddCleanCheck,
}

type Hook = Box<dyn FnOnce(&Path)>;

thread_local! {
    static HOOK: RefCell<Option<(FilesystemRacePoint, Hook)>> = RefCell::new(None);
    static JOURNAL_OPENS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(any(
    target_os = "macos",
    all(target_os = "linux", feature = "native-cow-integration")
))]
pub(crate) fn journal_open_count() -> usize {
    JOURNAL_OPENS.with(Cell::get)
}

pub(crate) struct FilesystemRaceHookGuard;

impl Drop for FilesystemRaceHookGuard {
    fn drop(&mut self) {
        HOOK.with(|hook| {
            hook.borrow_mut().take();
        });
    }
}

pub(crate) fn install(
    point: FilesystemRacePoint,
    hook: impl FnOnce(&Path) + 'static,
) -> FilesystemRaceHookGuard {
    HOOK.with(|slot| {
        let previous = slot.borrow_mut().replace((point, Box::new(hook)));
        assert!(
            previous.is_none(),
            "a filesystem race hook is already installed"
        );
    });
    FilesystemRaceHookGuard
}

pub(crate) fn fire(point: FilesystemRacePoint, path: &Path) {
    if point == FilesystemRacePoint::JournalOpen {
        JOURNAL_OPENS.with(|count| count.set(count.get() + 1));
    }
    HOOK.with(|slot| {
        let installed = slot.borrow_mut().take();
        match installed {
            Some((installed_point, hook)) if installed_point == point => hook(path),
            Some(installed) => {
                slot.borrow_mut().replace(installed);
            }
            None => {}
        }
    });
}
