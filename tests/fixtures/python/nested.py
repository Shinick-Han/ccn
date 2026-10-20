"""Nested functions and lambdas are measured as their own units."""


def make_validator(limit):
    def check(value):
        if value > limit:
            return False
        return value >= 0

    return check


def pipeline(values):
    scaled = map(lambda item: item * 2, values)
    return list(filter(lambda item: item > 0, scaled))


def apply(values, transform):
    if not values:
        return []
    return [transform(value) for value in values]
