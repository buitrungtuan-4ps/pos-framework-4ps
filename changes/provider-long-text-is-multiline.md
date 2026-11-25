---
kind: fixed
---
- **A delivery connection's item map keeps one mapping per line.** The connection form drew every
  text setting as a one-line input, which drops line breaks, so an item map of two or more
  `code=menu item id` lines was saved as one line. The cloud then refused every order from that
  vendor with "the connection's item map is malformed".
  - A text setting whose schema allows more than 256 characters, such as the demo delivery
    vendor's item map, is now a multi-line box that keeps its line breaks. Shorter text settings
    are unchanged.
  - A connection saved before this keeps its one-line map: open it, put each mapping on its own
    line, and save it again.
