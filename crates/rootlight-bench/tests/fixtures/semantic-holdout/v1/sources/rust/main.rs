mod dep_a;
mod dep_b;

use crate::dep_a::fixture_node_01;
use crate::dep_a::fixture_node_02;
use crate::dep_a::fixture_node_03;
use crate::dep_a::fixture_node_04;
use crate::dep_a::fixture_node_05;
use crate::dep_a::fixture_node_06;
use crate::dep_a::fixture_node_07;
use crate::dep_a::fixture_node_08 as fixture_node_08_local;
use crate::dep_a::FixtureProtocol;

pub fn execute_fixture(receiver: &dyn FixtureProtocol) {
    fixture_node_01();
    fixture_node_02();
    fixture_node_03();
    fixture_node_04();
    fixture_node_05();
    fixture_node_06();
    fixture_node_07();
    fixture_node_08_local();
    receiver.fixture_node_09();
    receiver.fixture_node_10();
    fixture_node_11();
    fixture_node_12();
}
