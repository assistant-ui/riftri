use super::{BaseReadLock, acquire_base_read_lock, open_coordination_lock};
use fs2::FileExt;

#[test]
fn readers_share_ownership_and_exclude_writers_until_the_last_reader_exits() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("base.lock");
    let first = acquire_base_read_lock(&path).unwrap();
    let second = open_coordination_lock(&path, "test second reader").unwrap();
    FileExt::try_lock_shared(&second).expect("first reader must hold shared ownership");
    let second = BaseReadLock(second);
    let writer = open_coordination_lock(&path, "test writer").unwrap();
    for reader in [first, second] {
        let error = FileExt::try_lock_exclusive(&writer).unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            fs2::lock_contended_error().raw_os_error()
        );
        drop(reader);
    }
    FileExt::try_lock_exclusive(&writer).unwrap();
    let contender = open_coordination_lock(&path, "test reader").unwrap();
    let error = FileExt::try_lock_shared(&contender).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        fs2::lock_contended_error().raw_os_error()
    );
    FileExt::unlock(&writer).unwrap();
    FileExt::try_lock_shared(&contender).unwrap();
    assert!(path.is_file(), "coordination locks must never be unlinked");
}

#[cfg(unix)]
#[test]
fn reader_drop_releases_a_description_still_held_by_an_inherited_descriptor() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("base.lock");
    let reader = acquire_base_read_lock(&path).unwrap();
    // Like fork(), dup() retains the same Unix open-file description. Merely
    // closing the original descriptor would keep its flock alive here.
    let inherited = reader.0.try_clone().unwrap();
    drop(reader);
    let writer = open_coordination_lock(&path, "test writer").unwrap();
    FileExt::try_lock_exclusive(&writer).expect("reader ownership explicitly released");
    drop(inherited);
}

#[cfg(unix)]
#[test]
fn reader_rejects_a_symlinked_lock_without_changing_its_target() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("preserve");
    std::fs::write(&target, b"preserve\n").unwrap();
    let path = fixture.path().join("base.lock");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(acquire_base_read_lock(&path).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"preserve\n");
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
