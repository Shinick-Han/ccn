// TSX rendering: JSX conditionals are plain JS operators.

interface Props {
  title: string;
  visible: boolean;
  count: number;
}

export function Title(props: Props) {
  return <h1>{props.title}</h1>;
}

export function Banner(props: Props) {
  return (
    <div>
      {props.visible && <span>New</span>}
    </div>
  );
}

export function Badge(props: Props) {
  if (props.count > 0) {
    return <em>{props.count}</em>;
  }
  return null;
}
