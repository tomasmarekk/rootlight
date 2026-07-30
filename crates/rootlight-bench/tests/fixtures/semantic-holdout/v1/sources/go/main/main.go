package main

import (
    "example/holdout/dep_a"
    "example/holdout/dep_b"
)

type FixtureBase interface{}
type FixtureDerived interface { FixtureBase }

type FixtureProtocol interface {
    FixtureBase
    FixtureNode09()
    FixtureNode10()
}

var _ FixtureProtocol = dep_a.FixtureVariantA{}
var _ FixtureProtocol = dep_b.FixtureVariantB{}

func ExecuteFixture(receiver FixtureProtocol) {
    dep_a.FixtureNode01()
    dep_a.FixtureNode02()
    dep_a.FixtureNode03()
    dep_a.FixtureNode04()
    dep_a.FixtureNode05()
    dep_a.FixtureNode06()
    dep_a.FixtureNode07()
    dep_a.FixtureNode08()
    receiver.FixtureNode09()
    receiver.FixtureNode10()
    FixtureNode11()
    FixtureNode12()
}
