#pragma once

class Shape {
public:
    virtual ~Shape() = default;
    virtual double area() const = 0;
};

class Circle : public Shape {
public:
    explicit Circle(double r) : radius(r) {}
    double area() const override;

private:
    double radius;
};
