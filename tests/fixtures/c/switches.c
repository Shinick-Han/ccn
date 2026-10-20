/* A two-case switch (no default, so case and section counts agree),
 * plus a loop/guard pair. */

const char *route(const char *method) {
    switch (method[0]) {
        case 'G':
            return "read";
        case 'P':
            return "write";
    }
    return "unknown";
}

int first_positive(const int *values, int n) {
    for (int i = 0; i < n; i++) {
        if (values[i] > 0) {
            return values[i];
        }
    }
    return 0;
}
