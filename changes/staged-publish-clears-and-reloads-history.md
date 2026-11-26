---
kind: fixed
---
- **After a staged publish the card starts again and the history lists the change** (ADR-0246
  decision 3). On **Settings history**, once **Publish** writes the staged values, each staged
  setting reads **Unchanged** again instead of the value just published, and the history list
  below reloads, so the publish shows there without reloading the page.
