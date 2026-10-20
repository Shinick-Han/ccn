// Switch cases and exception handling.

function route(kind) {
  switch (kind) {
    case "GET":
      return "read";
    case "POST":
      return "write";
    default:
      return "unknown";
  }
}

function parse(raw) {
  try {
    return JSON.parse(raw);
  } catch (err) {
    return null;
  }
}

function safeDivide(a, b) {
  if (b === 0) {
    throw new Error("division by zero");
  }
  return a / b;
}
