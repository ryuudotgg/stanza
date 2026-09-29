function summary(order: Order) {
  const total = sum(order.lines);

  return {
    id: order.id,
    total,
  };
}
