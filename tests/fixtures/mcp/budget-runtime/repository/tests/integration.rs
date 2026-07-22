use rootlight_budget_runtime_fixture::budget_entry;

#[test]
fn public_entry_is_stable() {
    assert_eq!(budget_entry(3), 10);
}
