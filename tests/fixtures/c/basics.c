/* Basic branching shapes: the base-1 guard and plain conditionals. */

int identity(int value) {
    return value;
}

int pick(int flag) {
    if (flag) {
        return 1;
    }
    return 0;
}

const char *grade(int score) {
    if (score >= 90) {
        return "A";
    } else if (score >= 80) {
        return "B";
    }
    return "C";
}
