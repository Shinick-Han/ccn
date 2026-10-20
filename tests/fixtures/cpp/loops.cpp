// Classic `for`, `while`, and range-based `for`.
#include <vector>

int sum(const std::vector<int> &values) {
    int total = 0;
    for (int i = 0; i < static_cast<int>(values.size()); i++) {
        total += values[i];
    }
    return total;
}

int countdown(int start) {
    int steps = 0;
    while (start > 0) {
        start--;
        steps++;
    }
    return steps;
}

int total(const std::vector<int> &values) {
    int result = 0;
    for (int value : values) {
        result += value;
    }
    return result;
}
