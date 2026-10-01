import { removeItem } from "./cart";

export function Cart({ items }) {
  const count = items.length;

  if (count === 0) return null;

  return (
    <section>
      <button
        type="button"
        onClick={() => {
          const item = items[0];

          if (!item) {
            return;
          }

          removeItem(item.id);
        }}
      >
        Remove first item
      </button>

      <ul>
        {items.map((item) => {
          const quantity = item.quantity;

          if (quantity === 0) return null;

          return (
            <li key={item.id}>
              {item.name}: {quantity}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
