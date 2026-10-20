/* `for` and `while` loops: each loop is one decision. */

int sum(const int *values, int n) {
    int total = 0;
    for (int i = 0; i < n; i++) {
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

int total(const int *values, int n) {
    int result = 0;
    for (int i = 0; i < n; i++) {
        result += values[i];
    }
    return result;
}
