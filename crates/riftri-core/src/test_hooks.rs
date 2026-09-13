use std::cell::RefCell;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilesystemRacePoint {
    BeforeJournalOpen,
    BeforeEmptyDirectoryRemoval,
}

type Hook = Box<dyn FnOnce(&Path)>;

thread_local! {
    static HOOK: RefCell<Option<(FilesystemRacePoint, Hook)>> = RefCell::new(None);
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
