#include "Overloads.hpp"

void process(int x) {
    (void)x;
}

void process(double x) {
    (void)x;
}

int dispatchOverload(int x) {
    process(x);
    return 0;
}
