function settle(order: Order) {
  if (order.total > 0)
    return order.withStatus("settled after the review window closed for refunds");

  if (order.paid) ship(order);
  else
    notify(order.customer, "held until the review window closes for each payment and its refund");

  for (const item of order.items)
    reserve(item, "settled after the review window closed on refunds");

  return order;
}
