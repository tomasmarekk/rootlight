import * as dependency from "./dep-a";
import { VariantA } from "./dep-a";
import { VariantB } from "./dep-b";

export function run(receiver: VariantA | VariantB) {
    dependency.directCall();
    receiver.absentMethod();
    receiver.soleMethod();
    receiver.sharedMethod();
}
