"""Compound boolean predicates and conditional expressions."""


def can_publish(user, post):
    return user.is_admin and post.approved


def should_retry(attempts, fatal):
    return attempts < 3 or fatal


def mixed(a, b, c):
    if a and (b or c):
        return "yes"
    return "no"


def label(count):
    return "many" if count > 1 else "one"
