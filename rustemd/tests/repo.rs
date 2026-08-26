use rustemd::repo::{Error, Repo, UnitDefinition};

fn definition(name: &str, description: &str) -> UnitDefinition {
    UnitDefinition::parse(
        name,
        &format!("[Unit]\nDescription={description}\n\n[Service]\nExecStart=/bin/true\n"),
    )
    .unwrap()
}
fn temp_repo() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::open(dir.path().to_path_buf()).unwrap();
    (dir, repo)
}

#[test]
fn typed_create_update_write_read_and_delete() {
    let (_dir, repo) = temp_repo();
    let first = definition("hello.service", "first");
    repo.create(&first).unwrap();
    assert_eq!(repo.read("hello.service").unwrap(), Some(first.clone()));
    assert!(matches!(repo.create(&first), Err(Error::AlreadyExists(_))));
    let second = definition("hello.service", "second");
    repo.update(&second).unwrap();
    assert_eq!(repo.read("hello.service").unwrap(), Some(second.clone()));
    assert!(matches!(
        repo.update(&definition("missing.service", "missing")),
        Err(Error::NotFound(_))
    ));
    repo.write(&first).unwrap();
    assert_eq!(repo.read("hello.service").unwrap(), Some(first));
    repo.delete("hello.service").unwrap();
    repo.delete("hello.service").unwrap();
    assert_eq!(repo.read("hello.service").unwrap(), None);
}
#[test]
fn list_parses_definitions_and_applies_root_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let primary = dir.path().join("primary");
    let secondary = dir.path().join("secondary");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&secondary).unwrap();
    std::fs::write(
        primary.join("same.service"),
        "[Unit]\nDescription=primary\n",
    )
    .unwrap();
    std::fs::write(
        secondary.join("same.service"),
        "[Unit]\nDescription=secondary\n",
    )
    .unwrap();
    std::fs::write(
        secondary.join("other.service"),
        "[Unit]\nDescription=other\n",
    )
    .unwrap();
    let repo = Repo::open_roots(vec![primary, secondary]).unwrap();
    let listed = repo.list().unwrap();
    assert_eq!(
        listed.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        vec!["other.service", "same.service"]
    );
    assert_eq!(
        repo.read("same.service")
            .unwrap()
            .unwrap()
            .document
            .sections[0]
            .entries[0]
            .value,
        "primary"
    );
}
#[test]
fn mutations_are_typed_and_serialized() {
    let (_dir, repo) = temp_repo();
    repo.create(&definition("counter.service", "zero")).unwrap();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let repo = &repo;
            scope.spawn(move || {
                for _ in 0..25 {
                    repo.mutate("counter.service", |current| {
                        let mut definition = current.unwrap();
                        definition.document.sections[0].entries[0].value.push('!');
                        Some(definition)
                    })
                    .unwrap();
                }
            });
        }
    });
    assert_eq!(
        repo.read("counter.service")
            .unwrap()
            .unwrap()
            .document
            .sections[0]
            .entries[0]
            .value
            .len(),
        204
    );
}
#[test]
fn rejects_paths_and_malformed_documents() {
    let (_dir, repo) = temp_repo();
    assert!(matches!(
        repo.read("../evil.service"),
        Err(Error::InvalidName(_))
    ));
    assert!(UnitDefinition::parse("bad.service", "Description=no section\n").is_err());
}
#[test]
fn read_path_returns_a_typed_definition() {
    let (dir, repo) = temp_repo();
    let path = dir.path().join("raw.service");
    std::fs::write(&path, "[Service]\nExecStart=/bin/true\n").unwrap();
    assert_eq!(repo.read_path(&path).unwrap().name, "raw.service");
}
