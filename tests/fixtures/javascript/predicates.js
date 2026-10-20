// Compound predicates and conditional expressions.

function canPublish(user, post) {
  return user.isAdmin && post.isApproved;
}

function shouldRetry(attempts, fatal) {
  return attempts < 3 || fatal;
}

function mixed(a, b, c) {
  if (a && (b || c)) {
    return "yes";
  }
  return "no";
}

function label(count) {
  return count > 1 ? "many" : "one";
}

function discount(kind, total) {
  return kind === "vip" ? total * 0.8 : total;
}
