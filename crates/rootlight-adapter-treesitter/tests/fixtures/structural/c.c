/** Greets a café visitor. */
#include <stdio.h>

struct Visitor {
    int value;
};

int greet(int value) {
    puts("olá");
    return value;
}
