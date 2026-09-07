# Structural Ruby fixture with explicit source declarations.
# Dynamic calls remain evidence, not executed module or method resolution.
require "support"
module Garden
  class Greeter
    PREFIX = "Hello 🌍"
    def greet(name, suffix: "!")
      text = PREFIX
      puts(name)
      text
    end
    def self.build(name)
      new(name)
    end
  end
end
