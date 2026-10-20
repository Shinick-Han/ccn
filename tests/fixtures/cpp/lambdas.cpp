// Lambdas are their own scope: an `if` inside one belongs to the lambda,
// never to the function that creates it.
#include <functional>
#include <vector>

std::function<int(int)> absolute() {
    return [](int value) {
        if (value < 0) {
            return -value;
        }
        return value;
    };
}

int count_above(const std::vector<int> &values, int limit) {
    auto bump = [](int value) { return value + 1; };
    int count = 0;
    for (int value : values) {
        if (value > limit) {
            count += bump(value);
        }
    }
    return count;
}
