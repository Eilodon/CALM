#include <iostream>
#include "Shape.hpp"

int main() {
    Circle c(2.0);
    Shape &s = c;
    // Virtual dispatch call site — resolves at runtime to Circle::area().
    std::cout << s.area() << std::endl;
    return 0;
}
