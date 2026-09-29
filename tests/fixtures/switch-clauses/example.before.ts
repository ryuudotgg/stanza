function label(status: Status) {
  switch (status) {
    case "draft":
    case "open": return "Open";
    case "closed": return "Closed";
    default: return "Unknown";
  }
}
