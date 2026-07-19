package demo;

import java.util.List;

/** Root-level documentation Ω. */
@interface Marker {
    /** Element documentation. */
    String value();
}

class Greeter {
    String greet(String name) {
        String text = "Hello 🌍";
        return name + text;
    }
}
