use super::*;
const FAKE: &str = "Bearer fixture-only-never-a-real-token";
fn fixture() -> (tempfile::TempDir, CredentialStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::at_directory(dir.path(), &"a".repeat(64)).unwrap();
    (dir, store)
}
fn create(store: &CredentialStore) -> CredentialStatus {
    store.create(SecretAuthorization::new(FAKE.into())).unwrap()
}

#[test]
fn immutable_references_round_trip_survive_reopen_and_delete_idempotently() {
    let (dir, store) = fixture();
    assert!(!store.status("unknown").unwrap().present);
    assert!(!store.root.exists());
    let first = create(&store);
    let second = create(&store);
    assert_ne!(first.reference, second.reference);
    let reopened = CredentialStore::at_directory(dir.path(), &store.scope).unwrap();
    let value = reopened.authorization(&first.reference).unwrap();
    assert_eq!(value.to_str().unwrap(), FAKE);
    assert!(value.is_sensitive());
    assert!(!format!("{value:?}").contains(FAKE));
    assert!(!format!("{:?}", SecretAuthorization::new(FAKE.into())).contains(FAKE));
    for value in [serde_json::to_value(&first).unwrap(), store.description()] {
        assert!(!value.to_string().contains(FAKE));
    }
    let bytes = fs::read(store.path(&first.reference).unwrap()).unwrap();
    let contains = bytes
        .windows(FAKE.len())
        .any(|window| window == FAKE.as_bytes());
    assert_eq!(
        contains,
        cfg!(unix),
        "Protection must match its advertised mode"
    );
    assert!(reopened.delete(&first.reference).unwrap().removed);
    assert!(!reopened.delete(&first.reference).unwrap().removed);
    assert!(!reopened.status(&first.reference).unwrap().present);
    assert!(reopened.status(&second.reference).unwrap().present);
}

#[test]
fn invalid_inputs_never_create_storage_and_errors_do_not_echo_secrets() {
    let (_dir, store) = fixture();
    for text in [
        String::new(),
        " ".into(),
        "Bearer x\r\ninjected:y".into(),
        "Bearer \u{7f}".into(),
        "Bearer é".into(),
        "x".repeat(MAX_AUTHORIZATION_BYTES + 1),
    ] {
        assert_eq!(
            store.create(SecretAuthorization::new(text)).unwrap_err(),
            CredentialError::Invalid
        );
    }
    assert!(!store.root.exists());
    for reference in ["".into(), "x\n".into(), "x".repeat(201)] {
        assert_eq!(
            store.status(&reference).unwrap_err(),
            CredentialError::Invalid
        );
    }
    let error = store.authorization(FAKE).unwrap_err();
    assert!(!serde_json::to_string(&error).unwrap().contains(FAKE));
    assert!(CredentialStore::at_directory(Path::new("relative"), &store.scope).is_err());
    assert!(CredentialStore::at_directory(store.root.parent().unwrap(), "../escape").is_err());
}

#[test]
fn corrupt_oversized_and_rebound_records_fail_closed() {
    let (_dir, store) = fixture();
    let first = create(&store);
    let second = create(&store);
    let path = store.path(&first.reference).unwrap();
    let original = fs::read(&path).unwrap();
    fs::write(store.path(&second.reference).unwrap(), &original).unwrap();
    assert_eq!(
        store.status(&second.reference).unwrap_err(),
        CredentialError::Unavailable
    );
    for bytes in [b"invalid prefix".to_vec(), vec![0; MAX_RECORD_BYTES + 1], {
        let mut bytes = original;
        let last = bytes.last_mut().unwrap();
        *last ^= 0xff;
        bytes
    }] {
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            store.status(&first.reference).unwrap_err(),
            CredentialError::Unavailable
        );
    }
    // A damaged unused record remains removable without decrypting it.
    assert!(store.delete(&first.reference).unwrap().removed);
}

#[test]
fn separate_configuration_scope_cannot_resolve_a_reference() {
    let (dir, store) = fixture();
    let reference = create(&store).reference;
    let other = CredentialStore::at_directory(dir.path(), &"b".repeat(64)).unwrap();
    assert!(!other.status(&reference).unwrap().present);
    other.directory(true).unwrap();
    let mut file = platform::create_private(&other.path(&reference).unwrap(), &other.user).unwrap();
    file.write_all(&fs::read(store.path(&reference).unwrap()).unwrap())
        .unwrap();
    drop(file);
    assert_eq!(
        other.status(&reference).unwrap_err(),
        CredentialError::Unavailable
    );
}

#[cfg(windows)]
#[test]
fn user_dpapi_requires_matching_entropy_and_rejects_unencrypted_values() {
    let encrypted = crypto::protect(FAKE.as_bytes(), b"scope-a").unwrap();
    assert_eq!(
        &*crypto::unprotect(&encrypted, b"scope-a").unwrap(),
        FAKE.as_bytes()
    );
    assert!(crypto::unprotect(&encrypted, b"scope-b").is_err());
    assert!(crypto::unprotect(FAKE.as_bytes(), b"scope-a").is_err());
}

#[cfg(unix)]
#[test]
fn permission_changes_and_links_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (dir, store) = fixture();
    let reference = create(&store).reference;
    let path = store.path(&reference).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        store.status(&reference).unwrap_err(),
        CredentialError::Unavailable
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let linked = dir.path().join("linked");
    fs::hard_link(&path, &linked).unwrap();
    assert_eq!(
        store.status(&reference).unwrap_err(),
        CredentialError::Unavailable
    );
    fs::remove_file(&linked).unwrap();
    fs::rename(&path, &linked).unwrap();
    symlink(&linked, &path).unwrap();
    assert_eq!(
        store.status(&reference).unwrap_err(),
        CredentialError::Unavailable
    );
    assert!(store.delete(&reference).is_err());
    fs::remove_file(&path).unwrap();
    fs::rename(&linked, &path).unwrap();
    fs::set_permissions(&store.root, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        store.status(&reference).unwrap_err(),
        CredentialError::Unavailable
    );
    fs::set_permissions(&store.root, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(store.status(&reference).unwrap().present);
}

#[cfg(windows)]
#[test]
fn permissive_replacement_files_are_rejected_and_anonymous_reads_are_denied() {
    use windows_sys::Win32::{
        Security::{ImpersonateAnonymousToken, RevertToSelf},
        System::Threading::GetCurrentThread,
    };
    let (dir, store) = fixture();
    let reference = create(&store).reference;
    let path = store.path(&reference).unwrap();
    let protected_path = path.clone();
    std::thread::spawn(move || {
        struct Revert;
        impl Drop for Revert {
            fn drop(&mut self) {
                assert_ne!(unsafe { RevertToSelf() }, 0);
            }
        }
        assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
        let _revert = Revert;
        assert_eq!(
            fs::File::open(protected_path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    })
    .join()
    .unwrap();
    // An ordinary file inherits its parent's ACL; it is not our protected file.
    let replacement = dir.path().join("ordinary-file");
    fs::write(&replacement, fs::read(&path).unwrap()).unwrap();
    fs::remove_file(&path).unwrap();
    fs::rename(&replacement, &path).unwrap();
    assert_eq!(
        store.status(&reference).unwrap_err(),
        CredentialError::Unavailable
    );
    assert!(store.delete(&reference).is_err());
}
