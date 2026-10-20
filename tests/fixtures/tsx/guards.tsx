// Loop and predicate shapes in component helpers.

export function firstMatch(values: string[], query: string): string | null {
  for (const value of values) {
    if (value === query || value.startsWith(query)) {
      return value;
    }
  }
  return null;
}

export function toggle(flags: Record<string, boolean>, key: string) {
  const next = { ...flags };
  next[key] = !(flags[key] ?? false);
  return next;
}
