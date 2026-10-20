// A two-case switch (no default, so case and section counts agree),
// plus try/catch.
#include <exception>

const char *route(char method) {
    switch (method) {
        case 'G':
            return "read";
        case 'P':
            return "write";
    }
    return "unknown";
}

int safe_divide(int a, int b) {
    try {
        return a / b;
    } catch (const std::exception &e) {
        return 0;
    }
}
