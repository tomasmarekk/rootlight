package demo;

import java.util.List;

/** Kořenová dokumentace Ω. */
@interface Marker {
    /** Dokumentace elementu. */
    String value();
}

class Greeter {
    String greet(String name) {
        String text = "Ahoj 🌍";
        return name + text;
    }
}
