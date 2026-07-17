/** Kořenová dokumentace Ω. */

import value from "./dep.js";

class Greeter {
  /** Dokumentace metody. */
  greet(name) {
    const text = "Ahoj 🌍";
    console.log(name);
    return text;
  }
}
