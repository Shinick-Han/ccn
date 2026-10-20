// Compound predicates (`&&`, `||`) and the conditional operator.

bool can_publish(bool approved, bool drafted) {
    return approved && drafted;
}

bool should_retry(int attempts, bool fatal) {
    return attempts < 3 || !fatal;
}

bool mixed(bool a, bool b, bool c) {
    return a && b || c;
}

const char *label(int count) {
    return count > 0 ? "some" : "none";
}

bool in_range(int value) {
    return value >= 0 && value <= 100;
}
