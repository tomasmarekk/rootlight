/**
 * TypeScript structural-extraction fixture.
 * Exercises typed declarations, calls, imports, Unicode, and strings.
 */
import { logger } from "./logger";

interface Named {
  greet(name: string): string;
}

type Greeting = string;

class Greeter implements Named {
  greet(name: string): string {
    const text: Greeting = "Hello 🌍";
    logger.info(name);
    return `${text}, ${name}`;
  }
}
