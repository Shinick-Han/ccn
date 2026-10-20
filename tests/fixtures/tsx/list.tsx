// List rendering with a conditional expression and an inline callback.

interface Item {
  id: number;
  label: string;
  done: boolean;
}

export function List({ items }: { items: Item[] }) {
  return (
    <ul>
      {items.map((item) => (
        <li key={item.id}>{item.done ? "done" : "todo"}</li>
      ))}
    </ul>
  );
}

export function Empty({ hidden }: { hidden: boolean }) {
  return hidden ? null : <p>Nothing here.</p>;
}
