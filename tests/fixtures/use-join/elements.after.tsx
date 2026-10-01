export function Rows(name: string, items: Item[]) {
  track(name);
  warm(name);
  const Row = rows(name);
  for (const item of items) mount(<Row item={item} />);
}
