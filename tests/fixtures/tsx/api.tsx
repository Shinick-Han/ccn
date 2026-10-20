// Async data loading with error handling.

type Loader = () => Promise<string[]>;

export async function loadNames(fetchAll: Loader): Promise<string[]> {
  try {
    const rows = await fetchAll();
    return rows.filter((row) => row.length > 0);
  } catch (error) {
    return [];
  }
}

export function useLabel(enabled: boolean) {
  return enabled && <strong>Enabled</strong>;
}
