"""Loops, exception handling, and a filtered comprehension."""


def total(values):
    result = 0
    for value in values:
        result += value
    return result


def parse_number(raw):
    try:
        return int(raw)
    except ValueError:
        return 0


def positive_squares(values):
    return [value * value for value in values if value > 0]


def countdown(start):
    n = start
    while n > 0:
        n -= 1
    return n
