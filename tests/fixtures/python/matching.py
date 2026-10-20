"""Structural pattern matching: every case arm is a decision."""


def describe(command):
    match command.split():
        case ["go", direction]:
            return f"moving {direction}"
        case ["look"]:
            return "looking"
        case _:
            return "unknown"
