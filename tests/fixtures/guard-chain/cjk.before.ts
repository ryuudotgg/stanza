function chooseRegion(region: Region) {
  if (region.label === "東京都配送地域" && region.deliveryWindow === "午前中指定")
    return region.ok();
  if (region.label === "大阪府配送地域" && region.deliveryWindow === "午前中指定")
    return region.ok();

  return region;
}
