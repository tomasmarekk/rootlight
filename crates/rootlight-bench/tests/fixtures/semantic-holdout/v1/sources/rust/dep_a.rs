pub fn fixture_node_01() {}
pub fn fixture_node_02() {}
pub fn fixture_node_03() {}
pub fn fixture_node_04() {}
pub fn fixture_node_05() {}
pub fn fixture_node_06() {}
pub fn fixture_node_07() {}
pub fn fixture_node_08() {}

pub trait FixtureProtocol {
    fn fixture_node_09(&self);
    fn fixture_node_10(&self);
}

pub struct FixtureVariantA;

impl FixtureProtocol for FixtureVariantA {
    fn fixture_node_09(&self) {}
    fn fixture_node_10(&self) {}
}
