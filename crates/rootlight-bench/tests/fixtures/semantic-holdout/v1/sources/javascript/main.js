import { FixtureVariantA, fixtureNode01 } from "./dep-a";
import { fixtureNode02 } from "./dep-a";
import { fixtureNode03 } from "./dep-a";
import { fixtureNode04 } from "./dep-a";
import { fixtureNode05 } from "./dep-a";
import { fixtureNode06 } from "./dep-a";
import { fixtureNode07 } from "./dep-a";
import { fixtureNode08 as fixtureNode08Local } from "./dep-a";
import { FixtureVariantB } from "./dep-b";

/** @param {FixtureVariantA | FixtureVariantB} receiver */
export function executeFixture(receiver) {
    fixtureNode01();
    fixtureNode02();
    fixtureNode03();
    fixtureNode04();
    fixtureNode05();
    fixtureNode06();
    fixtureNode07();
    fixtureNode08Local();
    receiver.fixtureNode09();
    receiver.fixtureNode10();
    fixtureNode11();
    fixtureNode12();
}
