use rustemd::repo::{Repo, UnitDefinition, UnitEntry, UnitSection};

#[test]
fn typed_crud_round_trip_preserves_the_structured_unit_definition() {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repo::open(directory.path().to_path_buf()).unwrap();
    let definition = UnitDefinition::parse(
        "backup.service",
        "[Unit]\nDescription=Nightly backup\n\n[Service]\nExecStart=/bin/backup --nightly\n",
    )
    .unwrap();

    repo.create(&definition).unwrap();

    let loaded = repo.read("backup.service").unwrap().unwrap();
    assert_eq!(loaded, definition);
    assert_eq!(loaded.document.sections.len(), 2);
    assert_eq!(
        loaded.document.sections[0],
        UnitSection {
            name: "Unit".into(),
            entries: vec![UnitEntry {
                key: "Description".into(),
                value: "Nightly backup".into()
            }],
        }
    );
    assert_eq!(
        loaded.to_text(),
        "[Unit]\nDescription=Nightly backup\n\n[Service]\nExecStart=/bin/backup --nightly\n"
    );
}
