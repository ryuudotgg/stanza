function validateShipment(shipment: Shipment, registry: Registry) {
  if (!registry.hasEligibleCarrier(shipment.destinationCountry, shipment.deliveryWindow))
    return shipment.withStatus("no eligible carrier for the requested delivery window");
  return shipment;
}
