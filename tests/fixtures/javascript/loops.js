// Loops and an inline callback; each loop adds one decision.

function sum(values) {
  let total = 0;
  for (const value of values) {
    total += value;
  }
  return total;
}

function countdown(start) {
  let n = start;
  while (n > 0) {
    n -= 1;
  }
  return n;
}

function register(handlers) {
  handlers.forEach(function (handler) {
    handler();
  });
  return handlers.length;
}
