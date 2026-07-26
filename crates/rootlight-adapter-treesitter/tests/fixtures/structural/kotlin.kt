/** Greets a café visitor. */
package demo

import kotlin.io.println

class Visitor {
    fun greet(name: String): String {
        println("olá")
        return name
    }
}
