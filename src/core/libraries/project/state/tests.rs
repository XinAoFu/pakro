use super::*;
use crate::packages::python_environment::model::ModelLimits;
use uuid::Uuid;

#[test]
fn p3_full_state_detects_registration_other_groups_and_new_target_tombstones() {
    let identity = ProjectIdentity::from_uuid(Uuid::from_u128(1));
    let mut model = CatalogModel::new(ModelLimits::default()).unwrap();
    let absent = ExpectedProjectState::from_model(&model, identity);
    model
        .register_project(identity.as_uuid(), "/project-a", &"a".repeat(64))
        .unwrap();
    assert!(absent.ensure_current(&model).is_err());
    let registered = ExpectedProjectState::from_model(&model, identity);
    let auxiliary =
        SelectionKey::new(identity.as_uuid(), "auxiliary", [8; 32], model.limits()).unwrap();
    model
        .apply_selection(
            &auxiliary,
            &model.snapshot(&auxiliary),
            None,
            Uuid::from_u128(10),
        )
        .unwrap();
    assert!(registered.ensure_current(&model).is_err());
    let first_target = ExpectedProjectState::from_model(&model, identity);
    let second = SelectionKey::new(identity.as_uuid(), "default", [9; 32], model.limits()).unwrap();
    model
        .apply_selection(&second, &model.snapshot(&second), None, Uuid::from_u128(11))
        .unwrap();
    assert!(first_target.ensure_current(&model).is_err());
    let before_forget = ExpectedProjectState::from_model(&model, identity);
    model.unregister_project(identity.as_uuid()).unwrap();
    assert!(before_forget.ensure_current(&model).is_err());
    assert!(
        absent.ensure_current(&model).is_err(),
        "cleared target slots must defeat absence ABA"
    );
    assert!(
        ExpectedProjectState::from_model(&model, identity)
            .ensure_identity(&ProjectIdentity::from_uuid(Uuid::from_u128(2)))
            .is_err()
    );
}

#[test]
fn p3_unrelated_project_updates_do_not_stale_the_observation() {
    let mut model = CatalogModel::new(ModelLimits::default()).unwrap();
    let identity = ProjectIdentity::from_uuid(Uuid::from_u128(1));
    let expected = ExpectedProjectState::from_model(&model, identity);
    model
        .register_project(Uuid::from_u128(2), "/project-b", &"b".repeat(64))
        .unwrap();
    let key = SelectionKey::new(Uuid::from_u128(2), "default", [1; 32], model.limits()).unwrap();
    model
        .apply_language_selection(
            &key,
            &model.snapshot(&key),
            &pakro_store::Digest::Sha256([5; 32]).to_string(),
            Uuid::from_u128(12),
        )
        .unwrap();
    expected.ensure_current(&model).unwrap();
}
