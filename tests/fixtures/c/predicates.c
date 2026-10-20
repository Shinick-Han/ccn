/* Compound predicates (`&&`, `||`) and the conditional operator. */

int can_publish(int approved, int drafted) {
    return approved && drafted;
}

int should_retry(int attempts, int fatal) {
    return attempts < 3 || !fatal;
}

int mixed(int a, int b, int c) {
    return a && b || c;
}

const char *label(int count) {
    return count > 0 ? "some" : "none";
}

int in_range(int value) {
    return value >= 0 && value <= 100;
}
