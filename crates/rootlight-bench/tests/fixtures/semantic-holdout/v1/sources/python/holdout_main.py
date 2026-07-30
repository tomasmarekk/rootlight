from holdout_dep_a import FixtureVariantA, fixture_node_01
from holdout_dep_a import fixture_node_02
from holdout_dep_a import fixture_node_03
from holdout_dep_a import fixture_node_04
from holdout_dep_a import fixture_node_05
from holdout_dep_a import fixture_node_06
from holdout_dep_a import fixture_node_07
from holdout_dep_a import fixture_node_08 as fixture_node_08_local
from holdout_dep_b import FixtureVariantB

def execute_fixture(receiver: FixtureVariantA | FixtureVariantB):
    fixture_node_01()
    fixture_node_02()
    fixture_node_03()
    fixture_node_04()
    fixture_node_05()
    fixture_node_06()
    fixture_node_07()
    fixture_node_08_local()
    receiver.fixture_node_09()
    receiver.fixture_node_10()
    fixture_node_11()
    fixture_node_12()
