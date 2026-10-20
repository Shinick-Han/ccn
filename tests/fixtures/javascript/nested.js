// Nested functions: inner decisions belong to the inner function only.

function makeValidator(limit) {
  function check(value) {
    if (value > limit) {
      return false;
    }
    return value >= 0;
  }
  return check;
}

function pipeline(items) {
  const scaled = items.map(function (item) {
    return item * 2;
  });
  return scaled.filter(function (item) {
    return item > 0;
  });
}

const formatter = (value) => {
  return value ? `#${value}` : "#0";
};
