// Basic branching shapes: the base-1 guard and plain conditionals.

function identity(value) {
  return value;
}

function pick(flag) {
  if (flag) {
    return 1;
  }
  return 0;
}

function grade(score) {
  if (score >= 90) {
    return "A";
  } else if (score >= 80) {
    return "B";
  } else {
    return "C";
  }
}
