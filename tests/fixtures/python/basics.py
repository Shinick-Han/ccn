"""Basic branching shapes: the base-1 guard and plain conditionals."""


def identity(value):
    return value


def pick(flag):
    if flag:
        return 1
    return 0


def grade(score):
    if score >= 90:
        return "A"
    elif score >= 80:
        return "B"
    else:
        return "C"
