// Nested components: decisions stay with the inner function.

export function Panel({ open }: { open: boolean }) {
  function Body() {
    if (!open) {
      return null;
    }
    return <section>Body</section>;
  }
  return <div>{open ? <Body /> : null}</div>;
}

export function Toolbar() {
  const handleClick = () => {
    if (window.confirm("Sure?")) {
      console.log("clicked");
    }
  };
  return <button onClick={handleClick}>Go</button>;
}
