function selectShipment(shipment: Shipment, registry: Registry) {
  if (!registry.hasEligibleCarrier(shipment.destinationCountry, shipment.deliveryWindow))
    return shipment.withStatus("no eligible carrier for the requested delivery window");

  if (!registry.hasAvailableRoute(shipment.destinationCountry, shipment.deliveryWindow))
    return shipment.withStatus("no available route for the requested delivery window");

  return registry.assign(shipment);
}
