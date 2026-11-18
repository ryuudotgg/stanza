export function total(orders: Order[]) {
  const sum = orders.reduce(
    (acc, order) => acc + order.price,
    0,
  );
  return sum;
}
