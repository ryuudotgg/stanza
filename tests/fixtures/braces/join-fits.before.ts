function settle(order: Order, queue: Order[]) {
  if (order.cancelled) {
    return null;
  }

  if (order.paid) {
    ship(order);
  } else {
    remind(order.customer);
  }

  for (const item of order.items) {
    reserve(item);
  }

  if (order.held) /* until the audit */ {
    return order;
  }

  while (queue.length > 0) {
    queue.shift();
  }

  if (order.total > 0) {
    return order.withStatus("settled after the review window closed on refunds");
  }

  return order;
}
