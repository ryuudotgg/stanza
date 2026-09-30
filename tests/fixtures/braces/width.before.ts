function chooseShipment(shipment: Shipment, registry: Registry) {
  if (shipment.expedited) {
    return registry.assign(shipment);
  }

  if (!registry.hasEligibleCarrier(shipment.destinationCountry, shipment.deliveryWindow)) {
    return shipment.withStatus("no eligible carrier for the requested delivery window");
  }

  return shipment;
}
